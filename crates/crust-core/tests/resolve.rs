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
    AreaLight, BsdfDraw, Emissive, HitRecord, Light, Material, MeshLightMaterial, MeshShape,
    OpenPBR, PathSampler, PreviewSurface, PtexRef, PtexTexture, Ray, ShadingPoint, Texture2D,
    TextureRef, Vec3A, materialx,
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

fn hit() -> HitRecord<'static> {
    HitRecord {
        p: Vec3A::new(0.1, -0.2, 0.0),
        normal: Vec3A::Z,
        tangent: Vec3A::X,
        t: 1.0,
        front_face: true,
        part: crust_core::HitPart::Face(crust_core::FaceHit {
            id: 3,
            uv: (0.3, 0.7),
            width: 0.0,
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
    assert_resolve_matches_at(name, mat, hit());
}

/// [`assert_resolve_matches`] at `rec`.
fn assert_resolve_matches_at(name: &str, mat: &dyn Material, rec: HitRecord<'_>) {
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
            let u = PathSampler::new(0, 0, 0, k).draw_sample_f32::<4>();
            let a = sp.scatter_importance(&r_in, || u);
            let b = mat.scatter_importance(&r_in, &rec, BsdfDraw::Fixed(u));
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

/// Every material the tests below shade: one of each kind the renderer
/// has, and the thin-walled transmissive ones.
fn materials() -> Vec<(String, Arc<dyn Material>)> {
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

    let thin_glass = OpenPBR {
        geometry_thin_walled: true,
        transmission_color: Vec3A::new(0.9, 0.6, 0.3),
        ..OpenPBR::glass(1.5)
    };

    let mut materials: Vec<(String, Arc<dyn Material>)> = vec![
        ("OpenPBR".into(), Arc::new(OpenPBR::default())),
        ("OpenPBR glass".into(), Arc::new(deep_glass.clone())),
        ("OpenPBR thin glass".into(), Arc::new(thin_glass.clone())),
        (
            "OpenPBR thin glass + Ptex".into(),
            Arc::new(OpenPBR {
                base_color_ptex: ptex.clone(),
                ..thin_glass.clone()
            }),
        ),
        (
            "PreviewSurface (textured opacity over thin glass)".into(),
            Arc::new(PreviewSurface::new(
                "t".into(),
                thin_glass.clone(),
                vec![(Target::Opacity, uv_input(TexOutput::R))],
                None,
            )),
        ),
        (
            "OpenPBR + Ptex under an emissive coat".into(),
            Arc::new(OpenPBR {
                base_color_ptex: ptex.clone(),
                ..coated_emitter.clone()
            }),
        ),
        (
            "OpenPBR + Ptex under a fuzz over an emissive coat".into(),
            Arc::new(OpenPBR {
                base_color_ptex: ptex.clone(),
                fuzz_weight: 0.8,
                fuzz_roughness: 0.4,
                fuzz_color: Vec3A::new(0.9, 0.6, 0.3),
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
            "OpenPBR emitter + Ptex".into(),
            Arc::new(OpenPBR {
                base_color_ptex: ptex.clone(),
                emission_luminance: 4.0,
                emission_color: Vec3A::new(1.0, 0.5, 0.25),
                ..OpenPBR::default()
            }),
        ),
        (
            "PreviewSurface (textured diffuse, constant emission)".into(),
            Arc::new(PreviewSurface::new(
                "e".into(),
                OpenPBR {
                    emission_luminance: 1.0,
                    emission_color: Vec3A::new(2.0, 1.0, 0.5),
                    ..OpenPBR::default()
                },
                vec![(Target::DiffuseColor, uv_input(TexOutput::Rgb))],
                Some(uv_input(TexOutput::Rgb)),
            )),
        ),
        (
            "PreviewSurface (constant emission, textured clearcoat)".into(),
            Arc::new(PreviewSurface::new(
                "c".into(),
                OpenPBR {
                    emission_luminance: 1.0,
                    emission_color: Vec3A::new(2.0, 1.0, 0.5),
                    ..OpenPBR::default()
                },
                vec![(Target::Clearcoat, uv_input(TexOutput::R))],
                None,
            )),
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
    let decline = |_: &str, _: crust_mtlx::TexelSpace<'_>| -> Option<TextureRef> { None };
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
        ("materialx_thin_walled.mtlx", "mtlx_openpbr_window"),
        ("materialx_thin_walled.mtlx", "mtlx_standard_film"),
    ] {
        let loaded = materialx::load(
            &sample(file),
            Some(node),
            &crust_core::mtlx::Host::new(&decline),
        )
        .unwrap_or_else(|e| panic!("{node} compiles: {e:?}"));
        materials.push((format!("MaterialX {node}"), loaded.material));
    }
    // A mesh light's wrapper over one of each: everything but emission is
    // the inner material's, emission the light's.
    let glow = Arc::new(Emissive::new(Vec3A::new(6.0, 3.0, 1.5)));
    let wrapped: Vec<(String, Arc<dyn Material>)> = materials
        .iter()
        .filter(|(name, _)| {
            [
                "OpenPBR",
                "OpenPBR thin glass + Ptex",
                "OpenPBR + Ptex under an emissive coat",
                "PreviewSurface (textured, emissive, normal-mapped, Ptex)",
                "MaterialX mtlx_emitter_constant",
                "MaterialX mtlx_openpbr_coated",
            ]
            .contains(&name.as_str())
        })
        .map(|(name, inner)| {
            let m: Arc<dyn Material> =
                Arc::new(MeshLightMaterial::new(inner.clone(), glow.clone()));
            (format!("MeshLightMaterial over {name}"), m)
        })
        .collect();
    assert_eq!(wrapped.len(), 6);
    materials.extend(wrapped);
    materials
}

#[test]
fn resolve_matches_per_query_shading_for_every_material() {
    for (name, mat) in &materials() {
        assert_resolve_matches(name, mat.as_ref());
    }
}

/// `Material::uniform_emission` is a promise about every hit: where a
/// material answers `Some(c)`, `emitted_at` — and the resolution a vertex
/// shades with — is `c` bit for bit at every point, normal, chart position,
/// face and angle. A mesh light's NEE reads the constant and its bounce side
/// the hit, so a drift would be an MIS pair that disagrees. Run with and
/// without `--features jit`: the MaterialX constant must equal both.
#[test]
fn uniform_emission_is_what_every_hit_emits() {
    let mut answered = Vec::new();
    for (name, mat) in &materials() {
        let Some(c) = mat.uniform_emission() else {
            continue;
        };
        answered.push(name.clone());
        for k in 0..64u32 {
            let u = PathSampler::new(1, 2, 3, k as i32).draw_sample_f32::<4>();
            let normal = Vec3A::new(u[0] - 0.5, u[1] - 0.5, 0.2 + u[2]).normalize();
            let rec = HitRecord {
                p: Vec3A::new(4.0 * u[0] - 2.0, 3.0 * u[1], -u[3]),
                normal,
                tangent: normal.any_orthonormal_vector(),
                front_face: k % 3 != 0,
                part: crust_core::HitPart::Face(crust_core::FaceHit {
                    id: k % 16,
                    uv: (u[2], u[3]),
                    width: 0.0,
                }),
                uv: Some((u[1], u[0])),
                ..hit()
            };
            let dir = Vec3A::new(u[3] - 0.5, u[0] - 0.5, -0.05 - u[1]).normalize();
            let r_in = Ray::new(rec.p - dir, dir);
            let cos = dir.dot(rec.normal).abs();
            assert_eq!(
                bits(mat.emitted_at(&r_in, &rec, cos)),
                bits(c),
                "{name}: emitted_at at sample {k}"
            );
            assert_eq!(
                bits(ShadingPoint::new(mat.as_ref(), &r_in, &rec, cos).emitted()),
                bits(c),
                "{name}: the resolution's emission at sample {k}"
            );
        }
    }
    for expected in [
        "MeshLightMaterial over OpenPBR + Ptex under an emissive coat",
        "OpenPBR",
        "Emissive",
        "OpenPBR emitter + Ptex",
        "PreviewSurface (textured diffuse, constant emission)",
        "MaterialX mtlx_emitter_constant",
    ] {
        assert!(
            answered.iter().any(|n| n == expected),
            "{expected} answers a uniform emission (answered: {answered:?})"
        );
    }
    for refused in [
        "OpenPBR + Ptex under an emissive coat",
        "PreviewSurface (textured, emissive, normal-mapped, Ptex)",
        "PreviewSurface (constant emission, textured clearcoat)",
    ] {
        assert!(
            !answered.iter().any(|n| n == refused),
            "{refused} must not answer a uniform emission"
        );
    }
}

/// The straight transmittance comes out of the resolved BSDF, so it is the
/// one the vertex would shade with: a material reporting any at a hit says
/// so up front (or the world never passes it), and the resolution without
/// it keeps every value of the whole — only the delta straight line leaves
/// its sampling, and the density renormalises over what stays.
#[test]
fn straight_transmission_is_declared_and_split_from_the_resolution() {
    let rec = hit();
    let mut thin = 0;
    for (name, mat) in &materials() {
        for dir in [
            Vec3A::new(0.3, -0.2, -1.0).normalize(),
            Vec3A::new(-0.7, 0.1, -0.4).normalize(),
        ] {
            let r_in = Ray::new(rec.p - dir, dir);
            let cos = dir.dot(rec.normal).abs();
            let sp = ShadingPoint::new(mat.as_ref(), &r_in, &rec, cos);
            let mut t = Vec3A::ZERO;
            for k in 0..64 {
                t += sp.straight_transmittance(&r_in, || {
                    PathSampler::new(0, 0, 0, k).draw_sample_f32::<2>()
                });
            }
            if t == Vec3A::ZERO {
                continue;
            }
            thin += 1;
            assert!(
                mat.has_straight_transmission(),
                "{name}: straight transmission at a hit, but not declared"
            );
            let mut rest = ShadingPoint::new(mat.as_ref(), &r_in, &rec, cos);
            rest.exclude_straight();
            assert_eq!(
                rest.straight_transmittance(&r_in, || {
                    PathSampler::new(0, 0, 0, 1).draw_sample_f32::<2>()
                }),
                Vec3A::ZERO,
                "{name}: excluded"
            );
            for k in 0..64 {
                if let Some(x) = rest.scatter_importance(&r_in, || {
                    PathSampler::new(0, 0, 0, k).draw_sample_f32::<4>()
                }) {
                    let straight = x.ray.direction().normalize().dot(dir) > 1.0 - 1e-6;
                    assert!(!(x.delta && straight), "{name}: a straight delta remains");
                }
            }
            for wi in [
                Vec3A::new(0.2, 0.3, 0.9).normalize(),
                Vec3A::new(0.1, 0.2, -0.9).normalize(),
            ] {
                let (Some((va, _)), Some((vb, _))) = (sp.eval(&r_in, wi), rest.eval(&r_in, wi))
                else {
                    panic!("{name}: eval answers with or without the straight lobe");
                };
                assert_eq!(bits(va), bits(vb), "{name}: eval value toward {wi}");
            }
        }
    }
    // Five, and the mesh-light wrapper over thin glass.
    assert_eq!(thin, 2 * 6, "every thin-walled material, both directions");
}

/// On a curve hit, a material reading the strand and a primvar resolves to
/// what per-query shading of the same hit gives.
#[test]
fn a_curve_reading_material_resolves_as_it_shades() {
    use crust_core::{CurveHit, StrandSpan, StrandTable, StrandValues, mtlx::Val};
    let dir = std::env::temp_dir().join("crust_resolve_curve");
    std::fs::create_dir_all(&dir).unwrap();
    let mtlx = dir.join("hair.mtlx");
    std::fs::write(
        &mtlx,
        r#"<?xml version="1.0"?>
<materialx version="1.39">
  <geompropvalue name="v" type="float"><input name="geomprop" type="string" value="curve:v" /></geompropvalue>
  <geompropvalue name="r" type="float"><input name="geomprop" type="string" value="curve:random" /></geompropvalue>
  <geompropvalue name="tint" type="color3"><input name="geomprop" type="string" value="tint" /></geompropvalue>
  <mix name="ramp" type="color3">
    <input name="bg" type="color3" nodename="tint" />
    <input name="fg" type="color3" value="0.9, 0.8, 0.6" />
    <input name="mix" type="float" nodename="v" />
  </mix>
  <standard_surface name="s" type="surfaceshader">
    <input name="base_color" type="color3" nodename="ramp" />
    <input name="specular_roughness" type="float" nodename="r" />
    <input name="emission" type="float" nodename="v" />
    <input name="emission_color" type="color3" nodename="tint" />
  </standard_surface>
  <surfacematerial name="m" type="material"><input name="surfaceshader" type="surfaceshader" nodename="s" /></surfacematerial>
</materialx>
"#,
    )
    .unwrap();
    let loaded = materialx::load(&mtlx, Some("m"), &crust_core::mtlx::Host::new(&|_, _| None))
        .expect("loads");
    let table = StrandTable::new(
        vec![
            StrandSpan {
                curve: 0,
                v0: 0.0,
                v1: 0.5,
            },
            StrandSpan {
                curve: 3,
                v0: 0.5,
                v1: 1.0,
            },
        ],
        "/World/Groom",
    )
    .with_primvars(vec![Some(StrandValues::PerCurve(vec![
        Val::vec3(0.2, 0.1, 0.05),
        Val::vec3(0.1, 0.1, 0.1),
        Val::vec3(0.3, 0.3, 0.3),
        Val::vec3(0.5, 0.2, 0.1),
    ]))]);
    for (prim, u) in [(0, 0.3), (1, 0.75)] {
        let rec = HitRecord {
            part: crust_core::HitPart::Curve(CurveHit::new(&table, prim, u).expect("a span")),
            ..hit()
        };
        assert!(rec.curve().is_some());
        assert_resolve_matches_at(&format!("curve span {prim}"), loaded.material.as_ref(), rec);
    }
}

/// A mesh light's wrapper emits what its light's NEE samples: the shared
/// `Emissive`'s `radiance_toward`, from either side of the surface, at any
/// point — whatever the inner material's own glow.
#[test]
fn a_mesh_light_material_emits_its_lights_radiance() {
    let emitter = Arc::new(Emissive::new(Vec3A::new(6.0, 3.0, 1.5)));
    let inner: Arc<dyn Material> = Arc::new(OpenPBR {
        emission_luminance: 9.0,
        coat_weight: 0.5,
        ..OpenPBR::default()
    });
    let wrapper = MeshLightMaterial::new(inner, emitter.clone());
    let shape = MeshShape::new(vec![[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]])
        .expect("a triangle");
    let light = AreaLight::new(shape, emitter.clone(), 0);
    for (from, front) in [
        (Vec3A::new(0.2, 0.2, 2.0), true),
        (Vec3A::new(0.2, 0.2, -2.0), false),
    ] {
        let s = light.sample_li(from, 0.3, 0.6).expect("sampled");
        let p = from + s.direction * s.distance;
        let rec = HitRecord {
            p,
            normal: if front { Vec3A::Z } else { -Vec3A::Z },
            front_face: front,
            ..hit()
        };
        let r_in = Ray::new(from, s.direction);
        let cos = s.direction.dot(rec.normal).abs();
        assert_eq!(bits(wrapper.emitted_at(&r_in, &rec, cos)), bits(s.radiance));
        assert_eq!(
            bits(s.radiance),
            bits(emitter.radiance_toward(p, -s.direction, front)),
            "front {front}"
        );
        assert_eq!(
            bits(ShadingPoint::new(&wrapper, &r_in, &rec, cos).emitted()),
            bits(s.radiance)
        );
    }
}
