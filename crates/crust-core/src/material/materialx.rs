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
use crust_mtlx::{Lobe, LobeKind, Program, ShadeCtx, TextureLoader, Val, reflectivity_from_ior};
use glam::Vec3A;

pub use crust_mtlx::MtlxError;

/// A surface whose parameters come from a MaterialX graph, re-evaluated at
/// every shading point.
///
/// Holds the compiled pattern [`Program`] and the flattened [`Lobe`] list, and
/// implements [`Material`] by running both and delegating the actual BSDF to
/// the [`OpenPBR`] they reduce to. Delegating rather than reimplementing is
/// the whole point: sampling, evaluation, MIS densities, the coat and fuzz
/// layering and the energy compensation all stay in one place, and a MaterialX
/// surface is unbiased by exactly the same argument as an authored one.
pub struct MtlxMaterial {
    program: Program,
    lobes: Vec<Lobe>,
    /// Defaults for everything no lobe speaks to, and the fallback when the
    /// graph yields nothing at all.
    base: OpenPBR,
    /// Name of the material node, for diagnostics.
    pub name: String,
}

impl std::fmt::Debug for MtlxMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MtlxMaterial({}, {} ops, {} lobes)",
            self.name,
            self.program.ops.len(),
            self.lobes.len()
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
    /// The parameter set is a temporary rather than something cached on the
    /// hit, because [`Material`]'s methods take `&self` and a `&HitRecord`
    /// with nowhere to stash it. That means a vertex evaluated for NEE *and*
    /// for a BSDF sample runs the graph twice — correct, and the obvious thing
    /// to memoise if MaterialX surfaces ever dominate a render.
    fn shade<R>(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        f: impl FnOnce(&OpenPBR, &HitRecord) -> R,
    ) -> R {
        let ctx = ShadeCtx {
            uv: if rec.has_uv { rec.uv } else { (0.0, 0.0) },
            normal: rec.normal,
            tangent: rec.tangent,
            view: r_in.direction(),
            position: rec.p,
        };
        SLOTS.with(|cell| {
            let mut slots = cell.borrow_mut();
            self.program.eval(&ctx, &mut slots);
            let (params, normal) = reduce(&self.lobes, &slots, &self.base);
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
            f(&params, &rec)
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

    fn uses_uv(&self) -> bool {
        // Unconditionally true rather than "does the program hold a texture":
        // a graph with no `image` node can still carry a `normalmap` over a
        // constant, or a `texcoord`-driven procedural, and both need the
        // chart. The cost of an unnecessary table is bounded; shading a
        // textured surface at (0, 0) everywhere is not obviously wrong on
        // screen, which is the failure worth avoiding.
        true
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
    let c = crust_mtlx::compile(path, material_node, load_texture)?;
    let material = MtlxMaterial {
        program: c.program,
        lobes: c.lobes,
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
        self.shade(r_in, rec, |params, _| params.clone())
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
/// `base` is the material's authored defaults, so anything no lobe speaks to
/// (emission, transmission, thin-film) keeps a sensible value instead of zero.
/// `coat_darkening` is one of those — no lobe speaks to it — and `load()`
/// supplies it as 0 rather than OpenPBR's 1.0, because MaterialX's `layer` is
/// single-scattering and models no coat-underside bounce series. See the
/// comment there before re-deriving it here.
pub fn reduce(lobes: &[Lobe], slots: &[Val], base: &OpenPBR) -> (OpenPBR, Option<Vec3A>) {
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

    // Base colour is a diffuse/metal blend, and metalness is their ratio:
    // OpenPBR mixes its metal and dielectric-base lobes by `base_metalness`
    // off one `base_color`, which is exactly the shape of these two pools.
    let base_total = diffuse.w + metal.w + sss.w;
    let mut m = base.clone();
    if base_total > 1e-5 {
        m.base_metalness = (metal.w / base_total).clamp(0.0, 1.0);
        m.base_color = (diffuse.color + metal.color + sss.color) / base_total;
        m.base_weight = base_total.clamp(0.0, 1.0);
    } else if spec.w > 1e-5 || coat.w > 1e-5 {
        // A purely specular stack (a bare glass shell): keep the specular
        // interfaces, but do not leave an unlit grey base under them.
        m.base_weight = 0.0;
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
    // Asymmetric on purpose, and the asymmetry is the point: the conductor's
    // coverage is *already* carried by `base_metalness` above, while the
    // dielectric's is carried by nothing else. `eval_specular` scales the metal
    // lobe by `specular_weight * base_metalness`, so taking `max(spec.w,
    // metal.w)` here made a mask-driven conductor render at m² of its energy
    // instead of m — on the DPEL lion's gold, exactly the mask value too dark.
    // So: the dielectric's coverage when there is a dielectric interface,
    // otherwise full weight and let `base_metalness` do the masking.
    m.specular_weight = if spec.w > 1e-5 {
        spec.w.clamp(0.0, 1.0)
    } else if metal.w > 1e-5 {
        1.0
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
    fn evaluate(text: &str, root: &str) -> (Vec<Lobe>, Vec<Val>) {
        let doc = Doc::parse(text).unwrap();
        let loader = |_: &str, _: Option<&str>| None;
        let mut c = Compiler::new(&doc, &loader);
        let one = c.constant(Val::ONE);
        let node = doc.find("", root).unwrap().clone();
        let mut lobes = Vec::new();
        flatten(&mut c, &node, one, 0, &mut lobes);
        let mut slots = Vec::new();
        c.program.eval(
            &ShadeCtx {
                uv: (0.0, 0.0),
                normal: Vec3A::Z,
                tangent: Vec3A::X,
                view: -Vec3A::Z,
                position: Vec3A::ZERO,
            },
            &mut slots,
        );
        (lobes, slots)
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
        let (lobes, slots) = evaluate(text, "m");
        let (m, _) = reduce(&lobes, &slots, &OpenPBR::default());
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
        let (lobes, slots) = evaluate(text, "m");
        let (m, _) = reduce(&lobes, &slots, &OpenPBR::default());
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
        let (lobes, slots) = evaluate(text, root);
        reduce(&lobes, &slots, &OpenPBR::default()).0
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
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
        assert!(near(m.base_metalness, 0.5), "metal {}", m.base_metalness);
        assert!(
            near(m.specular_weight, 1.0),
            "the conductor's coverage was counted twice: {}",
            m.specular_weight
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
        assert!(near(m.specular_weight, 1.0), "spec {}", m.specular_weight);
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
