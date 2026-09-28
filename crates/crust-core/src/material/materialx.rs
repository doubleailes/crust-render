//! MaterialX surfaces: the adapter between `crust-mtlx` and crust's material model.
//!
//! USD's own answer to MaterialX is a file-format plugin that composes a
//! `.mtlx` into the stage as `UsdShade` prims. `openusd` ships no such plugin,
//! so a `Material` whose only opinion is
//! `references = @foo.mtlx@</MaterialX/Materials/name>` composes **empty** —
//! and the importer's material resolution, finding no surface source, falls
//! back to grey. So crust reads the `.mtlx` itself: parsing the document and
//! compiling its graph is the standalone [`crust_mtlx`] crate (re-exported as
//! [`crate::mtlx`]), which knows nothing about crust.
//!
//! This file is everything that *does*. [`MtlxMaterial`] implements
//! [`Material`] by running the compiled pattern program per hit and shading
//! the closure tree it parameterises — BSDF leaves combined by `layer`, `mix`,
//! `add` and `multiply`, with MaterialX's semantics — through
//! [`ResolvedClosure`] (`material/closure`). [`load`] is what the importer
//! calls.
//!
//! The tree is evaluated as a tree. It used to be pooled onto one OpenPBR
//! parameter set — roughness averaged per kind, a `layer`'s top guessed into
//! "coat or base specular" by the shape of the tree, transmission and thin
//! film lost — and every trap the design record kept for this file came from
//! that pooling. Each leaf now keeps its own parameters, normal and tangent.
//!
//! The division of labour with the host is the same as everywhere else in
//! crust: nothing here decodes pixels. An `image` node's file crosses the
//! [`crate::AssetLoader`] seam and comes back as a [`crate::Texture2D`]
//! sampler.

use crate::PathSampler;
use crate::hittable::HitRecord;
use crate::material::closure::{MAX_LEAVES, ResolvedClosure};
use crate::material::{Material, Resolution, ScatterSample};
use crate::ray::Ray;
use crust_mtlx::{Closures, Program, ShadeCtx, TextureLoader, Val};
use glam::Vec3A;

pub use crust_mtlx::MtlxError;

/// A surface whose closure tree comes from a MaterialX graph, its parameters
/// re-evaluated at every shading point.
///
/// Emission reaches the integrator through [`Material::emitted_at`] and
/// **not** through `emitted()`, which stays at the trait default of zero. That
/// is deliberate rather than an omission: a graph's emission is a function of
/// the shading point, `emitted()` is the hit-free radiance the *light list*
/// reads, and the two must agree for anything the light list samples. So a
/// MaterialX emitter is never an `AreaLight` — it is found by BSDF/bounce
/// sampling only, at full weight, exactly as emissive curves, instances and
/// volumes already are.
pub struct MtlxMaterial {
    program: Program,
    /// `program` compiled to machine code, when the `jit` feature is on and
    /// `CRUST_SHADER_JIT` is not `0`. It fills the slots with the same bits
    /// the interpreter does (crust-jit's own tests pin it), so it is purely a
    /// faster way to run `program`.
    #[cfg(feature = "jit")]
    jit: Option<crust_jit::JitProgram>,
    /// The closure tree, its EDF terms and interior volume.
    closures: Closures,
    /// Name of the material node, for diagnostics.
    pub name: String,
}

impl std::fmt::Debug for MtlxMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MtlxMaterial({}, {} ops + {} constants, {} leaves, {} emission)",
            self.name,
            self.program.ops.len(),
            self.program.consts.len(),
            self.closures.leaf_count(),
            self.closures.emission.len()
        )
    }
}

thread_local! {
    /// Scratch value stack for [`Program::eval`].
    ///
    /// One buffer per render thread, reused for every shading call. The
    /// alternative — a `Vec` per call — allocates at every path vertex.
    static SLOTS: std::cell::RefCell<Vec<Val>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// What [`MtlxMaterial::probe`] reports at one hit.
pub struct Probe {
    /// The collapsed closure: every live leaf with its weight, lobe and frame.
    pub closure: ResolvedClosure,
    /// The emitted radiance.
    pub emission: Vec3A,
}

impl MtlxMaterial {
    /// Whether the graph can emit at all. Known at compile time, so the
    /// non-emissive case never runs its graph for emission.
    fn can_emit(&self) -> bool {
        !self.closures.emission.is_empty()
    }

    /// Runs the program at a hit and hands `f` the evaluated slots.
    fn with_slots<R>(&self, r_in: &Ray, rec: &HitRecord, f: impl FnOnce(&[Val]) -> R) -> R {
        let ctx = ShadeCtx {
            uv: if rec.has_uv { rec.uv } else { (0.0, 0.0) },
            normal: rec.normal,
            tangent: rec.tangent,
            view: r_in.direction(),
            position: rec.p,
            uv_width: rec.uv_width,
        };
        SLOTS.with(|cell| {
            let mut slots = cell.borrow_mut();
            let shader = crate::profile::scope(crate::profile::Section::RunShader);
            #[cfg(feature = "jit")]
            match &self.jit {
                Some(jit) => jit.eval(&ctx, &mut slots),
                None => self.program.eval(&ctx, &mut slots),
            }
            #[cfg(not(feature = "jit"))]
            self.program.eval(&ctx, &mut slots);
            drop(shader);
            f(&slots)
        })
    }

    /// The emitted radiance: the EDF terms summed, each `weight · color` per
    /// channel.
    ///
    /// Neither factor is clamped above — `multiply(uniform_edf, 100)` is how
    /// a document authors a bright emitter — but each is sanitised before the
    /// product, per channel: a non-finite or negative channel contributes
    /// nothing, and clamping the product instead would let two negative
    /// channels multiply into positive light. The weight is read with
    /// `Val::rgb`, because a `multiply` by a `color3` tints an emitter per
    /// channel; taking lane 0 would turn `(0, 0.6, 0.9)` into black.
    fn emission(&self, slots: &[Val]) -> Vec3A {
        let clean = |v: Vec3A| {
            let f = |x: f32| if x.is_finite() { x.max(0.0) } else { 0.0 };
            Vec3A::new(f(v.x), f(v.y), f(v.z))
        };
        self.closures
            .emission
            .iter()
            .map(|e| clean(slots[e.weight as usize].rgb()) * clean(slots[e.color as usize].rgb()))
            .sum()
    }

    fn resolved(&self, r_in: &Ray, rec: &HitRecord) -> ResolvedClosure {
        self.with_slots(r_in, rec, |s| {
            ResolvedClosure::resolve(&self.closures, s, r_in, rec)
        })
    }

    /// The collapsed closure and emission at a hit.
    ///
    /// Exists for `examples/mtlx_shade`. A MaterialX surface can only be wrong
    /// in ways that still look like a surface, so the way to check one is to
    /// read the numbers it produces at a named point on the chart — not to
    /// compare renders.
    pub fn probe(&self, r_in: &Ray, rec: &HitRecord) -> Probe {
        self.with_slots(r_in, rec, |s| Probe {
            closure: ResolvedClosure::resolve(&self.closures, s, r_in, rec),
            emission: self.emission(s),
        })
    }
}

impl Material for MtlxMaterial {
    fn kind(&self) -> &'static str {
        "MaterialX"
    }

    fn scatter_importance(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        sampler: PathSampler,
    ) -> Option<ScatterSample> {
        self.resolved(r_in, rec).scatter(r_in, rec, sampler)
    }

    fn eval(&self, r_in: &Ray, rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        self.resolved(r_in, rec).eval(r_in, rec, wi)
    }

    /// One program run per vertex: the emission and the collapsed closure
    /// both come from the same evaluated slots.
    fn resolve(&self, r_in: &Ray, rec: &HitRecord, cos_theta_o: f32) -> Option<Resolution> {
        let _ = cos_theta_o;
        Some(self.with_slots(r_in, rec, |s| {
            let emitted = if self.can_emit() {
                self.emission(s)
            } else {
                Vec3A::ZERO
            };
            Resolution::closure(
                emitted,
                ResolvedClosure::resolve(&self.closures, s, r_in, rec),
                *rec,
            )
        }))
    }

    fn make_ray(&self, rec: &HitRecord, wi: Vec3A) -> Ray {
        // The medium is a function of the shading point, so an externally
        // chosen direction resolves the closure too. Only a transmitting
        // direction can need it.
        if rec.normal.dot(wi) >= 0.0 {
            return Ray::new(rec.p, wi);
        }
        let r_in = Ray::new(rec.p + rec.normal, -rec.normal);
        self.resolved(&r_in, rec).make_ray(rec, wi)
    }

    /// Unconditionally true: a network with no image can still carry a normal
    /// map over a constant, or a texcoord-driven procedural, and both need the
    /// chart.
    fn uses_uv(&self) -> bool {
        true
    }

    fn emitted_at(&self, r_in: &Ray, rec: &HitRecord, cos_theta_o: f32) -> Vec3A {
        let _ = cos_theta_o;
        if !self.can_emit() {
            return Vec3A::ZERO;
        }
        self.with_slots(r_in, rec, |s| self.emission(s))
    }
}

/// Everything the importer needs to know about one loaded `.mtlx` material.
pub struct Loaded {
    /// Typed rather than `Arc<dyn Material>` so a caller can still reach
    /// [`MtlxMaterial::probe`]; it coerces to the trait object wherever the
    /// importer needs one.
    pub material: std::sync::Arc<MtlxMaterial>,
    /// How the material described itself — operator and leaf counts — for a
    /// debug line.
    pub summary: String,
    /// Node categories the compiler had no operator for, for one warning per
    /// material instead of one per node.
    pub unsupported: Vec<String>,
    /// Authored inputs the renderer cannot represent and closures it
    /// approximates, for one warning per material.
    pub reported: Vec<String>,
    /// How many `image` nodes resolved to a real texture.
    pub textures: usize,
}

/// Is the MaterialX program optimiser on? `CRUST_MTLX_OPT=0` keeps the
/// program exactly as compiled — every literal an instruction, nothing folded
/// or pruned — which is the reference the optimised program is pinned
/// against, and must render bit-identically to it.
fn optimize_enabled() -> bool {
    crate::config().mtlx_opt
}

/// Is the shader JIT on? `CRUST_SHADER_JIT=0` runs every MaterialX program on
/// the interpreter, which the JIT must match bit for bit — the A/B for any
/// change to either.
#[cfg(feature = "jit")]
fn jit_enabled() -> bool {
    crate::config().shader_jit
}

/// Builds a material from a `.mtlx` file.
///
/// `material_node` is the name of the `surfacematerial` (or `surface`) node to
/// start from; USD spells it as the last component of the reference's prim
/// path, `</MaterialX/Materials/surfacematerial_teapot_ceramic>`. When it is
/// `None` the first `surfacematerial` in the document is used.
///
/// `load_texture` resolves an `image` node's `file` — relative to the `.mtlx`
/// itself, which is how MaterialX anchors asset paths — into a sampler.
///
/// A tree with more leaves than [`MAX_LEAVES`] is refused rather than shaded
/// with some of its leaves silently missing.
pub fn load(
    path: &std::path::Path,
    material_node: Option<&str>,
    load_texture: TextureLoader<'_>,
) -> Result<Loaded, MtlxError> {
    let mut c = crust_mtlx::compile(path, material_node, load_texture)?;
    let leaves = c.closures.leaf_count();
    if leaves > MAX_LEAVES {
        return Err(MtlxError::Unsupported(format!(
            "{}: {leaves} BSDF leaves, more than the {MAX_LEAVES} a shading point holds",
            c.root_name
        )));
    }
    if optimize_enabled() {
        c.optimize();
    }
    #[cfg(feature = "jit")]
    let jit = jit_enabled()
        .then(|| match crust_jit::JitProgram::new(&c.program) {
            Ok(j) => Some(j),
            Err(e) => {
                tracing::warn!("{e}; {} runs on the interpreter", c.root_name);
                None
            }
        })
        .flatten();
    let reported: Vec<String> = c.closures.reported.iter().cloned().collect();
    let material = MtlxMaterial {
        #[cfg(feature = "jit")]
        jit,
        program: c.program,
        closures: c.closures,
        name: c.root_name,
    };
    let summary = format!("{material:?}");
    Ok(Loaded {
        material: std::sync::Arc::new(material),
        summary,
        unsupported: c.unsupported,
        reported,
        textures: c.textures,
    })
}

/// Splits a USD reference target such as `/MaterialX/Materials/surfacematerial_x`
/// into the material node name MaterialX knows it by.
///
/// USD's MaterialX plugin namespaces a document's materials under
/// `/MaterialX/Materials/`, so the leaf is the `surfacematerial` node's own
/// `name` attribute — which is what [`load`] looks up.
pub fn material_node_of(prim_path: &str) -> Option<&str> {
    let leaf = prim_path.rsplit('/').next()?;
    (!leaf.is_empty()).then_some(leaf)
}

#[cfg(test)]
mod tests;
