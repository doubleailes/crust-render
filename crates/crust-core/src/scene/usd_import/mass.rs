//! The curve mass field's import side ([`crate::mass`]): the `crust:mass:*`
//! attributes a curves prim opts in with, the material's
//! `outputs:crust:mass:color` terminal compiled into a colour graph, the
//! strands turned into the pieces a bake splats, and the collector that
//! groups prims into fields — one per prim, or one per `crust:mass:group`
//! token under the same placement — and bakes every field in parallel once
//! the USD read is over.
//!
//! Every warning here is raised on the importing thread: the attributes and
//! the terminal are read during the traversal, the group conflicts when a
//! prim joins a field, and the unplaceable placements when the baked fields
//! are attached ([`crate::WorldBuilder::attach_mass_fields`]). The bake
//! itself, on rayon's threads, warns about nothing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{Mat4 as GMat4, Vec3A};
use openusd::sdf;
use openusd::usd::{Prim, Stage};
use rayon::prelude::*;
use tracing::debug;

use crate::color::Space;
use crate::mass::{MassColor, MassField, MassPiece, MassSettings, MassSource, bake};
use crate::material::{CurveNeeds, Material};
use crate::strands::StrandTable;
use crate::warning;

use super::attrs::{custom_f32, custom_i32, custom_token};
use super::shapes::CurveBatches;
use super::strands::StrandRequest;
use super::{ImportCaches, prim_at};

/// The namespace every mass attribute lives in.
const NAMESPACE: &str = "crust:mass";

/// What a curves prim that opts in asks for, read off its attributes and
/// its material: the settings in the curves' own units, the group it joins,
/// the colour graph to bake (when its material has one), and whether the
/// material reads the field at all.
pub(super) struct MassRequest {
    pub(super) settings: MassSettings,
    pub(super) group: Option<String>,
    pub(super) color: Option<Arc<MassColor>>,
    /// Whether the bound material reads any `crust_mass_*` lookup: only then
    /// does the geometry carry the field's lookup (the field is baked, and
    /// reported, either way).
    pub(super) reads: bool,
    pub(super) working: Space,
}

impl MassRequest {
    /// The bake's own strand request: the colour graph's curve properties
    /// and primvars, which the bake table decodes beside the material's.
    pub(super) fn bake_request(&self) -> Option<StrandRequest<'_>> {
        self.color.as_ref().map(|c| StrandRequest {
            needs: CurveNeeds {
                chart: false,
                strand: true,
                primvars: c.primvars(),
                mass: false,
            },
            chart: None,
            working: self.working,
            mass: false,
        })
    }
}

/// Whether `prim` authors any `crust:mass:*` attribute.
pub(super) fn authored(prim: &Prim) -> bool {
    prim.attributes_in_namespace(NAMESPACE)
        .is_ok_and(|attrs| !attrs.is_empty())
}

/// Reports `crust:mass:*` on a prim that is not curves: nothing is baked.
pub(super) fn warn_not_curves(prim: &Prim) {
    if authored(prim) {
        warning!(
            MassNotCurves,
            at = prim.path(),
            "{}: crust:mass:* is read on BasisCurves and NurbsCurves only — no mass field is \
             baked for this {}",
            prim.path(),
            prim.type_name()
                .ok()
                .flatten()
                .map_or_else(|| "prim".to_owned(), |t| t.as_str().to_owned())
        );
    }
}

/// Reads the mass attributes of a curves prim bound to `material`: `Some`
/// when it opts in with a positive, finite `crust:mass:radius`. Out-of-domain
/// values are refused with one `mass.invalid_setting` each: a bad radius
/// bakes nothing, a bad threshold or cap takes its default.
pub(super) fn request(
    prim: &Prim,
    material: &dyn Material,
    color: Option<Arc<MassColor>>,
    working: Space,
) -> Option<MassRequest> {
    let refuse = |name: &str, what: String| {
        warning!(
            MassInvalidSetting,
            at = prim.path(),
            "{}: crust:mass:{name} {what}",
            prim.path(),
        );
    };
    let radius = custom_f32(prim, "crust:mass:radius")?;
    if !(radius.is_finite() && radius >= 0.0) {
        refuse(
            "radius",
            format!("is {radius}; it must be finite and not negative — no mass field is baked"),
        );
        return None;
    }
    if radius == 0.0 {
        return None;
    }
    let mut settings = MassSettings::new(radius);
    if let Some(t) = custom_f32(prim, "crust:mass:threshold") {
        if t.is_finite() && t > 0.0 {
            settings.threshold = t;
        } else {
            refuse(
                "threshold",
                format!(
                    "is {t}; it must be finite and positive — the default {} is used",
                    MassSettings::DEFAULT_THRESHOLD
                ),
            );
        }
    }
    if let Some(cap) = custom_i32(prim, "crust:mass:maxResolution") {
        if cap >= 1 {
            settings.max_resolution = cap as u32;
        } else {
            refuse(
                "maxResolution",
                format!(
                    "is {cap}; it must be at least 1 — the default {} is used",
                    MassSettings::DEFAULT_MAX_RESOLUTION
                ),
            );
        }
    }
    let group = custom_token(prim, "crust:mass:group").filter(|g| !g.is_empty());
    Some(MassRequest {
        settings,
        group,
        color,
        reads: material.curve_needs().mass,
        working,
    })
}

/// A curves prim's contribution, as the span loops leave it: its pieces in
/// the curves' local space, with the settings and group it was read with.
pub(super) struct CurvesMass {
    pub(super) source: MassSource,
    pub(super) settings: MassSettings,
    pub(super) group: Option<String>,
}

impl CurvesMass {
    /// The pieces of `batches` — the kernel's own primitives, so the piece's
    /// span is the `prim_id` a strand table indexes — with the colour graph
    /// and the bake table it reads.
    pub(super) fn new(
        batches: &CurveBatches,
        request: &MassRequest,
        bake_table: Option<StrandTable>,
    ) -> CurvesMass {
        CurvesMass {
            source: MassSource {
                pieces: pieces(batches, request.settings.radius),
                color: request.color.clone().zip(bake_table).map(|(c, t)| (c, t)),
            },
            settings: request.settings,
            group: request.group.clone(),
        }
    }

    /// The same contribution carried into another space by `xf`: the points
    /// through it, the radii and the settings' radius by the cube root of
    /// its determinant (exact for a uniform scale; under a non-uniform one
    /// the isotropic stand-in the texture footprint already uses).
    pub(super) fn transformed(mut self, xf: &GMat4) -> CurvesMass {
        let scale = xf.determinant().abs().cbrt();
        let m = glam::Affine3A::from_mat4(*xf);
        for p in &mut self.source.pieces {
            p.a = m.transform_point3a(p.a);
            p.b = m.transform_point3a(p.b);
            p.ra *= scale;
            p.rb *= scale;
        }
        self.settings = self.settings.scaled(scale);
        self
    }
}

/// How many straight pieces a cubic span is cut into at most.
const MAX_PIECES_PER_SPAN: usize = 32;

/// Every batch's primitives as pieces: a linear segment as it is, a cubic
/// span cut into straight pieces about half a radius long (the voxel), each
/// knowing which span and which part of its parameter it is.
fn pieces(batches: &CurveBatches, radius: f32) -> Vec<MassPiece> {
    let (segments, cubic) = batches;
    let mut out = Vec::with_capacity(segments.len() + cubic.len());
    for (i, s) in segments.iter().enumerate() {
        out.push(MassPiece {
            a: s.p0,
            b: s.p1,
            ra: s.r0,
            rb: s.r1,
            span: i as u32,
            u0: 0.0,
            u1: 1.0,
        });
    }
    let voxel = (radius / 2.0).max(1e-6);
    for (i, c) in cubic.iter().enumerate() {
        let polygon: f32 = c.cp.windows(2).map(|w| (w[1] - w[0]).length()).sum();
        let n = ((polygon / voxel).ceil() as usize).clamp(1, MAX_PIECES_PER_SPAN);
        let at = |u: f32| -> Vec3A {
            let s = 1.0 - u;
            c.cp[0] * (s * s * s)
                + c.cp[1] * (3.0 * u * s * s)
                + c.cp[2] * (3.0 * u * u * s)
                + c.cp[3] * (u * u * u)
        };
        for k in 0..n {
            let (u0, u1) = (k as f32 / n as f32, (k + 1) as f32 / n as f32);
            out.push(MassPiece {
                a: at(u0),
                b: at(u1),
                ra: c.r0 + (c.r1 - c.r0) * u0,
                rb: c.r0 + (c.r1 - c.r0) * u1,
                span: i as u32,
                u0,
                u1,
            });
        }
    }
    out
}

/// A placement, as a field's key: the prims that share a group under one
/// of these bake together.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Placement {
    /// The top-level scene, under one world transform (its matrix's bits):
    /// a groom split over sibling prims shares one, two characters' grooms
    /// do not.
    World([u32; 16]),
    /// A prototype, in its own space: the stage epoch, the prototype path
    /// and the part's frame in it.
    Prototype(u32, String, [u32; 16]),
}

impl Placement {
    pub(super) fn world(xf: &GMat4) -> Placement {
        Placement::World(bits(xf))
    }

    pub(super) fn prototype(epoch: u32, proto: &sdf::Path, local: &GMat4) -> Placement {
        Placement::Prototype(epoch, proto.to_string(), bits(local))
    }
}

fn bits(m: &GMat4) -> [u32; 16] {
    m.to_cols_array().map(f32::to_bits)
}

/// A field not yet baked: the prims joining it, as the traversal met them,
/// each with the settings it authored and its contribution. The group's
/// settings are decided when the field bakes, once every prim is in: the
/// first prim's in path order, which is not the traversal's order.
struct Pending {
    group: Option<String>,
    prims: Vec<(String, MassSettings, MassSource)>,
}

/// Groups opted-in prims into fields as the traversal meets them, and bakes
/// them all once it is over.
#[derive(Default)]
pub(super) struct MassCollector {
    fields: Vec<Pending>,
    /// `(placement, group)` → the field the group bakes into.
    groups: HashMap<(Placement, String), usize>,
}

impl MassCollector {
    /// Adds `prim`'s contribution under `placement`: to the field of its
    /// group there, or to one of its own. Returns the field's index.
    pub(super) fn add(&mut self, placement: Placement, prim: &str, mass: CurvesMass) -> usize {
        let CurvesMass {
            source,
            settings,
            group,
        } = mass;
        let index = match group {
            Some(group) => {
                let key = (placement, group);
                match self.groups.get(&key) {
                    Some(&i) => i,
                    None => {
                        let i = self.push(Some(key.1.clone()));
                        self.groups.insert(key, i);
                        i
                    }
                }
            }
            None => self.push(None),
        };
        self.fields[index]
            .prims
            .push((prim.to_owned(), settings, source));
        index
    }

    fn push(&mut self, group: Option<String>) -> usize {
        self.fields.push(Pending {
            group,
            prims: Vec::new(),
        });
        self.fields.len() - 1
    }

    pub(super) fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Bakes every field, in parallel, in index order — the order
    /// [`MassCollector::add`] handed out. Each group's settings are its
    /// first prim's in path order; a prim authoring others is named here,
    /// on the importing thread, by one `mass.group_conflict`. The strands
    /// were read at the frame's time code during the traversal; `time` only
    /// names it in the log, since the bake reads no attribute.
    pub(super) fn bake_all(&mut self, time: Option<f64>) -> (Vec<Arc<MassField>>, Duration) {
        let started = Instant::now();
        let pending = std::mem::take(&mut self.fields);
        self.groups.clear();
        let mut jobs: Vec<(MassSettings, Vec<String>, Vec<MassSource>)> = Vec::new();
        for mut p in pending {
            p.prims.sort_by(|a, b| a.0.cmp(&b.0));
            let settings = p.prims[0].1;
            for (prim, s, _) in &p.prims[1..] {
                if *s != settings {
                    warning!(
                        MassGroupConflict,
                        at = prim,
                        "{prim}: crust:mass:group \"{}\" bakes with {}'s settings (radius {}, \
                         threshold {}, maxResolution {}); this prim's (radius {}, threshold {}, \
                         maxResolution {}) are not used",
                        p.group.as_deref().unwrap_or(""),
                        p.prims[0].0,
                        settings.radius,
                        settings.threshold,
                        settings.max_resolution,
                        s.radius,
                        s.threshold,
                        s.max_resolution,
                    );
                }
            }
            let (prims, sources): (Vec<String>, Vec<MassSource>) =
                p.prims.into_iter().map(|(p, _, s)| (p, s)).unzip();
            jobs.push((settings, prims, sources));
        }
        let fields: Vec<Arc<MassField>> = jobs
            .into_par_iter()
            .map(|(settings, prims, sources)| Arc::new(bake(&sources, settings, prims)))
            .collect();
        let elapsed = started.elapsed();
        for f in &fields {
            debug!(
                "Mass field {:?}: {} bricks, {} bytes, radius {}, threshold {}, voxel {}{}, \
                 colour {}, surface {}, baked in {:?}{}",
                f.prims(),
                f.bricks(),
                f.bytes(),
                f.settings().radius,
                f.settings().threshold,
                f.voxel(),
                if f.capped() { " (capped)" } else { "" },
                f.has_color(),
                f.has_surface(),
                f.bake_time(),
                match time {
                    Some(t) => format!(" at time {t}"),
                    None => String::new(),
                }
            );
        }
        (fields, elapsed)
    }
}

/// The colour graph a material's `outputs:crust:mass:color` terminal names,
/// compiled: `None` without a terminal, or with one that cannot be baked
/// (reported with `mass.color_unsupported`). A node of the graph that needs
/// a view or a hit is reported the same way and reads its default.
pub(super) fn color_terminal(
    stage: &Stage,
    mat_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
) -> Option<Arc<MassColor>> {
    let mat = prim_at(stage, mat_path.clone());
    let attr = mat.attribute("outputs:crust:mass:color");
    let targets = attr.connections().ok()?;
    let target = targets.first()?;
    let unsupported = |what: String| {
        warning!(
            MassColorUnsupported,
            at = mat_path,
            "Material {mat_path}: outputs:crust:mass:color {what} — no mass colour is baked for \
             the curves it is bound to",
        );
    };
    let Some((prim_path, prop)) = target.split_property() else {
        unsupported(format!("connects to {target}, which is not a property"));
        return None;
    };
    let shader = prim_at(stage, prim_path);
    let is_mtlx = openusd_schemas::shade::Shader::get(stage, shader.path().clone())
        .ok()
        .flatten()
        .and_then(|s| super::materials::shader_info_id(&s))
        .is_some_and(|id| super::mtlx_network::is_mtlx_id(&id));
    let Some(output) = prop.strip_prefix("outputs:").filter(|_| is_mtlx) else {
        unsupported(format!(
            "connects to {target}, which is not the output of a MaterialX shader"
        ));
        return None;
    };
    let net = super::mtlx_network::translate(stage, caches.stage_path, Some(&shader), None);
    if !net.reported.is_empty() {
        warning!(
            MtlxIncomplete,
            at = mat_path,
            "Material {mat_path}: mass colour network not fully translated — {}",
            net.reported.join("; ")
        );
    }
    let Some(node) = net.surface.clone() else {
        unsupported(format!(
            "connects to {target}, which could not be translated"
        ));
        return None;
    };
    let output = (output != "out").then(|| output.to_owned());
    let compiled = super::materials::with_mtlx_host(
        mat_path,
        std::path::Path::new("/"),
        caches,
        |host, _luma| {
            let mut c = crust_mtlx::Compiler::new(&net.doc, host);
            let root = c.compile_named("", &node, output.as_deref());
            let unsupported: Vec<String> = c.unsupported.iter().cloned().collect();
            (c.program, root, unsupported)
        },
    );
    let (program, root, unsupported_nodes) = compiled;
    if !unsupported_nodes.is_empty() {
        warning!(
            MtlxIncomplete,
            at = mat_path,
            "Material {mat_path}: mass colour graph has no operator for node type(s) {} — \
             those inputs fall back to their defaults",
            unsupported_nodes.join(", ")
        );
    }
    let (program, root) = if crate::config().mtlx_opt {
        let (p, remap) = program.optimize(&[root]);
        (p, remap[root as usize].expect("the root survives"))
    } else {
        (program, root)
    };
    let needs = program.needs_view_or_hit();
    if !needs.is_empty() {
        warning!(
            MassColorUnsupported,
            at = mat_path,
            "Material {mat_path}: the mass colour graph reads {}, which need a view or a \
             surface hit that a bake has not — those nodes read their defaults",
            needs.join(", ")
        );
    }
    debug!(
        "Material {mat_path}: mass colour graph {node} ({} ops, {} primvars)",
        program.ops.len(),
        program.primvars.len()
    );
    Some(Arc::new(MassColor::new(program, root)))
}
