//! `Material::resolve` must answer exactly what per-query shading would, for
//! every material: the integrator resolves once per vertex and routes every
//! query through the result ([`ShadingPoint`]), so a resolution that drifted
//! from the material's own methods would change the image while every
//! material's own tests still passed. Compared bit for bit, over every kind
//! of material the renderer has.

use std::path::PathBuf;
use std::sync::Arc;

use crust_core::preview_surface::{Target, TexOutput, UvInput, Wrap};
use crust_core::{
    Emissive, HitRecord, Material, OpenPBR, PathSampler, PreviewSurface, PtexRef, PtexTexture, Ray,
    ShadingPoint, Texture2D, TextureRef, Vec3A, materialx,
};

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

/// A Ptex whose colour varies with face and position, so a lookup that ran
/// at the wrong place, or not at all, shows.
struct Gradient;
impl PtexTexture for Gradient {
    fn eval(&self, face_id: u32, u: f32, v: f32, _: f32) -> Vec3A {
        Vec3A::new(u, v, 0.1 * face_id as f32)
    }
    fn num_faces(&self) -> usize {
        16
    }
}

/// A UV texture that varies across the chart.
struct Ramp;
impl Texture2D for Ramp {
    fn eval(&self, u: f32, v: f32, _: f32) -> [f32; 4] {
        [u, v, 0.5 * (u + v), 1.0]
    }
}

fn uv_input(output: TexOutput) -> UvInput {
    UvInput {
        tex: Some(TextureRef(Arc::new(Ramp))),
        output,
        scale: [1.0; 4],
        bias: [0.0; 4],
        fallback: [0.0, 0.0, 0.0, 1.0],
        wrap: [Wrap::Repeat; 2],
        tiled: false,
    }
}

fn hit() -> HitRecord {
    HitRecord {
        p: Vec3A::new(0.1, -0.2, 0.0),
        normal: Vec3A::Z,
        tangent: Vec3A::X,
        t: 1.0,
        front_face: true,
        face: Some(crust_core::FaceHit {
            id: 3,
            uv: (0.3, 0.7),
        }),
        uv: Some((0.4, 0.6)),
        ..HitRecord::default()
    }
}

fn bits(v: Vec3A) -> [u32; 3] {
    [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]
}

fn same_ray(a: &Ray, b: &Ray, what: &str) {
    assert_eq!(bits(a.origin()), bits(b.origin()), "{what}: origin");
    assert_eq!(
        bits(a.direction()),
        bits(b.direction()),
        "{what}: direction"
    );
    match (a.medium(), b.medium()) {
        (None, None) => {}
        (Some(x), Some(y)) => {
            assert_eq!(bits(x.sigma_a), bits(y.sigma_a), "{what}: medium σa");
            assert_eq!(bits(x.sigma_s), bits(y.sigma_s), "{what}: medium σs");
            assert_eq!(x.g.to_bits(), y.g.to_bits(), "{what}: medium g");
        }
        _ => panic!("{what}: one ray carries a medium and the other does not"),
    }
}

/// Every query a [`ShadingPoint`] answers, against the material's own
/// method at the same hit.
fn assert_resolve_matches(name: &str, mat: &dyn Material) {
    let rec = hit();
    for dir in [
        Vec3A::new(0.3, -0.2, -1.0).normalize(),
        Vec3A::new(-0.7, 0.1, -0.4).normalize(),
    ] {
        let r_in = Ray::new(rec.p - dir, dir);
        let cos = dir.dot(rec.normal).abs();
        let sp = ShadingPoint::new(mat, &r_in, &rec, cos);

        assert_eq!(
            bits(sp.emitted()),
            bits(mat.emitted_at(&r_in, &rec, cos)),
            "{name}: emission"
        );
        for k in 0..64 {
            let sampler = PathSampler::new(0, 0, 0, k);
            let a = sp.scatter_importance(&r_in, sampler);
            let b = mat.scatter_importance(&r_in, &rec, sampler);
            match (a, b) {
                (None, None) => {}
                (Some(a), Some(b)) => {
                    let what = format!("{name}: scatter {k}");
                    assert_eq!(bits(a.value), bits(b.value), "{what}: value");
                    assert_eq!(a.pdf.to_bits(), b.pdf.to_bits(), "{what}: pdf");
                    assert_eq!(a.delta, b.delta, "{what}: delta");
                    assert_eq!(a.subsurface, b.subsurface, "{what}: subsurface");
                    assert_eq!(a.spread.to_bits(), b.spread.to_bits(), "{what}: spread");
                    same_ray(&a.ray, &b.ray, &what);
                }
                _ => panic!("{name}: scatter {k} answered on one side only"),
            }
        }
        for wi in [
            Vec3A::new(0.2, 0.3, 0.9).normalize(),
            Vec3A::new(-0.5, 0.1, 0.4).normalize(),
            Vec3A::new(0.1, 0.2, -0.9).normalize(),
        ] {
            match (sp.eval(&r_in, wi), mat.eval(&r_in, &rec, wi)) {
                (None, None) => {}
                (Some((va, pa)), Some((vb, pb))) => {
                    assert_eq!(bits(va), bits(vb), "{name}: eval value toward {wi}");
                    assert_eq!(pa.to_bits(), pb.to_bits(), "{name}: eval pdf toward {wi}");
                }
                _ => panic!("{name}: eval toward {wi} answered on one side only"),
            }
            same_ray(
                &sp.make_ray(wi),
                &mat.make_ray(&rec, wi),
                &format!("{name}: make_ray toward {wi}"),
            );
        }
    }
}

#[test]
fn resolve_matches_per_query_shading_for_every_material() {
    let ptex = Some(PtexRef(Arc::new(Gradient)));
    let coated_emitter = OpenPBR {
        coat_weight: 0.6,
        emission_luminance: 2.0,
        ..OpenPBR::default()
    };
    let deep_glass = OpenPBR {
        transmission_color: Vec3A::new(0.5, 0.7, 0.9),
        transmission_depth: 2.0,
        specular_roughness: 0.3,
        ..OpenPBR::glass(1.5)
    };

    let mut materials: Vec<(String, Arc<dyn Material>)> = vec![
        ("OpenPBR".into(), Arc::new(OpenPBR::default())),
        ("OpenPBR glass".into(), Arc::new(deep_glass.clone())),
        (
            "OpenPBR + Ptex under an emissive coat".into(),
            Arc::new(OpenPBR {
                base_color_ptex: ptex.clone(),
                ..coated_emitter.clone()
            }),
        ),
        (
            "OpenPBR glass + Ptex".into(),
            Arc::new(OpenPBR {
                base_color_ptex: ptex.clone(),
                ..deep_glass.clone()
            }),
        ),
        (
            "Emissive".into(),
            Arc::new(Emissive::new(Vec3A::splat(3.0))),
        ),
        (
            "PreviewSurface (textured, emissive, normal-mapped, Ptex)".into(),
            Arc::new(PreviewSurface::new(
                "p".into(),
                OpenPBR {
                    base_color_ptex: ptex.clone(),
                    ..coated_emitter.clone()
                },
                vec![
                    (Target::DiffuseColor, uv_input(TexOutput::Rgb)),
                    (Target::EmissiveColor, uv_input(TexOutput::Rgb)),
                    (Target::Roughness, uv_input(TexOutput::R)),
                ],
                Some(uv_input(TexOutput::Rgb)),
            )),
        ),
        (
            "PreviewSurface (textured opacity over glass)".into(),
            Arc::new(PreviewSurface::new(
                "g".into(),
                deep_glass.clone(),
                vec![(Target::Opacity, uv_input(TexOutput::R))],
                None,
            )),
        ),
    ];
    let decline = |_: &str, _: Option<&str>| -> Option<TextureRef> { None };
    for (file, node) in [
        ("materialx_basic.mtlx", "mtlx_ceramic"),
        ("materialx_basic.mtlx", "mtlx_metal"),
        ("materialx_basic.mtlx", "mtlx_lacquer"),
        ("materialx_emissive.mtlx", "mtlx_emitter_constant"),
        ("materialx_emissive.mtlx", "mtlx_emitter_textured"),
        ("materialx_surfaces.mtlx", "mtlx_openpbr_coated"),
        ("materialx_surfaces.mtlx", "mtlx_standard_gold"),
        ("materialx_surfaces.mtlx", "mtlx_gltf_clearcoat"),
        ("materialx_surfaces.mtlx", "mtlx_standard_glass"),
        ("materialx_surfaces.mtlx", "mtlx_gltf_ruby"),
        ("materialx_surfaces.mtlx", "mtlx_openpbr_bumped_coat"),
        ("materialx_subsurface.mtlx", "mtlx_openpbr_skin"),
        ("materialx_subsurface.mtlx", "mtlx_standard_marble"),
        ("materialx_subsurface.mtlx", "mtlx_bare_jade"),
        ("hair.mtlx", "mtlx_hair_bare"),
        ("hair.mtlx", "mtlx_hair_roughness"),
        ("hair.mtlx", "mtlx_hair_color"),
        ("hair.mtlx", "mtlx_hair_melanin"),
        ("hair.mtlx", "mtlx_hair_mix"),
        ("hair.mtlx", "mtlx_hair_clear"),
        ("hair.mtlx", "mtlx_hair_translucent_mix"),
    ] {
        let loaded = materialx::load(
            &sample(file),
            Some(node),
            &crust_core::mtlx::Host::new(&decline),
        )
        .unwrap_or_else(|e| panic!("{node} compiles: {e:?}"));
        materials.push((format!("MaterialX {node}"), loaded.material));
    }

    for (name, mat) in &materials {
        assert_resolve_matches(name, mat.as_ref());
    }
}
