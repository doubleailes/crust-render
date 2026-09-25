//! `UsdPreviewSurface` with texture-driven inputs.
//!
//! The standard USD preview network is a `UsdPreviewSurface` whose inputs
//! connect to `UsdUVTexture` nodes, each reading a file at the coordinates a
//! `UsdPrimvarReader_float2` supplies. The importer resolves that network once
//! (`scene/usd_import.rs`) into a list of [`UvInput`]s, one per connected
//! surface input, and this material evaluates them per shading point, writing
//! each result over the matching field of an otherwise-constant [`OpenPBR`]
//! and delegating the BSDF to it, as [`crate::MtlxMaterial`] does.
//!
//! A surface with **no** texture connection is never one of these. The
//! importer keeps building the plain `OpenPBR` it always did, so an untextured
//! stage pays nothing for this material existing and renders exactly as it did
//! before it did.
//!
//! What the node set specifies and this reproduces:
//!
//! - **`UsdUVTexture`**: `file` (UDIM sets included, addressed by the host),
//!   `sourceColorSpace` (resolved at load, see [`crate::ColorSpace::from_usd`]),
//!   `wrapS`/`wrapT`, `scale`, `bias`, `fallback`, and the `r`/`g`/`b`/`a`/`rgb`
//!   output the surface connects to.
//! - **`UsdPrimvarReader_float2`**: crust reads one chart (`primvars:st` and
//!   its fallbacks), so a reader naming any other primvar is warned about by
//!   the importer and shaded from that chart.
//!
//! What it does not: `UsdTransform2d` (warned about, identity chart),
//! `occlusion` and `displacement` (no counterpart in the integrator), and a
//! texture's alpha (the host samplers return opaque RGB, so `outputs:a` reads
//! 1.0 before `scale`/`bias`).

use crate::PathSampler;
use crate::hittable::HitRecord;
use crate::material::{Material, OpenPBR, ScatterSample};
use crate::ray::Ray;
use crate::texture::TextureRef;
use glam::Vec3A;

/// Which `UsdUVTexture` output a surface input connects to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TexOutput {
    R,
    G,
    B,
    A,
    Rgb,
}

impl TexOutput {
    /// From the connected output's base name (`rgb`, `r`, ...).
    pub fn from_name(name: &str) -> Option<TexOutput> {
        Some(match name {
            "r" => TexOutput::R,
            "g" => TexOutput::G,
            "b" => TexOutput::B,
            "a" => TexOutput::A,
            "rgb" => TexOutput::Rgb,
            _ => return None,
        })
    }
}

/// `UsdUVTexture`'s `wrapS` / `wrapT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrap {
    /// `repeat`, and `useMetadata` — the schema fallback, which asks for a
    /// wrap mode stored in the file. No format crust reads stores one, so it
    /// takes the node set's documented default, `repeat`.
    Repeat,
    Clamp,
    Mirror,
    /// Outside `[0, 1]` the texel is zero, before `scale`/`bias`.
    Black,
}

impl Wrap {
    pub fn from_token(token: Option<&str>) -> Wrap {
        match token {
            Some("clamp") => Wrap::Clamp,
            Some("mirror") => Wrap::Mirror,
            Some("black") => Wrap::Black,
            _ => Wrap::Repeat,
        }
    }

    /// Maps one coordinate into the texture's `[0, 1)` domain, or `None` where
    /// a `black` border applies. The host wraps periodically on its own, so
    /// `repeat` is the identity and the others only have to land inside a
    /// period.
    #[inline]
    fn apply(self, x: f32) -> Option<f32> {
        // The largest value below 1.0: the host wraps `x - floor(x)`, so 1.0
        // itself would come back as 0.0 — the opposite edge.
        const TOP: f32 = 1.0 - f32::EPSILON;
        match self {
            Wrap::Repeat => Some(x),
            Wrap::Clamp => Some(x.clamp(0.0, TOP)),
            Wrap::Mirror => {
                let t = x.rem_euclid(2.0);
                Some(if t > 1.0 { 2.0 - t } else { t }.min(TOP))
            }
            Wrap::Black => (0.0..=1.0).contains(&x).then_some(x.min(TOP)),
        }
    }
}

/// One `UsdUVTexture` as a surface input sees it.
#[derive(Clone, Debug)]
pub struct UvInput {
    /// `None` when the host could not load the file (or `CRUST_TEX=0`
    /// declined it): the input then reads `fallback`.
    pub tex: Option<TextureRef>,
    pub output: TexOutput,
    pub scale: [f32; 4],
    pub bias: [f32; 4],
    /// Returned **unscaled** when there is no texture, as the node set
    /// specifies. The importer fills it with the texture's authored
    /// `fallback`, or failing that with the surface input's own constant, so
    /// a declined texture renders on the surface's constants rather than
    /// black.
    pub fallback: [f32; 4],
    pub wrap: [Wrap; 2],
    /// The file carries a `<UDIM>` / `<UVTILE>` token. Tile addressing is the
    /// host's, so wrap modes are not applied to such a texture — they would
    /// fold every tile back onto the first.
    pub tiled: bool,
}

impl UvInput {
    /// The four output channels at a hit, `scale`/`bias` applied.
    #[inline]
    fn sample(&self, rec: &HitRecord) -> [f32; 4] {
        let Some(tex) = &self.tex else {
            return self.fallback;
        };
        let (u, v) = if rec.has_uv { rec.uv } else { (0.0, 0.0) };
        let texel = if self.tiled {
            tex.eval(u, v, rec.uv_width)
        } else {
            match (self.wrap[0].apply(u), self.wrap[1].apply(v)) {
                (Some(u), Some(v)) => tex.eval(u, v, rec.uv_width),
                _ => [0.0; 4],
            }
        };
        std::array::from_fn(|k| texel[k] * self.scale[k] + self.bias[k])
    }

    /// The connected output as a float input reads it. An `rgb` output
    /// feeding a float input is a type mismatch; its red channel is read,
    /// which is what Hydra does.
    #[inline]
    fn scalar(&self, s: [f32; 4]) -> f32 {
        match self.output {
            TexOutput::R | TexOutput::Rgb => s[0],
            TexOutput::G => s[1],
            TexOutput::B => s[2],
            TexOutput::A => s[3],
        }
    }

    /// The connected output as a colour input reads it; a single channel is
    /// broadcast.
    #[inline]
    fn color(&self, s: [f32; 4]) -> Vec3A {
        match self.output {
            TexOutput::Rgb => Vec3A::new(s[0], s[1], s[2]),
            _ => Vec3A::splat(self.scalar(s)),
        }
    }
}

/// The `UsdPreviewSurface` input a texture drives, and the `OpenPBR` field it
/// lands on — the same mapping `preview_surface_openpbr` uses for constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    DiffuseColor,
    EmissiveColor,
    Metallic,
    Roughness,
    Opacity,
    Ior,
    Clearcoat,
    ClearcoatRoughness,
}

impl Target {
    /// The surface input's base name, as authored (`inputs:<name>`).
    pub fn input_name(self) -> &'static str {
        match self {
            Target::DiffuseColor => "diffuseColor",
            Target::EmissiveColor => "emissiveColor",
            Target::Metallic => "metallic",
            Target::Roughness => "roughness",
            Target::Opacity => "opacity",
            Target::Ior => "ior",
            Target::Clearcoat => "clearcoat",
            Target::ClearcoatRoughness => "clearcoatRoughness",
        }
    }

    /// Writes one sampled input over its field. Clamped to each field's
    /// meaningful range, since a texture's `scale`/`bias` can push a value
    /// anywhere; emission alone is unbounded above, because a radiance above
    /// 1.0 is a bright light rather than an authoring error.
    fn apply(self, input: &UvInput, s: [f32; 4], o: &mut OpenPBR) {
        let unit = |x: f32| {
            if x.is_finite() {
                x.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        let colour = |c: Vec3A| {
            if c.is_finite() {
                c.max(Vec3A::ZERO)
            } else {
                Vec3A::ZERO
            }
        };
        match self {
            Target::DiffuseColor => o.base_color = colour(input.color(s)).min(Vec3A::ONE),
            Target::EmissiveColor => o.emission_color = colour(input.color(s)),
            Target::Metallic => o.base_metalness = unit(input.scalar(s)),
            Target::Roughness => o.specular_roughness = unit(input.scalar(s)),
            Target::Opacity => o.geometry_opacity = unit(input.scalar(s)),
            Target::Ior => {
                let ior = input.scalar(s);
                if ior.is_finite() && ior > 0.0 {
                    o.specular_ior = ior;
                }
            }
            Target::Clearcoat => o.coat_weight = unit(input.scalar(s)),
            Target::ClearcoatRoughness => o.coat_roughness = unit(input.scalar(s)),
        }
    }
}

/// A `UsdPreviewSurface` with at least one texture-driven input.
pub struct PreviewSurface {
    /// Every constant input, already applied — exactly the `OpenPBR` an
    /// untextured surface would be.
    base: OpenPBR,
    inputs: Vec<(Target, UvInput)>,
    /// A tangent-space normal, already in `[-1, 1]` after its texture's
    /// `scale`/`bias` (that is how the node set decodes a normal map).
    normal: Option<UvInput>,
    /// Whether emission varies per point. Decides whether the hit-free
    /// `emitted()` can still answer.
    emission_textured: bool,
    /// The primvar the network's `UsdPrimvarReader_float2` names, when it is
    /// not `st` (see [`Material::uv_primvar`]).
    uv_primvar: Option<String>,
    pub name: String,
}

impl std::fmt::Debug for PreviewSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PreviewSurface({}, {} textured input(s){})",
            self.name,
            self.inputs.len(),
            if self.normal.is_some() {
                " + normal"
            } else {
                ""
            }
        )
    }
}

impl PreviewSurface {
    /// `base` carries the surface's constants; `inputs` and `normal` the
    /// texture-driven ones. A textured `emissiveColor` switches emission on
    /// (`emission_luminance = 1`), as a non-black constant does.
    pub fn new(
        name: String,
        mut base: OpenPBR,
        inputs: Vec<(Target, UvInput)>,
        normal: Option<UvInput>,
    ) -> PreviewSurface {
        let emission_textured = inputs.iter().any(|(t, _)| *t == Target::EmissiveColor);
        if emission_textured {
            base.emission_luminance = 1.0;
        }
        PreviewSurface {
            base,
            inputs,
            normal,
            emission_textured,
            uv_primvar: None,
            name,
        }
    }

    /// Names the primvar the textures read, when it is not `st`.
    pub fn with_uv_primvar(mut self, primvar: Option<String>) -> PreviewSurface {
        self.uv_primvar = primvar.filter(|p| p != "st");
        self
    }

    /// The `OpenPBR` this surface reduces to at a hit, and the shading normal
    /// its normal map produces. Public for the tests and probes, which check
    /// the numbers rather than a render (see "Verified in numbers" in
    /// `CLAUDE.md`).
    pub fn probe(&self, rec: &HitRecord) -> (OpenPBR, Vec3A) {
        let mut params = self.base.clone();
        for (target, input) in &self.inputs {
            target.apply(input, input.sample(rec), &mut params);
        }
        (params, self.shading_normal(rec))
    }

    /// The normal-mapped shading normal, or `rec.normal` when there is none —
    /// under the same guard as the MaterialX adapter: a normal map may not
    /// flip the surface, or silhouettes shade black.
    fn shading_normal(&self, rec: &HitRecord) -> Vec3A {
        let Some(input) = self.normal.as_ref() else {
            return rec.normal;
        };
        // A texture that did not load reads its fallback, as every other
        // input does. The schema's (0, 0, 1) is the unperturbed normal, so
        // that case skips the frame rotation.
        if input.tex.is_none() && input.color(input.fallback) == Vec3A::Z {
            return rec.normal;
        }
        let v = input.color(input.sample(rec));
        if !v.is_finite() {
            return rec.normal;
        }
        let local = Vec3A::new(v.x, v.y, v.z.max(1e-4));
        let n = crust_mtlx::perturb_normal(local, rec.normal, rec.tangent);
        if n.dot(rec.normal) > 1e-3 {
            n
        } else {
            rec.normal
        }
    }

    fn shade<R>(&self, rec: &HitRecord, f: impl FnOnce(&OpenPBR, &HitRecord) -> R) -> R {
        let (params, normal) = self.probe(rec);
        let mut rec = *rec;
        rec.normal = normal;
        f(&params, &rec)
    }
}

impl Material for PreviewSurface {
    fn scatter_importance(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        sampler: PathSampler,
    ) -> Option<ScatterSample> {
        self.shade(rec, |m, rec| m.scatter_importance(r_in, rec, sampler))
    }

    fn eval(&self, r_in: &Ray, rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        self.shade(rec, |m, rec| m.eval(r_in, rec, wi))
    }

    fn make_ray(&self, rec: &HitRecord, wi: Vec3A) -> Ray {
        // Only decides whether the ray enters a medium, which no preview
        // input textures; the constants answer it.
        self.base.make_ray(rec, wi)
    }

    fn face_texture(&self) -> Option<&dyn crate::PtexTexture> {
        // A `surfaceMap` Ptex rides on the base, as on an untextured surface.
        // Where both are bound, the Ptex lookup inside `OpenPBR` runs after the
        // UV inputs and wins for `base_color`.
        self.base.face_texture()
    }

    fn uses_uv(&self) -> bool {
        true
    }

    fn eval_reads_textures(&self) -> bool {
        true
    }

    fn uv_primvar(&self) -> Option<&str> {
        self.uv_primvar.as_deref()
    }

    fn emitted(&self) -> Vec3A {
        // Hit-free, so it cannot see a textured emission; zero then, and the
        // hit-aware `emitted_at` answers instead. Nothing in the light list is
        // built from a preview surface, which is what makes that sound (see
        // `Material::emitted_at`).
        if self.emission_textured {
            Vec3A::ZERO
        } else {
            self.base.emitted()
        }
    }

    fn emitted_directional(&self, cos_theta_o: f32) -> Vec3A {
        if self.emission_textured {
            Vec3A::ZERO
        } else {
            self.base.emitted_directional(cos_theta_o)
        }
    }

    fn emitted_at(&self, _r_in: &Ray, rec: &HitRecord, cos_theta_o: f32) -> Vec3A {
        // Asked of every surface hit: a surface that cannot emit must not
        // sample its textures to find that out.
        if !self.emission_textured && self.base.emission_luminance <= 0.0 {
            return Vec3A::ZERO;
        }
        self.probe(rec).0.emitted_directional(cos_theta_o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Flat([f32; 4]);
    impl crate::Texture2D for Flat {
        fn eval(&self, _: f32, _: f32, _: f32) -> [f32; 4] {
            self.0
        }
    }

    /// Returns `u` in every channel, to watch the wrap modes.
    struct Ramp;
    impl crate::Texture2D for Ramp {
        fn eval(&self, u: f32, _: f32, _: f32) -> [f32; 4] {
            [u, u, u, 1.0]
        }
    }

    fn input(tex: Option<TextureRef>, output: TexOutput) -> UvInput {
        UvInput {
            tex,
            output,
            scale: [1.0; 4],
            bias: [0.0; 4],
            fallback: [0.0, 0.0, 0.0, 1.0],
            wrap: [Wrap::Repeat; 2],
            tiled: false,
        }
    }

    fn hit(u: f32) -> HitRecord {
        HitRecord {
            normal: Vec3A::Z,
            tangent: Vec3A::X,
            uv: (u, 0.5),
            has_uv: true,
            ..HitRecord::default()
        }
    }

    fn tex(t: impl crate::Texture2D + 'static) -> Option<TextureRef> {
        Some(TextureRef(std::sync::Arc::new(t)))
    }

    #[test]
    fn scale_and_bias_apply_to_texels_but_not_to_the_fallback() {
        let mut i = input(tex(Flat([0.5, 0.25, 1.0, 1.0])), TexOutput::Rgb);
        i.scale = [2.0; 4];
        i.bias = [-1.0; 4];
        assert_eq!(i.sample(&hit(0.3)), [0.0, -0.5, 1.0, 1.0]);
        i.tex = None;
        i.fallback = [0.0, 1.0, 0.0, 1.0];
        assert_eq!(i.sample(&hit(0.3)), [0.0, 1.0, 0.0, 1.0]);
    }

    #[test]
    fn channels_select_and_broadcast() {
        let s = [0.1, 0.2, 0.3, 0.4];
        let g = input(None, TexOutput::G);
        assert_eq!(g.scalar(s), 0.2);
        assert_eq!(g.color(s), Vec3A::splat(0.2));
        let rgb = input(None, TexOutput::Rgb);
        assert_eq!(rgb.scalar(s), 0.1);
        assert_eq!(rgb.color(s), Vec3A::new(0.1, 0.2, 0.3));
    }

    #[test]
    fn wrap_modes_fold_into_the_unit_domain() {
        let mut i = input(tex(Ramp), TexOutput::R);
        let at = |i: &UvInput, u: f32| i.sample(&hit(u))[0];
        assert_eq!(at(&i, 1.25), 1.25, "repeat is the host's job");
        i.wrap[0] = Wrap::Clamp;
        assert_eq!(at(&i, -0.5), 0.0);
        assert!(at(&i, 1.5) < 1.0 && at(&i, 1.5) > 0.999);
        i.wrap[0] = Wrap::Mirror;
        assert!((at(&i, 1.25) - 0.75).abs() < 1e-6);
        assert!((at(&i, -0.25) - 0.25).abs() < 1e-6);
        i.wrap[0] = Wrap::Black;
        assert_eq!(at(&i, 1.5), 0.0);
        assert_eq!(at(&i, 0.5), 0.5);
        assert_eq!(at(&i, 0.0), 0.0);
        // The host wraps 1.0 to 0.0, so the inclusive edge must be pulled
        // inside the domain or it samples the opposite side.
        assert!(at(&i, 1.0) < 1.0 && at(&i, 1.0) > 0.999);
    }

    #[test]
    fn a_tile_set_is_never_wrapped() {
        let mut i = input(tex(Ramp), TexOutput::R);
        i.wrap = [Wrap::Clamp; 2];
        i.tiled = true;
        assert_eq!(i.sample(&hit(3.4))[0], 3.4);
    }

    #[test]
    fn textured_inputs_land_on_their_fields() {
        let base = OpenPBR::default();
        let m = PreviewSurface::new(
            "t".into(),
            base,
            vec![
                (
                    Target::DiffuseColor,
                    input(tex(Flat([0.2, 0.4, 0.6, 1.0])), TexOutput::Rgb),
                ),
                (
                    Target::Roughness,
                    input(tex(Flat([0.7, 0.0, 0.0, 1.0])), TexOutput::R),
                ),
                (
                    Target::Metallic,
                    input(tex(Flat([0.0, 0.0, 0.9, 1.0])), TexOutput::B),
                ),
            ],
            None,
        );
        let (p, n) = m.probe(&hit(0.5));
        assert_eq!(p.base_color, Vec3A::new(0.2, 0.4, 0.6));
        assert_eq!(p.specular_roughness, 0.7);
        assert_eq!(p.base_metalness, 0.9);
        assert_eq!(n, Vec3A::Z, "no normal map, no perturbation");
        assert_eq!(m.emitted(), Vec3A::ZERO);
    }

    #[test]
    fn a_flat_normal_map_is_the_identity_and_a_tilted_one_tilts() {
        let mut nm = input(tex(Flat([0.5, 0.5, 1.0, 1.0])), TexOutput::Rgb);
        nm.scale = [2.0, 2.0, 2.0, 1.0];
        nm.bias = [-1.0, -1.0, -1.0, 0.0];
        let m = PreviewSurface::new("n".into(), OpenPBR::default(), vec![], Some(nm.clone()));
        assert!((m.probe(&hit(0.5)).1 - Vec3A::Z).length() < 1e-6);

        nm.tex = tex(Flat([1.0, 0.5, 1.0, 1.0]));
        let m = PreviewSurface::new("n".into(), OpenPBR::default(), vec![], Some(nm));
        let n = m.probe(&hit(0.5)).1;
        assert!(n.x > 0.5 && n.z > 0.5, "tilts toward +tangent: {n}");
        assert!((n.length() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_normal_map_that_did_not_load_reads_its_fallback() {
        let mut nm = input(None, TexOutput::Rgb);
        nm.fallback = [0.0, 0.0, 1.0, 1.0];
        let m = PreviewSurface::new("n".into(), OpenPBR::default(), vec![], Some(nm.clone()));
        assert_eq!(m.probe(&hit(0.5)).1, Vec3A::Z, "neutral fallback");

        nm.fallback = [0.6, 0.0, 0.8, 1.0];
        let m = PreviewSurface::new("n".into(), OpenPBR::default(), vec![], Some(nm));
        let n = m.probe(&hit(0.5)).1;
        assert!(n.x > 0.5 && n.z > 0.5, "authored fallback tilts: {n}");
    }

    #[test]
    fn textured_emission_answers_only_at_a_hit() {
        let m = PreviewSurface::new(
            "e".into(),
            OpenPBR::default(),
            vec![(
                Target::EmissiveColor,
                input(tex(Flat([4.0, 2.0, 1.0, 1.0])), TexOutput::Rgb),
            )],
            None,
        );
        assert_eq!(m.emitted(), Vec3A::ZERO);
        let ray = Ray::new(Vec3A::new(0.0, 0.0, 1.0), -Vec3A::Z);
        let e = m.emitted_at(&ray, &hit(0.5), 1.0);
        assert!(e.x > 3.0 && e.x > e.y && e.y > e.z, "{e}");
    }
}
