//! MaterialX surface-shader nodes (`open_pbr_surface`, `standard_surface`,
//! `gltf_pbr`) expanded into their nodegraphs' closure trees, checked in
//! numbers through the probe — every scenario of the materials spec's
//! surface-shader requirements.

use crust_core::closure::mx::{Fresnel, FresnelModel};
use crust_core::closure::{Lobe, Prepared};
use crust_core::materialx::{self, Loaded};
use crust_core::rt::Geometry;
use crust_core::{
    Emissive, HitRecord, LightList, Material, PathSampler, Ray, SamplingStrategy, Vec3A, Volumes,
    WorldBuilder, ray_color,
};
use std::sync::Arc;

/// Loads the first material of an inline document whose body declares a
/// surface node named `s`.
fn load(tag: &str, surface: &str) -> Loaded {
    try_load(tag, surface).expect("loads")
}

fn try_load(tag: &str, surface: &str) -> Result<Loaded, crust_core::mtlx::MtlxError> {
    let dir = std::env::temp_dir().join(format!("crust_mtlx_surf_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.mtlx");
    std::fs::write(
        &path,
        format!(
            r#"<materialx version="1.39">{surface}
                 <surfacematerial name="m" type="material">
                   <input name="surfaceshader" type="surfaceshader" nodename="s" />
                 </surfacematerial>
               </materialx>"#
        ),
    )
    .unwrap();
    let loaded = materialx::load(&path, None, &|_, _| None);
    let _ = std::fs::remove_dir_all(&dir);
    loaded
}

fn hit(u: f32, v: f32) -> HitRecord {
    HitRecord {
        p: Vec3A::ZERO,
        normal: Vec3A::Z,
        t: 1.0,
        front_face: true,
        face: None,
        uv: (u, v),
        tangent: Vec3A::X,
        has_uv: true,
        uv_width: 0.0,
        face_width: 0.0,
    }
}

fn straight_down() -> Ray {
    Ray::new(Vec3A::Z, -Vec3A::Z)
}

fn leaves(l: &Loaded, u: f32, v: f32) -> Vec<Prepared> {
    l.material
        .probe(&straight_down(), &hit(u, v))
        .closure
        .leaves()
        .to_vec()
}

fn find<'a>(ls: &'a [Prepared], category: &str) -> Vec<&'a Prepared> {
    ls.iter().filter(|l| l.category == category).collect()
}

fn specular(l: &Prepared) -> (Fresnel, f32, f32) {
    match l.lobe {
        Lobe::Specular {
            fresnel, ax, ay, ..
        } => (fresnel, ax, ay),
        _ => panic!("{} is not specular", l.category),
    }
}

fn near(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn a_constant_standard_surface_is_no_longer_the_fallback() {
    let l = load(
        "red",
        r#"<standard_surface name="s" type="surfaceshader">
             <input name="base_color" type="color3" value="0.8, 0.1, 0.1" />
           </standard_surface>"#,
    );
    assert!(l.unsupported.is_empty(), "{:?}", l.unsupported);
    let ls = leaves(&l, 0.5, 0.5);
    let d = find(&ls, "oren_nayar_diffuse_bsdf");
    assert_eq!(d.len(), 1, "one diffuse leaf");
    assert!(
        d[0].describe().contains("0.8000 0.1000 0.1000"),
        "{}",
        d[0].describe()
    );
    // The default specular (weight 1, roughness 0.2, IOR 1.5) is there too.
    let s = find(&ls, "dielectric_bsdf");
    assert_eq!(s.len(), 1);
    let (_, ax, ay) = specular(s[0]);
    assert!(
        near(ax, 0.04, 1e-6) && near(ay, 0.04, 1e-6),
        "α = 0.2² ({ax}, {ay})"
    );
}

#[test]
fn an_unauthored_open_pbr_input_takes_the_nodedef_default() {
    let l = load(
        "defaults",
        r#"<open_pbr_surface name="s" type="surfaceshader">
             <input name="base_color" type="color3" value="0.2, 0.4, 0.6" />
           </open_pbr_surface>"#,
    );
    let ls = leaves(&l, 0.5, 0.5);
    let s = find(&ls, "dielectric_bsdf");
    assert_eq!(
        s.len(),
        1,
        "specular only: no coat, no metal, no fuzz by default"
    );
    let (f, ax, ay) = specular(s[0]);
    // OpenPBR 1.1: specular_roughness 0.3 → α 0.09; IOR 1.5, left unmodulated
    // by specular_weight 1.
    assert!(near(ax, 0.09, 1e-6) && near(ay, 0.09, 1e-6), "({ax}, {ay})");
    let FresnelModel::Dielectric { ior } = f.model else {
        panic!()
    };
    assert!(near(ior, 1.5, 1e-4), "ior {ior}");
    assert_eq!(ls.len(), 2, "the specular over the EON diffuse");
}

#[test]
fn a_connected_input_varies_over_the_surface() {
    let l = load(
        "uv",
        r#"<texcoord name="tc" type="vector2" />
           <separate2 name="sp" type="multioutput"><input name="in" type="vector2" nodename="tc" /></separate2>
           <combine3 name="c" type="color3">
             <input name="in1" type="float" value="0.5" />
             <input name="in2" type="float" value="0.5" />
             <input name="in3" type="float" value="0.5" />
           </combine3>
           <multiply name="scaled" type="color3">
             <input name="in1" type="color3" nodename="c" />
             <input name="in2" type="float" nodename="u" />
           </multiply>
           <extract name="u" type="float">
             <input name="in" type="vector2" nodename="tc" />
             <input name="index" type="integer" value="0" />
           </extract>
           <gltf_pbr name="s" type="surfaceshader">
             <input name="base_color" type="color3" nodename="scaled" />
             <input name="metallic" type="float" value="0" />
           </gltf_pbr>"#,
    );
    let color = |u: f32| {
        let ls = leaves(&l, u, 0.5);
        find(&ls, "oren_nayar_diffuse_bsdf")[0].describe()
    };
    assert!(
        color(0.2).contains("0.1000 0.1000 0.1000"),
        "{}",
        color(0.2)
    );
    assert!(
        color(0.8).contains("0.4000 0.4000 0.4000"),
        "{}",
        color(0.8)
    );
}

#[test]
fn open_pbr_metal_honours_specular_weight() {
    let metal = |w: f32| {
        let l = load(
            &format!("metal{w}"),
            &format!(
                r#"<open_pbr_surface name="s" type="surfaceshader">
                     <input name="base_metalness" type="float" value="1" />
                     <input name="specular_weight" type="float" value="{w}" />
                   </open_pbr_surface>"#
            ),
        );
        let ls = leaves(&l, 0.5, 0.5);
        find(&ls, "generalized_schlick_bsdf")[0].weight.x
    };
    let (full, half) = (metal(1.0), metal(0.5));
    assert!(near(half, 0.5 * full, 1e-5), "{half} vs {full}");
}

#[test]
fn an_open_pbr_coat_broadens_the_base_highlight() {
    let l = load(
        "broaden",
        r#"<open_pbr_surface name="s" type="surfaceshader">
             <input name="specular_roughness" type="float" value="0.1" />
             <input name="coat_weight" type="float" value="1" />
             <input name="coat_roughness" type="float" value="0.5" />
           </open_pbr_surface>"#,
    );
    let ls = leaves(&l, 0.5, 0.5);
    let r = (0.1f32.powi(4) + 2.0 * 0.5f32.powi(4)).powf(0.25);
    let base_alpha = r * r;
    let alphas: Vec<f32> = find(&ls, "dielectric_bsdf")
        .iter()
        .map(|l| specular(l).1)
        .collect();
    assert!(
        alphas.iter().any(|a| near(*a, base_alpha, 1e-5)),
        "base α {base_alpha} among {alphas:?}"
    );
    assert!(
        alphas.iter().any(|a| near(*a, 0.25, 1e-5)),
        "coat α 0.25 among {alphas:?}"
    );
}

#[test]
fn a_standard_surface_metal_is_an_artistic_ior_conductor() {
    let l = load(
        "artistic",
        r#"<standard_surface name="s" type="surfaceshader">
             <input name="metalness" type="float" value="1" />
             <input name="base_color" type="color3" value="0.9, 0.6, 0.2" />
             <input name="specular_color" type="color3" value="1, 0.9, 0.7" />
           </standard_surface>"#,
    );
    let ls = leaves(&l, 0.5, 0.5);
    let c = find(&ls, "conductor_bsdf");
    assert_eq!(c.len(), 1);
    let (f, _, _) = specular(c[0]);
    let FresnelModel::Conductor { n, k } = f.model else {
        panic!()
    };
    // artistic_ior's reflectivity is `base_color · base` (base 1.0): the
    // complex IOR must reproduce it at normal incidence.
    let r = crust_core::mtlx::reflectivity_from_ior(n, k);
    assert!(r.abs_diff_eq(Vec3A::new(0.9, 0.6, 0.2), 1e-3), "{r}");
}

#[test]
fn a_gltf_clearcoat_is_a_separate_lobe() {
    let l = load(
        "clearcoat",
        r#"<gltf_pbr name="s" type="surfaceshader">
             <input name="metallic" type="float" value="0" />
             <input name="roughness" type="float" value="0.6" />
             <input name="clearcoat" type="float" value="1" />
             <input name="clearcoat_roughness" type="float" value="0" />
           </gltf_pbr>"#,
    );
    let ls = leaves(&l, 0.5, 0.5);
    let coat = find(&ls, "dielectric_bsdf");
    let base = find(&ls, "generalized_schlick_bsdf");
    assert_eq!((coat.len(), base.len()), (1, 1));
    assert!(near(specular(coat[0]).1, 1e-4, 1e-6), "a mirror clearcoat");
    assert!(near(specular(base[0]).1, 0.36, 1e-5), "α = 0.6²");
}

#[test]
fn a_standard_surface_glass_transmits() {
    let l = load(
        "glass",
        r#"<standard_surface name="s" type="surfaceshader">
             <input name="transmission" type="float" value="1" />
             <input name="specular_roughness" type="float" value="0" />
           </standard_surface>"#,
    );
    let p = l.material.probe(&straight_down(), &hit(0.5, 0.5));
    assert!(p.closure.transmits());
    assert!(
        p.closure
            .leaves()
            .iter()
            .all(|l| l.category == "dielectric_bsdf"),
        "at transmission 1 nothing but the two dielectric leaves remains"
    );
}

#[test]
fn gltf_attenuation_is_the_interior_medium() {
    let l = load(
        "atten",
        r#"<gltf_pbr name="s" type="surfaceshader">
             <input name="metallic" type="float" value="0" />
             <input name="transmission" type="float" value="1" />
             <input name="thickness" type="float" value="1" />
             <input name="attenuation_color" type="color3" value="1, 0.2, 0.2" />
             <input name="attenuation_distance" type="float" value="0.5" />
           </gltf_pbr>"#,
    );
    let p = l.material.probe(&straight_down(), &hit(0.5, 0.5));
    let m = p.closure.medium().expect("attenuation makes a medium");
    let expected = -0.2f32.ln() / 0.5;
    assert!(near(m.sigma_a.x, 0.0, 1e-4), "{}", m.sigma_a);
    assert!(near(m.sigma_a.y, expected, 1e-3) && near(m.sigma_a.z, expected, 1e-3));
    // `thickness` is ignored by MaterialX's graph, and reported as such.
    assert!(
        l.reported.iter().any(|r| r.contains("thickness")),
        "{:?}",
        l.reported
    );
}

#[test]
fn an_open_pbr_coat_normal_perturbs_only_the_coat() {
    let l = load(
        "coatnormal",
        r#"<constant name="n" type="vector3"><input name="value" type="vector3" value="0.3, 0, 0.954" /></constant>
           <open_pbr_surface name="s" type="surfaceshader">
             <input name="coat_weight" type="float" value="1" />
             <input name="geometry_coat_normal" type="vector3" nodename="n" />
           </open_pbr_surface>"#,
    );
    let ls = leaves(&l, 0.5, 0.5);
    let tilted = Vec3A::new(0.3, 0.0, 0.954).normalize();
    let mut coats = 0;
    for leaf in &ls {
        if (leaf.frame.n - tilted).length() < 1e-5 {
            coats += 1;
        } else {
            assert_eq!(
                leaf.frame.n,
                Vec3A::Z,
                "{} kept the base normal",
                leaf.category
            );
        }
    }
    assert_eq!(coats, 1, "exactly the coat leaf is tilted");
}

#[test]
fn authored_opacity_is_reported_not_applied() {
    let l = load(
        "opacity",
        r#"<standard_surface name="s" type="surfaceshader">
             <input name="opacity" type="color3" value="0.3, 0.3, 0.3" />
           </standard_surface>"#,
    );
    assert!(
        l.reported.iter().any(|r| r.starts_with("opacity")),
        "{:?}",
        l.reported
    );
    // Opaque all the same: nothing transmits.
    let p = l.material.probe(&straight_down(), &hit(0.5, 0.5));
    assert!(!p.closure.transmits());
}

#[test]
fn default_valued_inputs_are_silent() {
    let l = load(
        "silent",
        r#"<gltf_pbr name="s" type="surfaceshader">
             <input name="alpha" type="float" value="1" />
             <input name="alpha_mode" type="integer" value="0" />
           </gltf_pbr>"#,
    );
    assert!(l.reported.is_empty(), "{:?}", l.reported);
}

#[test]
fn a_live_fuzz_layer_reports_its_sheen_approximation() {
    let l = load(
        "fuzz",
        r#"<open_pbr_surface name="s" type="surfaceshader">
             <input name="fuzz_weight" type="float" value="0.5" />
           </open_pbr_surface>"#,
    );
    assert!(
        l.reported.iter().any(|r| r.contains("zeltner")),
        "{:?}",
        l.reported
    );
    let quiet = load(
        "nofuzz",
        r#"<open_pbr_surface name="s" type="surfaceshader" />"#,
    );
    assert!(quiet.reported.is_empty(), "{:?}", quiet.reported);
}

#[test]
fn a_non_default_version_is_reported() {
    let l = load(
        "version",
        r#"<open_pbr_surface name="s" type="surfaceshader" version="1.0" />"#,
    );
    assert!(
        l.reported.iter().any(|r| r.contains("version 1.0")),
        "{:?}",
        l.reported
    );
}

#[test]
fn a_coated_emitter_fades_through_the_coat_fresnel() {
    let l = load(
        "emit",
        r#"<open_pbr_surface name="s" type="surfaceshader">
             <input name="emission_luminance" type="float" value="10" />
             <input name="coat_weight" type="float" value="1" />
           </open_pbr_surface>"#,
    );
    let at = |cos: f32| {
        let sin = (1.0 - cos * cos).sqrt();
        let d = Vec3A::new(sin, 0.0, cos);
        l.material.emitted_at(&Ray::new(d, -d), &hit(0.5, 0.5), cos)
    };
    // Head on, the coat passes 1 − F0 (IOR 1.6: F0 ≈ 0.0533); at grazing, nothing.
    let f0 = ((1.6f32 - 1.0) / 2.6).powi(2);
    assert!(near(at(1.0).x, 10.0 * (1.0 - f0), 1e-3), "{}", at(1.0));
    assert!(at(0.0).x < 1e-3, "{}", at(0.0));
}

#[test]
fn standalone_bsdf_documents_are_untouched_by_the_builders() {
    // The surface node paths do not disturb a plain `surface` document.
    let l = load(
        "plain",
        r#"<oren_nayar_diffuse_bsdf name="d" type="BSDF" />
           <surface name="s" type="surfaceshader"><input name="bsdf" type="BSDF" nodename="d" /></surface>"#,
    );
    let ls = leaves(&l, 0.5, 0.5);
    assert_eq!(ls.len(), 1);
    assert!(
        l.material
            .eval(&straight_down(), &hit(0.5, 0.5), Vec3A::Z)
            .is_some()
    );
}

/// The white furnace: every fixture material on a unit sphere inside a
/// uniform emitter of radiance 1, traced unclamped (`ray_color` applies no
/// firefly clamp). A closure that is energy-conserving reflects and transmits
/// at most what arrives, so no pixel — head-on, mid-way or grazing — may come
/// back brighter than the environment behind it. A layer that added its top's
/// and base's full responses, or a leaf whose sample weight disagreed with its
/// `eval / pdf`, reads above 1 here.
#[test]
fn every_fixture_material_is_bounded_in_a_white_furnace() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/materialx_surfaces.mtlx");
    let names = [
        "mtlx_openpbr_coated",
        "mtlx_standard_gold",
        "mtlx_gltf_clearcoat",
        "mtlx_standard_glass",
        "mtlx_gltf_ruby",
        "mtlx_openpbr_bumped_coat",
    ];
    for name in names {
        let loaded = materialx::load(&path, Some(name), &|_, _| None).expect("loads");
        let mut world = WorldBuilder::new();
        world.attach(
            Geometry::Sphere {
                center: Vec3A::ZERO,
                radius: 1.0,
            },
            loaded.material.clone(),
        );
        world.attach(
            Geometry::Sphere {
                center: Vec3A::ZERO,
                radius: 50.0,
            },
            Arc::new(Emissive::new(Vec3A::ONE)),
        );
        let world = world.commit();
        let (lights, volumes) = (LightList::new(), Volumes::default());
        for offset in [0.0f32, 0.6, 0.95] {
            let ray = Ray::new(Vec3A::new(offset, 0.0, -5.0), Vec3A::Z);
            let n = 2048;
            let mut sum = Vec3A::ZERO;
            for i in 0..n {
                sum += ray_color(
                    &ray,
                    &world,
                    &lights,
                    &volumes,
                    24,
                    SamplingStrategy::PowerMis,
                    PathSampler::new(5, 11, 0, i),
                );
            }
            let mean = sum / n as f32;
            assert!(
                mean.max_element() <= 1.02 && mean.min_element() >= 0.0,
                "{name} at offset {offset}: furnace mean {mean}"
            );
        }
    }
}
