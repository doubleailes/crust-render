//! In-place rebuilds ([`crate::live`]): one material, one light, or the
//! settings and camera, built again from the stage by the functions the
//! traversal built them with, at the import's time code. Never a second
//! implementation: the bit-identity of an in-place update with a fresh
//! import rests on there being one.
//!
//! Each rebuild composes only what it reads: a stage population-masked to
//! the material's or the light's prim with its payloads loaded, as one chunk
//! of the streaming import is, or the payload-free index stage for the
//! settings.

use std::path::Path;
use std::sync::Arc;

use openusd::sdf;
use openusd::usd::{InitialLoadSet, Prim, Stage};

use crate::light::LightList;
use crate::live::{LightUpdate, PrimFrame, Provenance, SettingsUpdate, Structural, Update};
use crate::material::Material;
use crate::rt_world::WorldBuilder;
use crate::{AssetLoader, Warning};

use super::camera::{build_camera, local_to_world};
use super::light_links::LightLinks;
use super::materials::{resolve_displacement, resolve_fresh};
use super::mesh::SubdivPolicy;
use super::time::EvalTimeScope;
use super::xform::compose_with_parent;
use super::{
    ImportCaches, ImportCtx, ValidOptions, apply_exposure, emit_light, open_stage,
    open_stage_masked, prim_at, read_index, stage_builder,
};

/// Warning codes a settings change can move, by family: the settings, the
/// products and their AOVs, the camera, the time code and the colour space.
const SETTINGS_FAMILIES: &[&str] = &[
    "settings.",
    "product.",
    "aov.",
    "lpe.",
    "camera.",
    "time.",
    "color.",
    "subdiv.",
];

pub(crate) fn rebuild(
    prov: &Provenance,
    layer: &Path,
    assets: &dyn AssetLoader,
    warnings: &[Warning],
    change: &crate::live::Change,
) -> Result<Update, Structural> {
    let facts = &prov.stage;
    let path_str = layer
        .to_str()
        .ok_or_else(|| Structural("the layer path is not UTF-8".into()))?;
    let working = facts
        .working
        .ok_or_else(|| Structural("the scene was not imported from a stage".into()))?;
    let _time = EvalTimeScope::enter(facts.options.frame);
    let mut update = Update::default();
    if change.settings {
        update.settings = Some(refresh_settings(prov, layer, path_str, warnings)?);
    }
    let rebuilding = Rebuilding {
        prov,
        layer,
        path_str,
        working,
        assets,
        warnings,
    };
    for path in &change.materials {
        update
            .materials
            .push(rebuilding.material(path, &change.shaders)?);
    }
    for path in &change.lights {
        update.lights.push(rebuilding.light(path)?);
    }
    Ok(update)
}

/// Whether `prim` is `root` or below it.
fn under(prim: &str, root: &str) -> bool {
    prim.strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || rest.starts_with('.'))
}

/// Refuses a rebuild about `root` when the scene's warnings name it, or may
/// name it beyond the prims they keep: a fresh import could report them
/// differently now.
fn unwarned(warnings: &[Warning], root: &str) -> Result<(), Structural> {
    for w in warnings {
        if w.prims.len() >= crate::warnings::MAX_PRIMS || w.prims.iter().any(|p| under(p, root)) {
            return Err(Structural(format!(
                "the scene's {} warning may concern {root}",
                w.code.as_str()
            )));
        }
    }
    Ok(())
}

/// Refuses a rebuild that raised a warning the scene does not already carry
/// for the same prim, or any about `root` or the whole stage.
fn same_warnings(raised: &[Warning], scene: &[Warning], root: &str) -> Result<(), Structural> {
    for w in raised {
        let known = !w.prims.is_empty()
            && w.prims.iter().all(|p| {
                !under(p, root)
                    && scene
                        .iter()
                        .any(|s| s.code == w.code && s.prims.iter().any(|q| q == p))
            });
        if !known {
            return Err(Structural(format!(
                "rebuilding {root} raised {}",
                w.code.as_str()
            )));
        }
    }
    Ok(())
}

fn prim_path(path: &str) -> Result<sdf::Path, Structural> {
    sdf::Path::new(path)
        .ok()
        .filter(|p| p.is_prim())
        .ok_or_else(|| Structural(format!("{path} is not a prim path")))
}

/// Refuses a material whose network reads a prim outside it: a stage masked
/// to the material does not compose it, where the import's stage did.
fn closed_network(stage: &Stage, root: &sdf::Path) -> Result<(), Structural> {
    let mut stack = vec![prim_at(stage, root.clone())];
    while let Some(prim) = stack.pop() {
        for attr in prim.authored_attributes().unwrap_or_default() {
            for target in attr.connections().unwrap_or_default() {
                if !target.prim_path().has_prefix(root) {
                    return Err(Structural(format!(
                        "{} connects to {target}, outside {root}",
                        attr.path()
                    )));
                }
            }
        }
        stack.extend(prim.children().unwrap_or_default());
    }
    Ok(())
}

/// Whether `path` is a `Shader` prim on `stage`.
fn is_shader(stage: &Stage, path: &sdf::Path) -> bool {
    stage
        .prim(path.clone())
        .ok()
        .and_then(|p| p.type_name().ok().flatten())
        .is_some_and(|t| t.as_str() == "Shader")
}

struct Rebuilding<'a> {
    prov: &'a Provenance,
    layer: &'a Path,
    path_str: &'a str,
    working: crate::color::Space,
    assets: &'a dyn AssetLoader,
    warnings: &'a [Warning],
}

impl Rebuilding<'_> {
    fn caches(&self) -> ImportCaches<'_> {
        ImportCaches::new(
            self.assets,
            self.layer,
            SubdivPolicy::new(self.prov.stage.subdiv_level),
            self.working,
        )
    }

    fn masked(&self, path: &sdf::Path) -> Result<Stage, Structural> {
        open_stage(self.layer, self.path_str, Some(path.clone()))
            .map_err(|e| Structural(format!("cannot open the stage: {e}")))
    }

    /// The material at `path`, resolved as the traversal's first binding to
    /// it resolved it, into the slots it fills. Each of `shaders` under it
    /// must be a `Shader` on the material's stage ([`Change::shaders`]).
    ///
    /// [`Change::shaders`]: crate::live::Change::shaders
    fn material(
        &self,
        path: &str,
        shaders: &[String],
    ) -> Result<(Vec<u32>, Arc<dyn Material>), Structural> {
        let origin = self
            .prov
            .material(path)
            .ok_or_else(|| Structural(format!("the scene does not use {path}")))?;
        if origin.displaced {
            return Err(Structural(format!("{path} displaces a mesh")));
        }
        unwarned(self.warnings, path)?;
        let mat_path = prim_path(path)?;
        let scope = crate::WarningScope::enter();
        let built = (|| {
            let stage = self.masked(&mat_path)?;
            closed_network(&stage, &mat_path)?;
            for shader in shaders {
                let shader = prim_path(shader)?;
                if shader.has_prefix(&mat_path) && !is_shader(&stage, &shader) {
                    return Err(Structural(format!("{shader} is not a shader of {path}")));
                }
            }
            let mut caches = self.caches();
            let (material, mtlx) = resolve_fresh(&stage, &mat_path, &mut caches);
            let displaces = crate::config().displace
                && resolve_displacement(&stage, &mat_path, mtlx, &mut caches).is_some();
            if displaces {
                return Err(Structural(format!("{path} now defines a displacement")));
            }
            Ok(material)
        })();
        let raised = scope.finish();
        let material = built?;
        same_warnings(&raised, self.warnings, path)?;
        Ok((origin.slots.clone(), material))
    }

    /// The light at `path`, emitted as the traversal emitted it, its
    /// geometry ids the ones it had.
    fn light(&self, path: &str) -> Result<LightUpdate, Structural> {
        let origin = self
            .prov
            .light(path)
            .ok_or_else(|| Structural(format!("the scene has no light {path}")))?;
        unwarned(self.warnings, path)?;
        let light_path = prim_path(path)?;
        let scope = crate::WarningScope::enter();
        let built = (|| {
            let stage = self.masked(&light_path)?;
            let prim = prim_at(&stage, light_path.clone());
            let world_xf = local_to_world(&stage, &prim);
            let mut world = WorldBuilder::new();
            // Ids are handed out in attach order: claiming the ones before
            // the light's gives its geometry the ids it had.
            let filler: Arc<dyn Material> = super::materials::default_material();
            for _ in 0..origin.geoms.start {
                world.reserve_slot(filler.clone(), crate::ray::RayMask(0));
            }
            let mut ctx = scratch_ctx(self, world);
            if !emit_light(&stage, &mut ctx, &prim, world_xf) {
                return Err(Structural(format!("{path} is no longer a light")));
            }
            let geoms = origin.geoms.start..ctx.world.count() as u32;
            if ctx.lights.count() != 1 || geoms != origin.geoms {
                return Err(Structural(format!("{path} would emit other geometry")));
            }
            let emitters = geoms
                .map(|id| (id, ctx.world.material(id).clone()))
                .collect();
            let (light, _) = ctx.lights.remove(0);
            Ok(LightUpdate {
                index: origin.index,
                light,
                emitters,
            })
        })();
        let raised = scope.finish();
        let light = built?;
        same_warnings(&raised, self.warnings, path)?;
        Ok(light)
    }
}

/// An import context around `world` with nothing in it but the caches a
/// fresh import starts with, for one light's emitter to run in.
fn scratch_ctx<'a>(r: &'a Rebuilding<'a>, world: WorldBuilder) -> ImportCtx<'a> {
    let mut lights = LightList::new();
    lights.set_luma(crate::color::luma(r.working));
    ImportCtx {
        world,
        lights,
        volumes: Vec::new(),
        camera: None,
        wanted_camera: None,
        first_camera: None,
        cameras_seen: Vec::new(),
        caches: r.caches(),
        pending_meshes: Vec::new(),
        links: LightLinks::default(),
        light_origins: Vec::new(),
        settings: crate::tracer::RenderSettings::default(),
        stage_path: r.layer,
        index: None,
        assets: r.assets,
        layers: Default::default(),
    }
}

/// The settings, products and camera re-read as the import reads them.
fn refresh_settings(
    prov: &Provenance,
    layer: &Path,
    path_str: &str,
    warnings: &[Warning],
) -> Result<SettingsUpdate, Structural> {
    let facts = &prov.stage;
    if let Some(w) = warnings.iter().find(|w| {
        SETTINGS_FAMILIES
            .iter()
            .any(|f| w.code.as_str().starts_with(f))
    }) {
        return Err(Structural(format!(
            "the scene's {} warning may change with the settings",
            w.code.as_str()
        )));
    }
    let Some(camera) = facts.camera.as_deref() else {
        return Err(Structural("the scene renders the procedural camera".into()));
    };
    let cam_path = prim_path(camera)?;
    let options = ValidOptions::check(&facts.options)
        .map_err(|e| Structural(format!("the import options: {e}")))?;
    let scope = crate::WarningScope::enter();
    let built = (|| {
        let index = stage_builder()
            .load(InitialLoadSet::LoadNone)
            .open(path_str)
            .map_err(|e| Structural(format!("cannot open the stage: {e}")))?;
        let read = read_index(&index, layer, path_str, &facts.options, &options)
            .map_err(|e| Structural(format!("the settings: {e}")))?;
        if Some(read.working) != facts.working {
            return Err(Structural("the working colour space changed".into()));
        }
        if read.subdiv.level != facts.subdiv_level {
            return Err(Structural("the subdivision level changed".into()));
        }
        // Tessellation that reads the camera stands only when what it read is
        // what it would read now, bit for bit: an exposure keeps it, a move,
        // a lens or a resolution does not.
        if read.subdiv.adaptive.map(|r| r.key()) != facts.adaptive {
            return Err(Structural(
                "adaptive subdivision would read another camera projection".into(),
            ));
        }
        if facts.on_screen_shapes.is_some()
            && read.projection.map(|p| p.key()) != facts.on_screen_shapes
        {
            return Err(Structural(
                "implicit shapes would be sized for another camera projection".into(),
            ));
        }
        if facts.unsized_shapes && read.projection.is_some() {
            return Err(Structural(
                "implicit shapes would be sized for the newly named camera".into(),
            ));
        }
        if read.domes_seen_by_camera != facts.domes_seen_by_camera {
            return Err(Structural("the dome's camera visibility changed".into()));
        }
        // The geometry kept the primvars the products asked for when it was
        // read, and nothing else: asking for another needs it read again.
        if read.products.primvars.key() != facts.primvars {
            return Err(Structural(
                "the products ask for other primvars than the geometry kept".into(),
            ));
        }
        if read
            .wanted_camera
            .as_ref()
            .is_some_and(|c| c.path() != &cam_path)
        {
            return Err(Structural("the render camera changed".into()));
        }
        drop(index);
        let stage = open_stage(layer, path_str, Some(cam_path.clone()))
            .map_err(|e| Structural(format!("cannot open the stage: {e}")))?;
        let prim: Prim = prim_at(&stage, cam_path.clone());
        let rc = build_camera(&stage, &prim, &read.settings)
            .ok_or_else(|| Structural(format!("{camera} is no longer a camera")))?;
        rc.warn_lens_units(Some(camera));
        let settings = apply_exposure(
            read.settings,
            read.exposure_compensation,
            rc.exposure_scale,
            Some(camera),
        );
        Ok(SettingsUpdate {
            settings,
            camera: rc.camera,
            aovs: read.products.request,
        })
    })();
    let raised = scope.finish();
    let update = built?;
    if let Some(w) = raised.first() {
        return Err(Structural(format!(
            "re-reading the settings raised {}",
            w.code.as_str()
        )));
    }
    Ok(update)
}

/// Each of `paths` as the import composes it: on a stage masked to them with
/// payloads loaded (so ops a payload authors count), at the import's time
/// code, through the traversal's own [`compose_with_parent`].
pub(crate) fn prim_frames(
    prov: &Provenance,
    layer: &Path,
    paths: &[String],
    place_op: &str,
) -> Result<Vec<PrimFrame>, Structural> {
    let path_str = layer
        .to_str()
        .ok_or_else(|| Structural("the layer path is not UTF-8".into()))?;
    let _time = EvalTimeScope::enter(prov.stage.options.frame);
    let prims = paths
        .iter()
        .map(|p| prim_path(p))
        .collect::<Result<Vec<_>, _>>()?;
    let stage = open_stage_masked(layer, path_str, Some(prims.clone()))
        .map_err(|e| Structural(format!("cannot open the stage: {e}")))?;
    // Warnings the walk raises are the import's to report, not placement's.
    let _scope = crate::WarningScope::enter();
    prims
        .iter()
        .map(|path| {
            let prim = stage
                .prim(path.clone())
                .ok()
                .filter(|p| p.is_valid().unwrap_or(false))
                .ok_or_else(|| Structural(format!("{path} is not a prim of the stage")))?;
            use openusd_schemas::geom::XformableSchema;
            let xf = openusd_schemas::geom::Xformable::from_prim(prim.clone())
                .ok()
                .flatten();
            let order: Vec<String> = xf
                .as_ref()
                .and_then(|xf| xf.xform_op_order().ok().flatten())
                .map(|o| o.iter().map(|t| t.as_str().to_string()).collect())
                .unwrap_or_default();
            let parent = path
                .parent()
                .map(|p| local_to_world(&stage, &prim_at(&stage, p)))
                .unwrap_or(glam::Mat4::IDENTITY);
            let world = compose_with_parent(&prim, parent);
            let place = prim
                .attribute(place_op)
                .get::<openusd::gf::Mat4d>()
                .ok()
                .flatten()
                .map(|m| glam::DMat4::from_cols_array(&m.0));
            Ok(PrimFrame {
                path: path.to_string(),
                xformable: xf.is_some(),
                resets_xform_stack: order.iter().any(|e| e == "!resetXformStack!"),
                parent_to_world: parent.as_dmat4(),
                local_to_world: world.as_dmat4(),
                xform_op_order: order,
                place_op: place,
            })
        })
        .collect()
}
