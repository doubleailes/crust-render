//! MaterialX surfaces: the adapter between `crust-mtlx` and crust's material model.
//!
//! USD's own answer to MaterialX is a file-format plugin that composes a
//! `.mtlx` into the stage as `UsdShade` prims. `openusd` ships no such plugin,
//! so a `Material` whose only opinion is
//! `references = @foo.mtlx@</MaterialX/Materials/name>` composes **empty** —
//! and the importer's material resolution, finding no surface source, falls
//! back to grey. That is what the two DPEL assets (MaterialX Teapot, MaterialX
//! Lion) look like on import without this module.
//!
//! So crust reads the `.mtlx` itself. Parsing the document and compiling its
//! graph is the standalone [`crust_mtlx`] crate (re-exported as
//! [`crate::mtlx`]), which knows nothing about crust. This file is everything
//! that *does*: [`MtlxMaterial`] implements [`Material`] by running the
//! compiled program per hit and delegating the BSDF to the [`OpenPBR`] it
//! reduces to; [`reduce`] is that pooling of flattened lobes onto OpenPBR's
//! fixed lobe stack; [`load`] is what the importer calls.
//!
//! The pooling is the lossy part, and deliberately so. crust-mtlx flattens a
//! `layer`/`mix` tree into weighted lobes at compile time (see its `bsdf`
//! module); **at shading time** those lobes are pooled by kind and the pools
//! normalised into OpenPBR parameters. OpenPBR has two specular lobes, and the
//! flattening keeps the one structural fact that tells them apart: a
//! dielectric layered directly over a diffuse is the **base specular** (that
//! is how OpenPBR's own dielectric base is built), while a dielectric layered
//! over a base that already carries a specular — another dielectric, a
//! conductor — arrives as a [`LobeKind::Coat`] and lands on OpenPBR's coat
//! lobe with its own roughness and IOR. The teapot's ceramic, a smooth glaze
//! (roughness 0.002) over a mask-driven rougher one over diffuse, therefore
//! keeps both lobes; a varnish over a conductor keeps its varnish (the single
//! pool used to lose it outright, since a metal base zeroes the dielectric
//! Fresnel term).
//!
//! What this still cannot represent: a stack of *three* or more dielectrics
//! pools its upper ones into one coat roughness; a coat dielectric's `tint`
//! is ignored, because MaterialX tints the coat's *reflection* while OpenPBR's
//! `coat_color` is absorption on the way through to the substrate; and the
//! promotion is decided by the tree, not by weights — a glaze over a base
//! specular whose mask evaluates to zero at some point still shades there as
//! coat-over-diffuse. That last one is a choice: deciding per point would
//! draw a hard seam along the mask's zero contour, since a coat and a base
//! specular attenuate the substrate very differently.
//!
//! The division of labour with the host is the same as everywhere else in
//! crust: nothing here decodes pixels. An `image` node's file crosses the
//! [`crate::AssetLoader`] seam and comes back as a [`crate::Texture2D`]
//! sampler.

use crate::PathSampler;
use crate::hittable::HitRecord;
use crate::material::brdf::alpha_to_roughness;
use crate::material::{Material, OpenPBR, ScatterSample};
use crate::ray::Ray;
use crust_mtlx::{
    Flattened, LobeKind, Program, ShadeCtx, TextureLoader, Val, reflectivity_from_ior,
};
use glam::Vec3A;

pub use crust_mtlx::MtlxError;

/// A surface whose parameters come from a MaterialX graph, re-evaluated at
/// every shading point.
///
/// Holds the compiled pattern [`Program`] and the flattened closure tree —
/// BSDF lobes and EDF emission terms — and implements [`Material`] by running
/// both and delegating the actual BSDF to the [`OpenPBR`] they reduce to.
/// Delegating rather than reimplementing is the whole point: sampling,
/// evaluation, MIS densities, the coat and fuzz layering and the energy
/// compensation all stay in one place, and a MaterialX surface is unbiased by
/// exactly the same argument as an authored one.
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
    /// The BSDF lobes and the EDF emission terms the graph flattened to.
    flat: Flattened,
    /// Defaults for everything neither a lobe nor an emission term speaks to,
    /// and the fallback when the graph yields nothing at all.
    base: OpenPBR,
    /// Name of the material node, for diagnostics.
    pub name: String,
}

impl std::fmt::Debug for MtlxMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MtlxMaterial({}, {} ops + {} constants, {} lobes, {} emission)",
            self.name,
            self.program.ops.len(),
            self.program.consts.len(),
            self.flat.lobes.len(),
            self.flat.emission.len()
        )
    }
}

thread_local! {
    /// Scratch value stack for [`Program::eval`].
    ///
    /// One buffer per render thread, reused for every shading call. The
    /// alternative — a `Vec` per call — allocates several times per path
    /// vertex (sample, then once per NEE and guide evaluation), which is a
    /// cost spread too thinly across the profile to ever look like a
    /// bottleneck while still being pure waste.
    static SLOTS: std::cell::RefCell<Vec<Val>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl MtlxMaterial {
    /// Evaluates the graph at a hit and hands the resulting OpenPBR to `f`.
    ///
    /// Every call runs the graph. The integrator does not come through here:
    /// it asks [`Material::resolve`] once per vertex and queries the result,
    /// so this serves the direct `Material` methods (tests, probes).
    fn shade<R>(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        f: impl FnOnce(&OpenPBR, &HitRecord) -> R,
    ) -> R {
        let (params, rec) = self.run(r_in, rec);
        f(&params, &rec)
    }

    /// Whether the graph can emit at all: an EDF, or an emissive base.
    fn can_emit(&self) -> bool {
        !self.flat.emission.is_empty() || self.base.emission_luminance > 0.0
    }

    /// Runs the graph at a hit: the OpenPBR it reduces to, and the record
    /// with the graph's shading normal applied.
    fn run(&self, r_in: &Ray, rec: &HitRecord) -> (OpenPBR, HitRecord) {
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
            #[cfg(feature = "jit")]
            match &self.jit {
                Some(jit) => jit.eval(&ctx, &mut slots),
                None => self.program.eval(&ctx, &mut slots),
            }
            #[cfg(not(feature = "jit"))]
            self.program.eval(&ctx, &mut slots);
            let (params, normal) = reduce(&self.flat, &slots, &self.base);
            // A shading normal from the graph replaces the geometric one for
            // the BSDF, but must not flip the surface: a normal map can push
            // the frame past the horizon on a silhouette, and shading with a
            // normal facing away from the viewer makes a black rim.
            let mut rec = *rec;
            if let Some(n) = normal
                && n.length_squared() > 1e-12
            {
                let n = n.normalize();
                if n.dot(rec.normal) > 1e-3 {
                    rec.normal = n;
                }
            }
            (params, rec)
        })
    }
}

impl Material for MtlxMaterial {
    fn scatter_importance(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        sampler: PathSampler,
    ) -> Option<ScatterSample> {
        self.shade(r_in, rec, |m, rec| m.scatter_importance(r_in, rec, sampler))
    }

    fn eval(&self, r_in: &Ray, rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        self.shade(r_in, rec, |m, rec| m.eval(r_in, rec, wi))
    }

    fn resolve(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        cos_theta_o: f32,
    ) -> Option<crate::material::Resolution> {
        let (params, rec) = self.run(r_in, rec);
        // `emitted_at`'s answer, from the run already made rather than a
        // second one; see there for the gate and for `cos_theta_o`.
        let emitted = if self.can_emit() {
            params.emitted_directional(cos_theta_o)
        } else {
            Vec3A::ZERO
        };
        Some(crate::material::Resolution {
            bsdf: params.into_resolved(&rec),
            rec,
            emitted,
        })
    }

    fn uses_uv(&self) -> bool {
        // Unconditionally true rather than "does the program hold a texture":
        // a graph with no `image` node can still carry a `normalmap` over a
        // constant, or a `texcoord`-driven procedural, and both need the
        // chart. The cost of an unnecessary table is bounded; shading a
        // textured surface at (0, 0) everywhere is not obviously wrong on
        // screen, which is the failure worth avoiding.
        true
    }

    fn emitted_at(&self, r_in: &Ray, rec: &HitRecord, cos_theta_o: f32) -> Vec3A {
        // The integrator asks this of *every* surface hit, so the non-emissive
        // case — which is every MaterialX material shipped today, the two DPEL
        // assets included — must not evaluate the graph. Without this early
        // out the teapot would run its ~50-op program, and the lion its
        // 140-op one, an extra time per path vertex to be told the answer is
        // zero. Whether the graph has an EDF at all is known at compile time,
        // so this is the same kind of structural gate as `uses_uv`.
        if !self.can_emit() {
            return Vec3A::ZERO;
        }
        // `cos_theta_o` is used as the tracer measured it, against the
        // ray-facing geometric normal, rather than recomputed against any
        // normal the graph produces: the coat slab emission passes through
        // belongs to the geometric interface, and letting a normal map
        // perturb its Fresnel falloff would make emission flicker at pixel
        // scale for nothing.
        self.shade(r_in, rec, |m, _| m.emitted_directional(cos_theta_o))
    }

    fn make_ray(&self, rec: &HitRecord, wi: Vec3A) -> Ray {
        // No graph evaluation: `make_ray` only decides whether the ray carries
        // an interior medium, and this reduction never produces a
        // transmissive OpenPBR (MaterialX transmission maps to no lobe here).
        Ray::new(rec.p, wi)
    }
}

/// Everything the importer needs to know about one loaded `.mtlx` material.
pub struct Loaded {
    /// Typed rather than `Arc<dyn Material>` so a caller can still reach
    /// [`MtlxMaterial::probe`]; it coerces to the trait object wherever the
    /// importer needs one.
    pub material: std::sync::Arc<MtlxMaterial>,
    /// How the reduction described itself — operator count and lobe count —
    /// for a debug line the importer can print without `dyn Material` having
    /// to implement `Debug`.
    pub summary: String,
    /// Node categories the compiler had no operator for, for one warning per
    /// material instead of one per node.
    pub unsupported: Vec<String>,
    /// Whether any `image` node resolved to a real texture. A material whose
    /// every texture was declined still renders — on its constant inputs — but
    /// the difference between "no textures authored" and "no textures found"
    /// is worth surfacing.
    pub textures: usize,
}

/// Is the MaterialX program optimiser on? `CRUST_MTLX_OPT=0` keeps the
/// program exactly as compiled — every literal an instruction, nothing folded
/// or pruned — which is the reference the optimised program is pinned
/// against, and must render bit-identically to it.
fn optimize_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("CRUST_MTLX_OPT").as_deref() != Ok("0"))
}

/// Is the shader JIT on? `CRUST_SHADER_JIT=0` runs every MaterialX program on
/// the interpreter, which the JIT must match bit for bit — the A/B for any
/// change to either.
#[cfg(feature = "jit")]
fn jit_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("CRUST_SHADER_JIT").as_deref() != Ok("0"))
}

/// Builds a material from a `.mtlx` file.
///
/// `material_node` is the name of the `surfacematerial` (or `surface`) node to
/// start from; USD spells it as the last component of the reference's prim
/// path, `</MaterialX/Materials/surfacematerial_teapot_ceramic>`. When it is
/// `None` the first `surfacematerial` in the document is used, which is what a
/// single-material document means.
///
/// `load_texture` resolves an `image` node's `file` — relative to the `.mtlx`
/// itself, which is how MaterialX anchors asset paths — into a sampler.
pub fn load(
    path: &std::path::Path,
    material_node: Option<&str>,
    load_texture: TextureLoader<'_>,
) -> Result<Loaded, MtlxError> {
    let mut c = crust_mtlx::compile(path, material_node, load_texture)?;
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
    let material = MtlxMaterial {
        #[cfg(feature = "jit")]
        jit,
        program: c.program,
        flat: Flattened {
            lobes: c.lobes,
            emission: c.emission,
        },
        // `coat_darkening` is the one default a MaterialX material must *not*
        // inherit. OpenPBR's coat darkening is the bounce series between the
        // coat's underside and the substrate: a fraction `K̄` of what the base
        // reflects is turned back down by total internal reflection and
        // re-absorbed. MaterialX's `layer` models nothing of the sort — it is
        // single-scattering, `base·(1 − F) + top` — so a coat that exists only
        // because `reduce()` promoted a `dielectric_bsdf` must not impose it.
        // It is not a small correction: at the DPEL lion's `coat_ior` of 1.45,
        // `K̄ = 0.540`, and against its substrate albedo of ~0.22 the factor is
        // 0.52 — the whole body at half brightness against the asset's own
        // reference render. A material authored as OpenPBR keeps the spec
        // default of 1.0; only this path opts out.
        base: OpenPBR {
            coat_darkening: 0.0,
            ..OpenPBR::default()
        },
        name: c.root_name,
    };
    let summary = format!("{material:?}");
    Ok(Loaded {
        material: std::sync::Arc::new(material),
        summary,
        unsupported: c.unsupported,
        textures: c.textures,
    })
}

/// Evaluates a material's graph at a hit and returns the OpenPBR parameters it
/// reduces to.
///
/// Exists for `examples/mtlx_shade`. A MaterialX surface can only be wrong in
/// ways that still look like a surface, so the way to check one is to read the
/// numbers it produces at a named point on the chart — not to compare renders.
impl MtlxMaterial {
    pub fn probe(&self, r_in: &Ray, rec: &HitRecord) -> OpenPBR {
        self.run(r_in, rec).0
    }
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

/// One pool's running totals.
#[derive(Default, Clone, Copy)]
struct Pool {
    w: f32,
    color: Vec3A,
    roughness: f32,
    ior: f32,
}

impl Pool {
    fn add(&mut self, w: f32, color: Vec3A, roughness: f32, ior: f32) {
        self.w += w;
        self.color += color * w;
        self.roughness += roughness * w;
        self.ior += ior * w;
    }

    /// Weighted means, or the supplied neutral when the pool is empty.
    fn mean_color(&self, neutral: Vec3A) -> Vec3A {
        if self.w > 1e-6 {
            self.color / self.w
        } else {
            neutral
        }
    }

    fn mean(&self, total: f32, neutral: f32) -> f32 {
        if self.w > 1e-6 {
            total / self.w
        } else {
            neutral
        }
    }

    /// `mean`, for a pool whose roughness accumulator is in MaterialX's
    /// **alpha** rather than crust's perceptual roughness. The neutral is
    /// already perceptual (it comes from the OpenPBR base), so only the pooled
    /// branch is converted.
    fn mean_alpha(&self, total: f32, neutral: f32) -> f32 {
        if self.w > 1e-6 {
            alpha_to_roughness(total / self.w)
        } else {
            neutral
        }
    }
}

/// Folds the evaluated lobes into an OpenPBR parameter set.
///
/// `base` is the material's authored defaults, so anything neither a lobe nor
/// an emission term speaks to (transmission, thin-film) keeps a sensible value
/// instead of zero.
/// `coat_darkening` is one of those — no lobe speaks to it — and `load()`
/// supplies it as 0 rather than OpenPBR's 1.0, because MaterialX's `layer` is
/// single-scattering and models no coat-underside bounce series. See the
/// comment there before re-deriving it here.
pub fn reduce(flat: &Flattened, slots: &[Val], base: &OpenPBR) -> (OpenPBR, Option<Vec3A>) {
    let lobes = &flat.lobes;
    let get = |i: u32| slots.get(i as usize).copied().unwrap_or(Val::ZERO);
    let (mut diffuse, mut spec, mut coat, mut metal, mut sheen, mut sss) = (
        Pool::default(),
        Pool::default(),
        Pool::default(),
        Pool::default(),
        Pool::default(),
        Pool::default(),
    );
    let mut normal_sum = Vec3A::ZERO;
    let mut normal_w = 0.0f32;

    for l in lobes {
        // A weight can leave the graph slightly outside [0,1] (a mask run
        // through `contrast` is not clamped by MaterialX either); clamping
        // here keeps the pools' normalisation meaningful.
        let w = get(l.weight).x().clamp(0.0, 1.0);
        // NaN-safe by construction: a weight that leaves the graph as NaN
        // fails this comparison and the lobe is dropped, rather than poisoning
        // every pool total it would otherwise be added to.
        if w.is_nan() || w <= 1e-5 {
            continue;
        }
        let color = get(l.color).rgb();
        let rough = get(l.roughness).x().clamp(0.0, 1.0);
        let ior = get(l.ior).x();
        match l.kind {
            LobeKind::Diffuse => diffuse.add(w, color, rough, 0.0),
            LobeKind::Dielectric => spec.add(w, color, rough, ior),
            LobeKind::Coat => coat.add(w, color, rough, ior),
            LobeKind::Conductor => {
                // Recover the reflectivity colour OpenPBR's metal lobe takes.
                // `ior`/`extinction` are the general authoring; when the graph
                // fed them from `artistic_ior` this inverts it exactly.
                let n = get(l.ior).rgb();
                let k = get(l.extinction).rgb();
                let refl =
                    if k.length_squared() > 1e-12 || (n - Vec3A::ONE).length_squared() > 1e-12 {
                        reflectivity_from_ior(n, k) * color
                    } else {
                        color
                    };
                metal.add(w, refl, rough, 0.0);
            }
            LobeKind::Sheen => sheen.add(w, color, rough, 0.0),
            LobeKind::Subsurface => sss.add(w, color, rough, 0.0),
        }
        if let Some(slot) = l.normal {
            let n = get(slot).rgb();
            if n.length_squared() > 1e-12 {
                normal_sum += n.normalize() * w;
                normal_w += w;
            }
        }
    }

    // Base colour is a diffuse/metal blend off one `base_color`, which is
    // exactly the shape of these two pools.
    let base_total = diffuse.w + metal.w + sss.w;
    let mut m = base.clone();
    if base_total > 1e-5 {
        m.base_color = (diffuse.color + metal.color + sss.color) / base_total;
        m.base_weight = base_total.clamp(0.0, 1.0);
    } else if spec.w > 1e-5 || coat.w > 1e-5 {
        // A purely specular stack (a bare glass shell): keep the specular
        // interfaces, but do not leave an unlit grey base under them.
        m.base_weight = 0.0;
    }

    // How much surface the dielectric *base* covers. Diffuse and a dielectric
    // interface are **layered**, not partitioned — `layer(dielectric, diffuse)`
    // hands both full weight over the same area — so the base they share covers
    // the larger of the two rather than their sum. Taking the max is also what
    // lets a bare dielectric with nothing opaque under it still count as a
    // dielectric base, which is the case that used to vanish.
    let diel_base = (diffuse.w + sss.w).max(spec.w);

    // Metal against that base. The two *are* mutually exclusive — a `mix` is
    // what puts a conductor beside a dielectric base — so this ratio is exactly
    // what OpenPBR's `base_metalness` means, and `eval_specular` reads it as
    // the metal half's whole coverage.
    //
    // The denominator has to include the dielectric interface, not just the
    // opaque pools: `mix(conductor, bare_dielectric)` has no diffuse at all, so
    // dividing by `diffuse + metal + sss` pinned metalness to 1 and
    // `eval_specular` then dropped the dielectric half entirely.
    let substrate = diel_base + metal.w;
    if substrate > 1e-5 {
        m.base_metalness = (metal.w / substrate).clamp(0.0, 1.0);
    }
    // NOT un-squared: `oren_nayar_diffuse_bsdf`'s `roughness` is its Oren-Nayar
    // sigma, not a microfacet alpha. Only the GGX lobes below convert.
    m.base_diffuse_roughness = diffuse.mean(diffuse.roughness, 0.0).clamp(0.0, 1.0);

    // The specular lobe serves both the dielectric coat and the metal's own
    // microfacet distribution, so its roughness is their joint weighted mean —
    // taken in MaterialX's alpha, then un-squared once (`alpha_to_roughness`)
    // because crust's `specular_roughness` is perceptual and gets squared again
    // by `roughness_to_alpha_aniso`.
    //
    // Pooling *then* converting, rather than converting each lobe as it lands,
    // is a deliberate choice. `√` is concave, so `mean(√α) ≤ √(mean α)`; the two
    // agree only when a single lobe contributes, which is every case that
    // cannot tell them apart. What is genuinely linear across a GGX mixture is
    // the slope second moment, ∝ α², so the moment-preserving pool would be
    // `√(Σ wᵢαᵢ²)` — and against that, averaging in α is the closer of the two,
    // while `mean(√α)` is biased *smooth*. It also keeps this joint spec+metal
    // numerator meaningful, since it only makes sense in one space. If the pool
    // ever needs to preserve the moment exactly, accumulate `rough*rough` in
    // `Pool::add` and take `sqrt(sqrt(mean))` here.
    let rough_w = spec.w + metal.w;
    m.specular_roughness = if rough_w > 1e-5 {
        alpha_to_roughness(((spec.roughness + metal.roughness) / rough_w).clamp(0.0, 1.0))
    } else {
        // Already perceptual — it is the OpenPBR base, not a MaterialX alpha.
        base.specular_roughness
    };
    // The interface's coverage *within* the dielectric base, which is what
    // OpenPBR's `specular_weight` means — not its coverage of the whole
    // surface. `eval_specular` already scales the dielectric half by
    // `(1 - base_metalness)`, so storing the raw pool weight here counted the
    // same masking twice; the render sees `(1 - base_metalness) * this`, and
    // that product is what has to come back out as the authored coverage.
    //
    // A material with no dielectric leaf lands on 0, and that is now the right
    // answer rather than a problem: the metal half no longer reads this, so
    // there is nothing left to keep alive with a synthetic 1.0. That synthetic
    // weight was itself a defect — it gave every conductor-only graph a
    // full-strength white dielectric lobe its author never wrote.
    m.specular_weight = if diel_base > 1e-5 {
        (spec.w / diel_base).clamp(0.0, 1.0)
    } else {
        0.0
    };
    m.specular_color = spec.mean_color(Vec3A::ONE);
    m.specular_ior = spec.mean(spec.ior, base.specular_ior).clamp(1.0, 3.0);

    // The second specular lobe. `coat_color` and `coat_darkening` stay at the
    // base defaults: a MaterialX dielectric's `tint` multiplies its
    // *reflection*, whereas OpenPBR's coat reflection is untinted and
    // `coat_color` is the absorption of light passing through to the
    // substrate — mapping one onto the other would tint the wrong thing.
    m.coat_weight = coat.w.clamp(0.0, 1.0);
    m.coat_roughness = coat
        .mean_alpha(coat.roughness, base.coat_roughness)
        .clamp(0.0, 1.0);
    m.coat_ior = coat.mean(coat.ior, base.coat_ior).clamp(1.0, 3.0);

    m.fuzz_weight = sheen.w.clamp(0.0, 1.0);
    m.fuzz_color = sheen.mean_color(Vec3A::ONE);
    // NOT un-squared: `sheen_bsdf`'s `roughness` drives the Charlie NDF
    // directly in MaterialX, exactly as `fuzz_roughness` does here. It is not a
    // GGX alpha.
    m.fuzz_roughness = sheen.mean(sheen.roughness, 0.3).clamp(0.0, 1.0);

    if sss.w > 1e-5 {
        m.subsurface_weight = (sss.w / base_total.max(1e-5)).clamp(0.0, 1.0);
        m.subsurface_color = sss.mean_color(Vec3A::ONE);
    }

    // Emission, and this is the one pool that **adds** rather than averages.
    // MaterialX's `add` sums two EDFs, `mix` partitions one between two
    // branches and `multiply` scales one — so the flattened terms are already
    // a partition of one radiance and summing them reconstructs it. The BSDF
    // pools take a weighted *mean* because two diffuse leaves describe one
    // surface shared between them; two emitters are twice the light.
    //
    // Neither the weight nor the colour is clamped *above*, which is deliberate
    // and is the whole point of reading an EDF at all. `multiply(uniform_edf,
    // 100)` is how a MaterialX document authors a bright emitter, and the
    // colour can come straight off an `image` node reading an HDR file.
    // Clamping either would put back the very ceiling this path exists to
    // remove — emission is the one shading input for which a value above 1.0 is
    // meaningful rather than an authoring error (an albedo above 1 creates
    // energy; radiance above 1 is just a bright light).
    //
    // **Both factors are read as RGB**, and the weight is not a scalar. MaterialX
    // declares `ND_multiply_edfC` — `multiply` on an EDF by a `color3` — so a
    // tinted emitter is ordinary authoring, and `Op::Binary { Mul }` promotes
    // arity through `Val::zip`, landing the tint in the weight slot per channel.
    // Reading lane 0 alone turned a weight of `(0, 0.6, 0.9)` into a *black*
    // emitter and `(1, 0.5, 0.2)` into a neutral one at full strength. `rgb()`
    // broadcasts an arity-1 `Val`, so a `float` weight still scales all three
    // channels and the scalar path is unchanged.
    //
    // Each factor is sanitised *before* the product rather than after, which is
    // what stops two negative channels multiplying into positive light.
    //
    // Non-finite is refused per channel, where the lobe loop above drops the
    // whole lobe — and the difference is structural, not stylistic. Emission
    // **sums**, so a zeroed channel contaminates nothing; a lobe's weight is a
    // *divisor* (`Pool::w` normalises every colour in its pool), so a NaN there
    // has to take the lobe with it. Keeping `radiance` finite and non-negative
    // is also what the `peak` factorisation below relies on.
    let sane = |v: Vec3A| Vec3A::select(v.is_finite_mask(), v, Vec3A::ZERO).max(Vec3A::ZERO);
    let mut radiance = Vec3A::ZERO;
    for t in &flat.emission {
        radiance += sane(get(t.weight).rgb()) * sane(get(t.color).rgb());
    }
    let peak = radiance.max_element();
    if peak > 0.0 {
        // OpenPBR splits emission into a scalar and a colour and `emitted()`
        // multiplies them straight back, so the split is a presentation choice
        // and any factorisation is exact. Factor by the **peak channel**:
        // `emission_color` then always lands in [0,1]³ with one channel at
        // exactly 1 — a chromaticity, which is what anything reading that
        // field on its own will assume — and the whole HDR range lives in the
        // scalar. Factoring by Rec.709 luminance instead, which is what
        // OpenPBR's spec means by nits, sends a saturated emitter's *colour*
        // above one: (0, 0, 8) has luminance 0.43 and would store a colour of
        // (0, 0, 18.6).
        m.emission_luminance = peak;
        m.emission_color = radiance / peak;

        // A `surface` with an `edf` and no `bsdf` flattens to no lobes at all,
        // and the branches above would then leave `base_weight` at the base's
        // 1.0 over OpenPBR's default grey — a pure emitter that is also a grey
        // diffuse reflector. `specular_weight` and `coat_weight` already land
        // on 0 through their own empty pools.
        if base_total <= 1e-5 && spec.w <= 1e-5 && coat.w <= 1e-5 {
            m.base_weight = 0.0;
        }
    }

    let normal = (normal_w > 1e-5).then(|| normal_sum / normal_w);
    (m, normal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crust_mtlx::{Compiler, Doc, flatten};

    #[test]
    fn a_usd_reference_target_names_the_materialx_node() {
        assert_eq!(
            material_node_of("/MaterialX/Materials/surfacematerial_teapot_ceramic"),
            Some("surfacematerial_teapot_ceramic")
        );
        assert_eq!(material_node_of(""), None);
    }

    /// Compiles a document and evaluates it at one flat, upward-facing point.
    fn evaluate(text: &str, root: &str) -> (Flattened, Vec<Val>) {
        let doc = Doc::parse(text).unwrap();
        let loader = |_: &str, _: Option<&str>| None;
        let mut c = Compiler::new(&doc, &loader);
        let one = c.constant(Val::ONE);
        let node = doc.find("", root).unwrap().clone();
        let mut flat = Flattened::default();
        flatten(&mut c, &node, one, 0, &mut flat);
        let mut slots = Vec::new();
        c.program.eval(
            &ShadeCtx {
                uv: (0.0, 0.0),
                normal: Vec3A::Z,
                tangent: Vec3A::X,
                view: -Vec3A::Z,
                position: Vec3A::ZERO,
                uv_width: 0.0,
            },
            &mut slots,
        );
        (flat, slots)
    }

    #[test]
    fn a_zero_weight_leaf_contributes_nothing() {
        // Both assets use a `weight = 0` dielectric as a mix's null branch;
        // if its own weight input were ignored it would coat every surface.
        let text = r#"<materialx>
          <dielectric_bsdf name="null" type="BSDF">
            <input name="weight" type="float" value="0" />
          </dielectric_bsdf>
          <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
          <mix name="m" type="BSDF">
            <input name="bg" type="BSDF" nodename="null" />
            <input name="fg" type="BSDF" nodename="d" />
            <input name="mix" type="float" value="0.5" />
          </mix>
        </materialx>"#;
        let (flat, slots) = evaluate(text, "m");
        let (m, _) = reduce(&flat, &slots, &OpenPBR::default());
        assert_eq!(
            m.specular_weight, 0.0,
            "a weight=0 dielectric coated the surface"
        );
    }

    #[test]
    fn a_conductor_becomes_metal_and_a_diffuse_does_not() {
        let text = r#"<materialx>
          <oren_nayar_diffuse_bsdf name="d" type="BSDF">
            <input name="color" type="color3" value="0.8, 0.2, 0.2" />
          </oren_nayar_diffuse_bsdf>
          <conductor_bsdf name="c" type="BSDF">
            <input name="roughness" type="float" value="0.1" />
          </conductor_bsdf>
          <mix name="m" type="BSDF">
            <input name="bg" type="BSDF" nodename="c" />
            <input name="fg" type="BSDF" nodename="d" />
            <input name="mix" type="float" value="0.25" />
          </mix>
        </materialx>"#;
        let (flat, slots) = evaluate(text, "m");
        let (m, _) = reduce(&flat, &slots, &OpenPBR::default());
        assert!(
            (m.base_metalness - 0.75).abs() < 1e-4,
            "{}",
            m.base_metalness
        );
    }

    // --- Two specular lobes -------------------------------------------------

    const DIFFUSE: &str = r#"<oren_nayar_diffuse_bsdf name="d" type="BSDF">
        <input name="color" type="color3" value="0.6, 0.1, 0.1" />
      </oren_nayar_diffuse_bsdf>"#;
    const SATIN: &str = r#"<dielectric_bsdf name="satin" type="BSDF">
        <input name="roughness" type="vector2" value="0.4, 0.4" />
        <input name="ior" type="float" value="1.5" />
      </dielectric_bsdf>"#;
    const CLEAR: &str = r#"<dielectric_bsdf name="clear" type="BSDF">
        <input name="roughness" type="vector2" value="0.02, 0.02" />
        <input name="ior" type="float" value="1.6" />
      </dielectric_bsdf>"#;

    fn layer(name: &str, top: &str, base: &str) -> String {
        format!(
            r#"<layer name="{name}" type="BSDF">
                 <input name="top" type="BSDF" nodename="{top}" />
                 <input name="base" type="BSDF" nodename="{base}" />
               </layer>"#
        )
    }

    fn reduced(text: &str, root: &str) -> OpenPBR {
        let (flat, slots) = evaluate(text, root);
        reduce(&flat, &slots, &OpenPBR::default()).0
    }

    /// A `<surface>` with the given `edf` expression and no `bsdf`, reduced.
    fn emissive(body: &str, edf_node: &str) -> OpenPBR {
        let text = format!(
            r#"<materialx>
                 {body}
                 <surface name="s" type="surfaceshader">
                   <input name="edf" type="EDF" nodename="{edf_node}" />
                 </surface>
               </materialx>"#
        );
        reduced(&text, "s")
    }

    /// What the integrator actually sees: the two OpenPBR emission fields are
    /// only ever multiplied back together, so their product is the contract
    /// and either field alone is a presentation detail.
    fn radiance_of(m: &OpenPBR) -> Vec3A {
        m.emission_color * m.emission_luminance
    }

    #[test]
    fn an_emission_term_reaches_openpbr_emission() {
        let m = emissive(
            r#"<uniform_edf name="e" type="EDF">
                 <input name="color" type="color3" value="0.5, 0.25, 0.125" />
               </uniform_edf>"#,
            "e",
        );
        let r = radiance_of(&m);
        assert!(
            (r - Vec3A::new(0.5, 0.25, 0.125)).length() < 1e-5,
            "emission reduced to {r:?}"
        );
    }

    /// The test this whole path exists for. An albedo above 1 creates energy
    /// and `eon_diffuse` clamps it, correctly — radiance above 1 is just a
    /// bright light, and nothing between the graph and the film may bound it.
    #[test]
    fn an_emission_above_one_is_not_clamped() {
        let m = emissive(
            r#"<uniform_edf name="e" type="EDF">
                 <input name="color" type="color3" value="1, 1, 1" />
               </uniform_edf>
               <multiply name="mul" type="EDF">
                 <input name="in1" type="EDF" nodename="e" />
                 <input name="in2" type="float" value="8" />
               </multiply>"#,
            "mul",
        );
        let r = radiance_of(&m);
        assert!(r.x > 1.0, "emission was clamped to {r:?}");
        assert!((r - Vec3A::splat(8.0)).length() < 1e-4, "{r:?}");
    }

    /// Emission is the one pool that **sums**. The BSDF pools take a weighted
    /// mean because two diffuse leaves describe one surface shared between
    /// them; two emitters are twice the light. This is the assertion that
    /// catches someone "fixing" the emission pool to match its neighbours.
    #[test]
    fn emission_terms_add_rather_than_average() {
        let m = emissive(
            r#"<uniform_edf name="a" type="EDF">
                 <input name="color" type="color3" value="1, 1, 1" />
               </uniform_edf>
               <uniform_edf name="b" type="EDF">
                 <input name="color" type="color3" value="1, 1, 1" />
               </uniform_edf>
               <add name="sum" type="EDF">
                 <input name="in1" type="EDF" nodename="a" />
                 <input name="in2" type="EDF" nodename="b" />
               </add>"#,
            "sum",
        );
        assert!((radiance_of(&m) - Vec3A::splat(2.0)).length() < 1e-5);
    }

    #[test]
    fn a_mixed_edf_partitions_between_its_branches() {
        let m = emissive(
            r#"<uniform_edf name="a" type="EDF">
                 <input name="color" type="color3" value="4, 0, 0" />
               </uniform_edf>
               <uniform_edf name="b" type="EDF">
                 <input name="color" type="color3" value="0, 0, 8" />
               </uniform_edf>
               <mix name="m" type="EDF">
                 <input name="fg" type="EDF" nodename="a" />
                 <input name="bg" type="EDF" nodename="b" />
                 <input name="mix" type="float" value="0.25" />
               </mix>"#,
            "m",
        );
        // 0.25·(4,0,0) + 0.75·(0,0,8)
        let r = radiance_of(&m);
        assert!((r - Vec3A::new(1.0, 0.0, 6.0)).length() < 1e-4, "{r:?}");
    }

    /// The factorisation is by peak channel, so `emission_color` is always a
    /// chromaticity and the range lives in the scalar. Factoring by Rec.709
    /// luminance instead would send a saturated emitter's *colour* above one —
    /// (0, 0, 8) has luminance 0.43 and would store (0, 0, 18.6).
    #[test]
    fn the_split_puts_the_range_in_the_scalar() {
        let m = emissive(
            r#"<uniform_edf name="e" type="EDF">
                 <input name="color" type="color3" value="0, 0, 8" />
               </uniform_edf>"#,
            "e",
        );
        assert!(near(m.emission_luminance, 8.0), "{}", m.emission_luminance);
        assert!(near(m.emission_color.max_element(), 1.0));
        assert!(m.emission_color.max_element() <= 1.0);
        assert!((radiance_of(&m) - Vec3A::new(0.0, 0.0, 8.0)).length() < 1e-4);
    }

    /// A `surface` with an `edf` and no `bsdf` flattens to no lobes, and the
    /// base-colour branches would otherwise leave it at OpenPBR's default grey
    /// at full weight: a pure emitter that is also a diffuse reflector.
    #[test]
    fn a_pure_edf_surface_does_not_also_reflect() {
        let m = emissive(
            r#"<uniform_edf name="e" type="EDF">
                 <input name="color" type="color3" value="2, 2, 2" />
               </uniform_edf>"#,
            "e",
        );
        assert!(near(m.base_weight, 0.0), "base_weight {}", m.base_weight);
        assert!(near(m.specular_weight, 0.0));
        assert!(near(m.coat_weight, 0.0));
    }

    /// An emitter that *does* carry a BSDF keeps it — the arm above must not
    /// fire whenever emission exists.
    #[test]
    fn an_emitter_with_a_bsdf_keeps_its_base() {
        let text = r#"<materialx>
          <oren_nayar_diffuse_bsdf name="d" type="BSDF">
            <input name="color" type="color3" value="0.8, 0.2, 0.2" />
          </oren_nayar_diffuse_bsdf>
          <uniform_edf name="e" type="EDF">
            <input name="color" type="color3" value="2, 2, 2" />
          </uniform_edf>
          <surface name="s" type="surfaceshader">
            <input name="bsdf" type="BSDF" nodename="d" />
            <input name="edf" type="EDF" nodename="e" />
          </surface>
        </materialx>"#;
        let m = reduced(text, "s");
        assert!(near(m.base_weight, 1.0));
        assert!((m.base_color - Vec3A::new(0.8, 0.2, 0.2)).length() < 1e-5);
        assert!((radiance_of(&m) - Vec3A::splat(2.0)).length() < 1e-5);
    }

    /// Guards the claim that every existing MaterialX render is untouched:
    /// with no EDF, emission comes through from `base` exactly as before.
    #[test]
    fn a_graph_with_no_edf_leaves_emission_at_the_base() {
        let text = r#"<materialx>
          <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
          <surface name="s" type="surfaceshader">
            <input name="bsdf" type="BSDF" nodename="d" />
          </surface>
        </materialx>"#;
        let m = reduced(text, "s");
        assert_eq!(m.emission_luminance, OpenPBR::default().emission_luminance);
        assert_eq!(m.emission_color, OpenPBR::default().emission_color);
    }

    /// MaterialX declares `ND_multiply_edfC` — a `color3` weight on an EDF —
    /// so the weight slot is not a scalar and must be applied per channel.
    #[test]
    fn a_colour_weight_multiplies_emission_per_channel() {
        let m = emissive(
            r#"<uniform_edf name="e" type="EDF">
                 <input name="color" type="color3" value="1, 1, 1" />
               </uniform_edf>
               <multiply name="mul" type="EDF">
                 <input name="in1" type="EDF" nodename="e" />
                 <input name="in2" type="color3" value="0.25, 0.5, 1.0" />
               </multiply>"#,
            "mul",
        );
        // Three distinct channels, so a broadcast cannot pass by accident.
        let r = radiance_of(&m);
        assert!(
            (r - Vec3A::new(0.25, 0.5, 1.0)).length() < 1e-5,
            "a colour weight reduced to {r:?}"
        );
    }

    /// The regression test. Reading the weight as lane 0 saw `0.0` here and
    /// dropped the term, so a green-blue emitter rendered **black** — and a
    /// weight of `(1, 0.5, 0.2)` would have rendered neutral at full strength.
    /// It also exercises `literal_zero`'s every-lane rule, which is what lets
    /// the branch survive flattening to be reduced at all.
    #[test]
    fn a_zero_red_weight_keeps_green_and_blue() {
        let m = emissive(
            r#"<uniform_edf name="e" type="EDF">
                 <input name="color" type="color3" value="1, 1, 1" />
               </uniform_edf>
               <multiply name="mul" type="EDF">
                 <input name="in1" type="EDF" nodename="e" />
                 <input name="in2" type="color3" value="0, 0.6, 0.9" />
               </multiply>"#,
            "mul",
        );
        let r = radiance_of(&m);
        assert!(r.y > 0.0 && r.z > 0.0, "the emitter went black: {r:?}");
        assert!(
            (r - Vec3A::new(0.0, 0.6, 0.9)).length() < 1e-5,
            "reduced to {r:?}"
        );
    }

    /// Both factors are colours, so neither may be broadcast over the other.
    #[test]
    fn a_colour_weight_and_a_colour_edf_multiply_componentwise() {
        let m = emissive(
            r#"<uniform_edf name="e" type="EDF">
                 <input name="color" type="color3" value="0.5, 1, 0.25" />
               </uniform_edf>
               <multiply name="mul" type="EDF">
                 <input name="in1" type="EDF" nodename="e" />
                 <input name="in2" type="color3" value="2, 0, 4" />
               </multiply>"#,
            "mul",
        );
        let r = radiance_of(&m);
        assert!((r - Vec3A::new(1.0, 0.0, 1.0)).length() < 1e-5, "{r:?}");
    }

    /// The broadcast guard: `Val::rgb` widens an arity-1 value, so a `float`
    /// weight still scales every channel and the scalar path is unchanged.
    #[test]
    fn a_float_weight_still_scales_every_channel() {
        let m = emissive(
            r#"<uniform_edf name="e" type="EDF">
                 <input name="color" type="color3" value="1, 0.5, 0.25" />
               </uniform_edf>
               <multiply name="mul" type="EDF">
                 <input name="in1" type="EDF" nodename="e" />
                 <input name="in2" type="float" value="4" />
               </multiply>"#,
            "mul",
        );
        let r = radiance_of(&m);
        assert!((r - Vec3A::new(4.0, 2.0, 1.0)).length() < 1e-5, "{r:?}");
    }

    /// Non-finite and negative channels are refused **per channel**, where the
    /// lobe loop drops the whole lobe. The difference is structural: emission
    /// sums, so a zeroed channel contaminates nothing, whereas a lobe's weight
    /// is a divisor. Sanitising each factor *before* the product is also what
    /// stops two negative channels multiplying into positive light.
    #[test]
    fn a_bad_channel_does_not_cost_the_others() {
        let (flat, mut slots) = evaluate(
            r#"<materialx>
                 <uniform_edf name="e" type="EDF">
                   <input name="color" type="color3" value="1, 1, 1" />
                 </uniform_edf>
                 <surface name="s" type="surfaceshader">
                   <input name="edf" type="EDF" nodename="e" />
                 </surface>
               </materialx>"#,
            "s",
        );
        // Poison the evaluated colour directly: no MaterialX node reliably
        // produces a NaN, and the guard is about what reaches `reduce`.
        let slot = flat.emission[0].color as usize;
        slots[slot] = Val::vec3(f32::NAN, 0.5, -2.0);
        let (m, _) = reduce(&flat, &slots, &OpenPBR::default());
        let r = radiance_of(&m);
        assert!(r.is_finite(), "a NaN channel escaped into radiance: {r:?}");
        assert_eq!(r.x, 0.0, "the NaN channel must contribute nothing");
        assert_eq!(r.z, 0.0, "the negative channel must contribute nothing");
        assert!((r.y - 0.5).abs() < 1e-5, "the good channel was lost: {r:?}");
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    /// The dielectric and conductor pools must each keep their own authored
    /// coverage through the reduction.
    ///
    /// `eval_specular` applies `base_metalness` to the metal half and
    /// `(1 - base_metalness) * specular_weight` to the dielectric half, so
    /// those two products *are* the coverages the render sees. While the
    /// dielectric's coverage lived in `specular_weight`, which scaled both
    /// halves, each pool multiplied the other: a conductor at 0.25 under a
    /// glaze at 0.75 rendered its metal at 0.25 x 0.75.
    #[test]
    fn unequal_dielectric_and_conductor_coverage_survive_the_reduction() {
        let text = format!(
            r#"<materialx>{DIFFUSE}
                 <dielectric_bsdf name="glaze" type="BSDF">
                   <input name="roughness" type="vector2" value="0.04, 0.04" />
                 </dielectric_bsdf>
                 {}
                 <conductor_bsdf name="c" type="BSDF">
                   <input name="roughness" type="float" value="0.01" />
                 </conductor_bsdf>
                 <mix name="m" type="BSDF">
                   <input name="fg" type="BSDF" nodename="c" />
                   <input name="bg" type="BSDF" nodename="db" />
                   <input name="mix" type="float" value="0.25" />
                 </mix>
               </materialx>"#,
            layer("db", "glaze", "d")
        );
        let m = reduced(&text, "m");

        // Authored: the conductor over a quarter of the surface, the glaze over
        // the diffuse across the other three quarters. Each must arrive intact.
        let metal_coverage = m.base_metalness;
        let diel_coverage = (1.0 - m.base_metalness) * m.specular_weight;
        assert!(
            near(metal_coverage, 0.25),
            "conductor coverage {metal_coverage}, authored 0.25"
        );
        assert!(
            near(diel_coverage, 0.75),
            "dielectric coverage {diel_coverage}, authored 0.75"
        );
    }

    /// A conductor mixed with a *bare* dielectric — nothing opaque under it —
    /// must keep both branches.
    ///
    /// `base_metalness` was the metal's share of the diffuse/metal/subsurface
    /// pools alone, so with no diffuse it pinned to 1 and `eval_specular`
    /// dropped the dielectric half outright. A dielectric base exists whenever
    /// a dielectric interface does, whether or not anything sits beneath it.
    #[test]
    fn a_conductor_mixed_with_a_bare_dielectric_keeps_both_branches() {
        let text = r#"<materialx>
            <dielectric_bsdf name="g" type="BSDF">
              <input name="roughness" type="vector2" value="0.04, 0.04" />
            </dielectric_bsdf>
            <conductor_bsdf name="c" type="BSDF">
              <input name="roughness" type="float" value="0.01" />
            </conductor_bsdf>
            <mix name="m" type="BSDF">
              <input name="fg" type="BSDF" nodename="c" />
              <input name="bg" type="BSDF" nodename="g" />
              <input name="mix" type="float" value="0.5" />
            </mix>
          </materialx>"#;
        let m = reduced(text, "m");
        assert!(
            m.base_metalness < 1.0,
            "metalness pinned to 1 with no diffuse, which zeroes the dielectric"
        );
        assert!(
            near(m.base_metalness, 0.5),
            "conductor coverage {}, authored 0.5",
            m.base_metalness
        );
        let diel_coverage = (1.0 - m.base_metalness) * m.specular_weight;
        assert!(
            near(diel_coverage, 0.5),
            "dielectric coverage {diel_coverage}, authored 0.5"
        );
    }

    /// A masked conductor must not pay its own coverage twice.
    ///
    /// `base_metalness` already carries how much of the surface is metal, and
    /// `eval_specular` scales the metal lobe by `specular_weight *
    /// base_metalness`. Setting `specular_weight = max(spec.w, metal.w)` there
    /// made a `mix`-masked conductor render at m² of its energy rather than m.
    #[test]
    fn a_masked_conductor_does_not_halve_its_own_weight() {
        let text = r#"<materialx>
            <oren_nayar_diffuse_bsdf name="d" type="BSDF">
              <input name="color" type="color3" value="0.6, 0.1, 0.1" />
            </oren_nayar_diffuse_bsdf>
            <conductor_bsdf name="c" type="BSDF">
              <input name="roughness" type="float" value="0.01" />
            </conductor_bsdf>
            <mix name="m" type="BSDF">
              <input name="fg" type="BSDF" nodename="c" />
              <input name="bg" type="BSDF" nodename="d" />
              <input name="mix" type="float" value="0.5" />
            </mix>
          </materialx>"#;
        let m = reduced(text, "m");
        assert!(
            near(m.base_metalness, 0.5),
            "the conductor's coverage was counted twice: {}",
            m.base_metalness
        );
        // No dielectric leaf in this graph, so no dielectric interface. The
        // metal half reads `base_metalness` alone, so there is nothing to keep
        // alive with a synthetic weight here.
        assert_eq!(
            m.specular_weight, 0.0,
            "a graph with no dielectric grew a specular interface"
        );
    }

    // --- MaterialX roughness is a GGX alpha ---------------------------------

    /// MaterialX's physically-based BSDF nodes take the microfacet **alpha**,
    /// not an artist-facing roughness — that is what `roughness_anisotropy`
    /// exists to produce, and why the DPEL teapot's `.mtlx` puts `power` nodes
    /// named `desquare_roughness_*` in front of its conductors. crust squares
    /// its own `*_roughness` on the way into GGX, so the reduction has to
    /// un-square once or the value is squared twice and every MaterialX
    /// surface renders far sharper than authored.
    #[test]
    fn a_materialx_roughness_is_an_alpha_and_is_unsquared() {
        let text = format!(
            r#"<materialx>{DIFFUSE}
                 <dielectric_bsdf name="g" type="BSDF">
                   <input name="roughness" type="vector2" value="0.25, 0.25" />
                 </dielectric_bsdf>
                 {}</materialx>"#,
            layer("L", "g", "d")
        );
        let m = reduced(&text, "L");
        assert!(
            near(m.specular_roughness, 0.5),
            "alpha 0.25 should reduce to roughness 0.5, got {}",
            m.specular_roughness
        );
    }

    /// The pool averages in alpha and un-squares once at the end, rather than
    /// un-squaring each lobe as it lands. This is the case that tells the two
    /// apart: `√` is concave, so `mean(√α) < √(mean α)` whenever two lobes of
    /// different roughness both contribute. Per-lobe conversion would give
    /// `(0.2 + 0.8)/2 = 0.5` here; pooling in alpha gives `√0.34`.
    #[test]
    fn the_alpha_pool_averages_before_it_unsquares() {
        let text = format!(
            r#"<materialx>{DIFFUSE}
                 <dielectric_bsdf name="a" type="BSDF">
                   <input name="roughness" type="vector2" value="0.04, 0.04" />
                 </dielectric_bsdf>
                 <dielectric_bsdf name="b" type="BSDF">
                   <input name="roughness" type="vector2" value="0.64, 0.64" />
                 </dielectric_bsdf>
                 <mix name="g" type="BSDF">
                   <input name="fg" type="BSDF" nodename="a" />
                   <input name="bg" type="BSDF" nodename="b" />
                   <input name="mix" type="float" value="0.5" />
                 </mix>
                 {}</materialx>"#,
            layer("L", "g", "d")
        );
        let m = reduced(&text, "L");
        assert!(
            near(m.specular_roughness, (0.34f32).sqrt()),
            "expected sqrt(mean alpha) = {}, got {}",
            (0.34f32).sqrt(),
            m.specular_roughness
        );
        assert!(
            !near(m.specular_roughness, 0.5),
            "the pool converted per lobe instead of after the mean"
        );
    }

    /// Only the GGX lobes are in alpha. `oren_nayar_diffuse_bsdf`'s roughness
    /// is its Oren-Nayar sigma and `sheen_bsdf`'s drives the Charlie NDF
    /// directly, exactly as crust's own parameters do — converting either
    /// would be a second bug in the opposite direction.
    #[test]
    fn an_oren_nayar_sigma_and_a_sheen_roughness_are_not_alphas() {
        let text = r#"<materialx>
            <oren_nayar_diffuse_bsdf name="d" type="BSDF">
              <input name="roughness" type="float" value="0.2" />
            </oren_nayar_diffuse_bsdf>
            <sheen_bsdf name="s" type="BSDF">
              <input name="roughness" type="float" value="0.5" />
            </sheen_bsdf>
            <layer name="L" type="BSDF">
              <input name="top" type="BSDF" nodename="s" />
              <input name="base" type="BSDF" nodename="d" />
            </layer>
          </materialx>"#;
        let m = reduced(text, "L");
        assert!(
            near(m.base_diffuse_roughness, 0.2),
            "oren-nayar sigma {}",
            m.base_diffuse_roughness
        );
        assert!(
            near(m.fuzz_roughness, 0.5),
            "sheen roughness {}",
            m.fuzz_roughness
        );
    }

    #[test]
    fn a_two_roughness_stack_keeps_both() {
        // The teapot ceramic's shape. The single-pool reduction averaged the
        // two roughnesses into one lobe; now each keeps its own.
        let text = format!(
            "<materialx>{DIFFUSE}{SATIN}{CLEAR}{}{}</materialx>",
            layer("inner", "satin", "d"),
            layer("outer", "clear", "inner")
        );
        let m = reduced(&text, "outer");
        assert!(near(m.specular_weight, 1.0), "spec {}", m.specular_weight);
        // The authored numbers are GGX alphas; crust's roughness is perceptual.
        assert!(
            near(m.specular_roughness, (0.4f32).sqrt()),
            "spec rough {}",
            m.specular_roughness
        );
        assert!(near(m.specular_ior, 1.5), "spec ior {}", m.specular_ior);
        assert!(near(m.coat_weight, 1.0), "coat {}", m.coat_weight);
        assert!(
            near(m.coat_roughness, (0.02f32).sqrt()),
            "coat rough {}",
            m.coat_roughness
        );
        assert!(near(m.coat_ior, 1.6), "coat ior {}", m.coat_ior);
        assert!(near(m.base_metalness, 0.0), "metal {}", m.base_metalness);
        assert!(near(m.base_weight, 1.0), "base {}", m.base_weight);
    }

    #[test]
    fn a_varnish_over_a_conductor_is_a_coat_over_metal() {
        let text = format!(
            r#"<materialx>{CLEAR}
                 <conductor_bsdf name="c" type="BSDF">
                   <input name="roughness" type="float" value="0.1" />
                 </conductor_bsdf>
                 {}</materialx>"#,
            layer("L", "clear", "c")
        );
        let m = reduced(&text, "L");
        assert!(near(m.base_metalness, 1.0), "metal {}", m.base_metalness);
        // The varnish is the *coat*; nothing is left as a base dielectric, and
        // the metal no longer needs `specular_weight` held at 1 to survive.
        assert_eq!(
            m.specular_weight, 0.0,
            "the varnish was counted as a base specular as well as a coat"
        );
        assert!(
            near(m.specular_roughness, (0.1f32).sqrt()),
            "spec rough {}",
            m.specular_roughness
        );
        assert!(near(m.coat_weight, 1.0), "coat {}", m.coat_weight);
        assert!(
            near(m.coat_roughness, (0.02f32).sqrt()),
            "coat rough {}",
            m.coat_roughness
        );
    }

    #[test]
    fn a_glaze_over_a_pruned_dummy_stays_the_base_specular() {
        // The transmission dummy sits in the base as a mix's null branch; it
        // must not turn the glaze above it into a coat over nothing.
        let text = format!(
            r#"<materialx>{DIFFUSE}{CLEAR}
                 <dielectric_bsdf name="dummy" type="BSDF">
                   <input name="weight" type="float" value="0" />
                 </dielectric_bsdf>
                 <mix name="m" type="BSDF">
                   <input name="fg" type="BSDF" nodename="d" />
                   <input name="bg" type="BSDF" nodename="dummy" />
                   <input name="mix" type="float" value="0.5" />
                 </mix>
                 {}</materialx>"#,
            layer("L", "clear", "m")
        );
        let m = reduced(&text, "L");
        assert_eq!(m.coat_weight, 0.0, "the dummy promoted the glaze");
        assert!(near(m.specular_weight, 1.0), "spec {}", m.specular_weight);
        assert!(
            near(m.specular_roughness, (0.02f32).sqrt()),
            "spec rough {}",
            m.specular_roughness
        );
    }

    #[test]
    fn a_glaze_over_a_zero_multiplied_dummy_stays_the_base_specular() {
        // The other way to author a null branch: `multiply(BSDF, 0)` rather
        // than a leaf `weight = 0`. It has to prune for the same reason — the
        // lobe beneath it can never contribute, but left in place it counts as
        // a specular interface and promotes the glaze above it to a coat over
        // a base that carries no specular at all.
        let text = format!(
            r#"<materialx>{DIFFUSE}{CLEAR}
                 <dielectric_bsdf name="dummy" type="BSDF" />
                 <multiply name="off" type="BSDF">
                   <input name="in1" type="BSDF" nodename="dummy" />
                   <input name="in2" type="float" value="0" />
                 </multiply>
                 {}
                 {}</materialx>"#,
            layer("inner", "off", "d"),
            layer("L", "clear", "inner")
        );
        let m = reduced(&text, "L");
        assert_eq!(
            m.coat_weight, 0.0,
            "the zero-multiplied dummy promoted the glaze"
        );
        assert!(near(m.specular_weight, 1.0), "spec {}", m.specular_weight);
        assert!(
            near(m.specular_roughness, (0.02f32).sqrt()),
            "the glaze did not land on the base specular: {}",
            m.specular_roughness
        );
    }

    #[test]
    fn a_glaze_over_a_diffuse_alone_has_no_coat() {
        let text = format!(
            "<materialx>{DIFFUSE}{CLEAR}{}</materialx>",
            layer("L", "clear", "d")
        );
        let m = reduced(&text, "L");
        assert_eq!(m.coat_weight, 0.0);
        assert!(near(m.specular_weight, 1.0));
        assert!(near(m.specular_roughness, (0.02f32).sqrt()));
    }

    #[test]
    fn a_bare_two_dielectric_shell_has_no_base() {
        let text = format!(
            "<materialx>{SATIN}{CLEAR}{}</materialx>",
            layer("L", "clear", "satin")
        );
        let m = reduced(&text, "L");
        assert_eq!(
            m.base_weight, 0.0,
            "an unlit grey base was left under the shell"
        );
        assert!(near(m.coat_weight, 1.0), "coat {}", m.coat_weight);
        assert!(near(m.specular_weight, 1.0), "spec {}", m.specular_weight);
    }
}
