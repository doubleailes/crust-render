//! MaterialX surface-shader nodes (`open_pbr_surface`, `standard_surface`,
//! `gltf_pbr`) expanded into their nodegraphs' closure trees, checked in
//! numbers through the probe — every scenario of the materials spec's
//! surface-shader requirements.

use crust_core::closure::mx::{Fresnel, FresnelModel};
use crust_core::closure::{self, Lobe, Prepared};
use crust_core::materialx::{self, Loaded};
use crust_core::rt::Geometry;
use crust_core::{
    AreaLight, Emissive, HitRecord, LightList, MASK_INDIRECT, MASK_SHADOW, Material, OpenPBR,
    PathSampler, Ray, SamplingStrategy, SphereShape, Vec3A, Volumes, WorldBuilder, ray_color,
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

/// The luminance MaterialX's `luminance` node takes by default (ACEScg).
fn acescg_luminance(c: Vec3A) -> f32 {
    c.dot(Vec3A::new(0.2722287, 0.6740818, 0.0536895))
}

#[test]
fn standard_surface_opacity_is_the_luminance_of_its_colour() {
    let l = load(
        "opacity",
        r#"<standard_surface name="s" type="surfaceshader">
             <input name="opacity" type="color3" value="0.2, 0.5, 0.8" />
           </standard_surface>"#,
    );
    assert!(
        l.reported.is_empty(),
        "applied, not reported: {:?}",
        l.reported
    );
    assert!(l.material.has_cutout());
    let p = l.material.probe(&straight_down(), &hit(0.5, 0.5));
    let expected = acescg_luminance(Vec3A::new(0.2, 0.5, 0.8));
    assert!(
        near(p.opacity, expected, 1e-6),
        "{} vs {expected}",
        p.opacity
    );
    // A cutout is presence, not transmission: the surface itself is opaque.
    assert!(!p.closure.transmits());
}

#[test]
fn an_opaque_surface_has_no_cutout() {
    for (tag, doc) in [
        (
            "std",
            r#"<standard_surface name="s" type="surfaceshader" />"#,
        ),
        (
            "opbr",
            r#"<open_pbr_surface name="s" type="surfaceshader">
                 <input name="geometry_opacity" type="float" value="1" />
               </open_pbr_surface>"#,
        ),
        // OPAQUE never reads alpha, whatever alpha is.
        (
            "gltf",
            r#"<texcoord name="tc" type="vector2" />
               <extract name="u" type="float">
                 <input name="in" type="vector2" nodename="tc" />
                 <input name="index" type="integer" value="0" />
               </extract>
               <gltf_pbr name="s" type="surfaceshader">
                 <input name="alpha" type="float" nodename="u" />
                 <input name="alpha_mode" type="integer" value="0" />
               </gltf_pbr>"#,
        ),
    ] {
        let l = load(tag, doc);
        assert!(!l.material.has_cutout(), "{tag}");
        assert_eq!(l.material.opacity(&straight_down(), &hit(0.1, 0.5)), 1.0);
    }
}

#[test]
fn open_pbr_geometry_opacity_is_the_presence() {
    let l = load(
        "gopacity",
        r#"<open_pbr_surface name="s" type="surfaceshader">
             <input name="geometry_opacity" type="float" value="0.25" />
           </open_pbr_surface>"#,
    );
    assert!(l.reported.is_empty(), "{:?}", l.reported);
    assert!(l.material.has_cutout());
    assert_eq!(l.material.opacity(&straight_down(), &hit(0.5, 0.5)), 0.25);
}

#[test]
fn gltf_alpha_follows_alpha_mode() {
    // `alpha` = u, so one document covers both sides of the cutoff.
    let doc = |mode: &str| {
        format!(
            r#"<texcoord name="tc" type="vector2" />
               <extract name="u" type="float">
                 <input name="in" type="vector2" nodename="tc" />
                 <input name="index" type="integer" value="0" />
               </extract>
               <gltf_pbr name="s" type="surfaceshader">
                 <input name="alpha" type="float" nodename="u" />
                 <input name="alpha_cutoff" type="float" value="0.5" />
                 {mode}
               </gltf_pbr>"#
        )
    };
    let opacity = |l: &Loaded, u: f32| l.material.opacity(&straight_down(), &hit(u, 0.5));
    // MASK: all or nothing about the cutoff, which is inclusive.
    let mask = load(
        "mask",
        &doc(r#"<input name="alpha_mode" type="integer" value="1" />"#),
    );
    assert!(mask.reported.is_empty(), "{:?}", mask.reported);
    assert_eq!(opacity(&mask, 0.3), 0.0);
    assert_eq!(opacity(&mask, 0.5), 1.0);
    assert_eq!(opacity(&mask, 0.7), 1.0);
    // BLEND: alpha itself.
    let blend = load(
        "blend",
        &doc(r#"<input name="alpha_mode" type="integer" value="2" />"#),
    );
    assert_eq!(opacity(&blend, 0.3), 0.3);
    // A mode that does not fold goes through the graph's two `ifequal`s:
    // mode = 2u picks OPAQUE at u = 0, MASK at u = 0.5 and BLEND at u = 1.
    let live = load(
        "live",
        &format!(
            r#"<multiply name="mode" type="float">
                 <input name="in1" type="float" nodename="u" />
                 <input name="in2" type="float" value="2" />
               </multiply>
               {}"#,
            doc(r#"<input name="alpha_mode" type="integer" nodename="mode" />"#)
        ),
    );
    assert!(live.material.has_cutout());
    assert_eq!(opacity(&live, 0.0), 1.0, "OPAQUE ignores alpha = 0");
    assert_eq!(opacity(&live, 0.5), 1.0, "MASK at alpha = cutoff");
    assert_eq!(opacity(&live, 1.0), 1.0, "BLEND at alpha = 1");
}

#[test]
fn a_surface_node_opacity_is_the_presence() {
    let l = load(
        "surface",
        r#"<oren_nayar_diffuse_bsdf name="d" type="BSDF" />
           <surface name="s" type="surfaceshader">
             <input name="bsdf" type="BSDF" nodename="d" />
             <input name="opacity" type="float" value="0.4" />
           </surface>"#,
    );
    assert!(l.reported.is_empty(), "{:?}", l.reported);
    assert!(near(
        l.material.opacity(&straight_down(), &hit(0.5, 0.5)),
        0.4,
        0.0
    ));
}

/// A black cutout sphere in the white furnace. A ray that passes through it
/// crosses it twice, so at opacity `a` it sees the environment with
/// probability `(1 − a)²` and the absorber otherwise.
#[test]
fn a_cutout_passes_its_share_of_the_furnace() {
    let l = load(
        "cutout_furnace",
        r#"<open_pbr_surface name="s" type="surfaceshader">
             <input name="base_color" type="color3" value="0, 0, 0" />
             <input name="specular_weight" type="float" value="0" />
             <input name="geometry_opacity" type="float" value="0.5" />
           </open_pbr_surface>"#,
    );
    for (offset, r) in FURNACE_OFFSETS.iter().zip(furnace(&l)) {
        assert!(
            (r - Vec3A::splat(0.25)).abs().max_element() < 0.04,
            "offset {offset}: {r} vs 0.25"
        );
    }
}

/// A floor under a sphere light, with a black cutout sheet between them at
/// opacity 0.5: the shadow side takes `1 − opacity` through the sheet, the
/// bounce side passes it with that probability, and every strategy must see
/// half the unoccluded light. This is the pair a cutout has to keep: NEE's
/// transmittance and the bounce side's pass-through describe the same
/// visibility, or the MIS strategies disagree.
#[test]
fn every_sampling_strategy_agrees_through_a_cutout() {
    let sheet = load(
        "sheet",
        r#"<gltf_pbr name="s" type="surfaceshader">
             <input name="base_color" type="color3" value="0, 0, 0" />
             <input name="metallic" type="float" value="0" />
             <input name="specular" type="float" value="0" />
             <input name="alpha" type="float" value="0.5" />
             <input name="alpha_mode" type="integer" value="2" />
           </gltf_pbr>"#,
    );
    let quad = |y: f32, half: f32| Geometry::TriangleMesh {
        vertices: vec![
            Vec3A::new(-half, y, -half),
            Vec3A::new(half, y, -half),
            Vec3A::new(half, y, half),
            Vec3A::new(-half, y, half),
        ],
        indices: vec![[0, 2, 1], [0, 3, 2]],
        normals: None,
    };
    let scene = |occluded: bool| {
        let mut world = WorldBuilder::new();
        let mut lights = LightList::new();
        world.attach(
            quad(0.0, 50.0),
            Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))),
        );
        if occluded {
            world.attach(quad(2.5, 50.0), sheet.material.clone());
        }
        let emitter = Arc::new(Emissive::new(Vec3A::splat(40.0)));
        let (center, radius) = (Vec3A::new(0.0, 3.5, 0.0), 0.5);
        let id = world.attach_masked(
            Geometry::Sphere { center, radius },
            emitter.clone(),
            MASK_SHADOW | MASK_INDIRECT,
        );
        lights.add(AreaLight::new(SphereShape { center, radius }, emitter, id));
        (world.commit(), lights)
    };
    let ray = Ray::new(Vec3A::new(0.5, 2.0, 0.5), Vec3A::new(-0.5, -2.0, -0.5));
    let mean = |world: &crust_core::World, lights: &LightList, s: SamplingStrategy, n: i32| {
        let mut sum = 0.0f64;
        for i in 0..n {
            sum += ray_color(
                &ray,
                world,
                lights,
                &Volumes::default(),
                3,
                s,
                PathSampler::new(1, 2, 0, i),
            )
            .x as f64;
        }
        sum / n as f64
    };
    let (open, open_lights) = scene(false);
    let (world, lights) = scene(true);
    assert!(world.has_cutouts() && !open.has_cutouts());
    let reference = 0.5 * mean(&open, &open_lights, SamplingStrategy::PowerMis, 8192);
    assert!(reference > 0.0);
    for (s, n, tol) in [
        (SamplingStrategy::PowerMis, 8192, 0.05),
        (SamplingStrategy::LightOnly, 8192, 0.05),
        // BSDF sampling alone has to find a small light by chance: noisier.
        (SamplingStrategy::BsdfOnly, 65_536, 0.12),
    ] {
        let m = mean(&world, &lights, s, n);
        assert!(
            (m - reference).abs() < tol * reference,
            "{s:?}: {m} vs {reference}"
        );
    }
}

/// An opaque sheet between a floor and its light blocks every shadow ray,
/// in a world with no cutout (the any-hit answer stands) and in one with a
/// cutout elsewhere (the re-walk meets the opaque sheet and stops). Light
/// sampling alone then sees nothing: the bounce side meets a black sheet.
#[test]
fn an_opaque_occluder_blocks_shadow_rays_with_or_without_cutouts() {
    let ghost = load(
        "far_ghost",
        r#"<open_pbr_surface name="s" type="surfaceshader">
             <input name="geometry_opacity" type="float" value="0.5" />
           </open_pbr_surface>"#,
    );
    let quad = |y: f32, half: f32| Geometry::TriangleMesh {
        vertices: vec![
            Vec3A::new(-half, y, -half),
            Vec3A::new(half, y, -half),
            Vec3A::new(half, y, half),
            Vec3A::new(-half, y, half),
        ],
        indices: vec![[0, 2, 1], [0, 3, 2]],
        normals: None,
    };
    let ray = Ray::new(Vec3A::new(0.5, 2.0, 0.5), Vec3A::new(-0.5, -2.0, -0.5));
    for (sheet, cutout_elsewhere) in [(false, false), (true, false), (true, true)] {
        let mut world = WorldBuilder::new();
        let mut lights = LightList::new();
        world.attach(
            quad(0.0, 50.0),
            Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))),
        );
        if sheet {
            world.attach(quad(2.5, 50.0), Arc::new(OpenPBR::diffuse(Vec3A::ZERO)));
        }
        if cutout_elsewhere {
            world.attach(
                Geometry::Sphere {
                    center: Vec3A::new(0.0, -10.0, 0.0),
                    radius: 1.0,
                },
                ghost.material.clone(),
            );
        }
        let emitter = Arc::new(Emissive::new(Vec3A::splat(40.0)));
        let (center, radius) = (Vec3A::new(0.0, 3.5, 0.0), 0.5);
        let id = world.attach_masked(
            Geometry::Sphere { center, radius },
            emitter.clone(),
            MASK_SHADOW | MASK_INDIRECT,
        );
        lights.add(AreaLight::new(SphereShape { center, radius }, emitter, id));
        let world = world.commit();
        assert_eq!(world.has_cutouts(), cutout_elsewhere);
        let n = 1024;
        let mut sum = 0.0f64;
        for i in 0..n {
            sum += ray_color(
                &ray,
                &world,
                &lights,
                &Volumes::default(),
                3,
                SamplingStrategy::LightOnly,
                PathSampler::new(1, 2, 0, i),
            )
            .x as f64;
        }
        let mean = sum / n as f64;
        if sheet {
            assert!(mean < 1e-3, "cutouts elsewhere {cutout_elsewhere}: {mean}");
        } else {
            assert!(mean > 0.1, "the open floor is lit: {mean}");
        }
    }
}

/// The tangent a leaf shades with, by category.
fn tangents(l: &Loaded, category: &str) -> Vec<Vec3A> {
    find(&leaves(l, 0.5, 0.5), category)
        .iter()
        .map(|p| p.frame.t)
        .collect()
}

fn same_direction(a: Vec3A, b: Vec3A) -> bool {
    (a - b).abs().max_element() < 1e-5
}

/// `standard_surface` turns its tangent by `specular_rotation` of a full turn
/// about the normal — clockwise seen from above, since `rotate3d` is
/// Rodrigues' formula at minus its angle — and only where the lobe is
/// anisotropic. The coat turns by its own rotation, the diffuse not at all.
#[test]
fn standard_surface_rotates_its_anisotropic_tangents() {
    let l = load(
        "rotate",
        r#"<standard_surface name="s" type="surfaceshader">
             <input name="metalness" type="float" value="0.5" />
             <input name="specular_anisotropy" type="float" value="0.5" />
             <input name="specular_rotation" type="float" value="0.25" />
             <input name="coat" type="float" value="1" />
             <input name="coat_anisotropy" type="float" value="0.3" />
             <input name="coat_rotation" type="float" value="0.125" />
           </standard_surface>"#,
    );
    assert!(l.reported.is_empty(), "{:?}", l.reported);
    let quarter = Vec3A::new(0.0, -1.0, 0.0);
    for t in tangents(&l, "conductor_bsdf") {
        assert!(same_direction(t, quarter), "metal {t}");
    }
    let eighth = Vec3A::new(0.5f32.sqrt(), -(0.5f32.sqrt()), 0.0);
    let ts = tangents(&l, "dielectric_bsdf");
    assert_eq!(ts.len(), 2, "specular and coat");
    assert!(
        ts.iter().any(|t| same_direction(*t, quarter)),
        "specular {ts:?}"
    );
    assert!(ts.iter().any(|t| same_direction(*t, eighth)), "coat {ts:?}");
    for t in tangents(&l, "oren_nayar_diffuse_bsdf") {
        assert!(same_direction(t, Vec3A::X), "diffuse {t}");
    }

    // Isotropic: the graph's `ifgreater` keeps the tangent.
    let iso = load(
        "rotate_iso",
        r#"<standard_surface name="s" type="surfaceshader">
             <input name="specular_rotation" type="float" value="0.25" />
           </standard_surface>"#,
    );
    for t in tangents(&iso, "dielectric_bsdf") {
        assert!(same_direction(t, Vec3A::X), "isotropic {t}");
    }
}

/// glTF's `anisotropy_rotation` is radians counter-clockwise from the
/// tangent toward the bitangent, on every base leaf; the clearcoat keeps the
/// authored tangent, as its graph does.
#[test]
fn gltf_rotates_its_base_tangent_not_the_clearcoat() {
    let l = load(
        "gltf_rotate",
        r#"<gltf_pbr name="s" type="surfaceshader">
             <input name="metallic" type="float" value="0.5" />
             <input name="anisotropy_strength" type="float" value="0.6" />
             <input name="anisotropy_rotation" type="float" value="1.5707964" />
             <input name="clearcoat" type="float" value="1" />
           </gltf_pbr>"#,
    );
    assert!(l.reported.is_empty(), "{:?}", l.reported);
    let ts = tangents(&l, "generalized_schlick_bsdf");
    assert_eq!(ts.len(), 2, "reflection and metal");
    for t in ts {
        assert!(same_direction(t, Vec3A::Y), "base {t}");
    }
    for t in tangents(&l, "dielectric_bsdf") {
        assert!(same_direction(t, Vec3A::X), "clearcoat {t}");
    }
}

/// A rotated anisotropic lobe samples and evaluates in the same turned
/// frame, so it stays bounded in the furnace like the unrotated one.
#[test]
fn a_rotated_anisotropic_metal_is_bounded_in_the_furnace() {
    let l = load(
        "rotate_furnace",
        r#"<standard_surface name="s" type="surfaceshader">
             <input name="base_color" type="color3" value="1, 1, 1" />
             <input name="metalness" type="float" value="1" />
             <input name="specular_roughness" type="float" value="0.4" />
             <input name="specular_anisotropy" type="float" value="0.9" />
             <input name="specular_rotation" type="float" value="0.3" />
           </standard_surface>"#,
    );
    for (offset, r) in FURNACE_OFFSETS.iter().zip(furnace(&l)) {
        assert!(r.max_element() < 1.02, "offset {offset}: {r}");
        assert!(r.min_element() > 0.8, "offset {offset}: {r}");
    }
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

/// The impact offsets the white furnace aims at: head-on, mid-way, grazing.
const FURNACE_OFFSETS: [f32; 3] = [0.0, 0.6, 0.95];

/// The white furnace: `l`'s material on a unit sphere inside a uniform
/// emitter of radiance 1, traced unclamped (`ray_color` applies no firefly
/// clamp). Returns the mean radiance at each of [`FURNACE_OFFSETS`] — on a
/// convex surface, the material's directional albedo there.
fn furnace(l: &Loaded) -> [Vec3A; 3] {
    let mut world = WorldBuilder::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 1.0,
        },
        l.material.clone(),
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
    FURNACE_OFFSETS.map(|offset| {
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
        sum / n as f32
    })
}

/// Every fixture material in the white furnace. A closure that is
/// energy-conserving reflects and transmits at most what arrives, so no
/// pixel — head-on, mid-way or grazing — may come back brighter than the
/// environment behind it. A layer that added its top's and base's full
/// responses, or a leaf whose sample weight disagreed with its `eval / pdf`,
/// reads above 1 here.
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
        for (offset, mean) in FURNACE_OFFSETS.into_iter().zip(furnace(&loaded)) {
            assert!(
                mean.max_element() <= 1.02 && mean.min_element() >= 0.0,
                "{name} at offset {offset}: furnace mean {mean}"
            );
        }
    }
}

/// The DPEL Teapot's look in miniature, and in white: a dust diffuse and a
/// stain diffuse, each mixed by its mask against a zero-weight dielectric —
/// the assets' idiom for turning a mask into a *coverage* — over a smooth
/// glaze over the body. The Lion puts the same dust mix under a sheen.
const COVERAGE_STACK: &str = r#"
  <oren_nayar_diffuse_bsdf name="dust" type="BSDF">
    <input name="color" type="color3" value="1, 1, 1" />
    <input name="roughness" type="float" value="0.544" />
  </oren_nayar_diffuse_bsdf>
  <dielectric_bsdf name="dust_dummy" type="BSDF">
    <input name="ior" type="float" value="1" />
    <input name="weight" type="float" value="0" />
  </dielectric_bsdf>
  <mix name="dust_mix" type="BSDF">
    <input name="fg" type="BSDF" nodename="dust" />
    <input name="bg" type="BSDF" nodename="dust_dummy" />
    <input name="mix" type="float" value="0.3" />
  </mix>
  <oren_nayar_diffuse_bsdf name="stain" type="BSDF">
    <input name="color" type="color3" value="1, 1, 1" />
    <input name="roughness" type="float" value="0.268" />
  </oren_nayar_diffuse_bsdf>
  <dielectric_bsdf name="stain_dummy" type="BSDF">
    <input name="ior" type="float" value="1" />
    <input name="weight" type="float" value="0" />
  </dielectric_bsdf>
  <mix name="stain_mix" type="BSDF">
    <input name="fg" type="BSDF" nodename="stain" />
    <input name="bg" type="BSDF" nodename="stain_dummy" />
    <input name="mix" type="float" value="0.4" />
  </mix>
  <dielectric_bsdf name="glaze" type="BSDF">
    <input name="ior" type="float" value="1.48" />
    <input name="roughness" type="vector2" value="0.002, 0.002" />
  </dielectric_bsdf>
  <oren_nayar_diffuse_bsdf name="body" type="BSDF">
    <input name="color" type="color3" value="1, 1, 1" />
  </oren_nayar_diffuse_bsdf>
  <layer name="glazing" type="BSDF">
    <input name="top" type="BSDF" nodename="glaze" />
    <input name="base" type="BSDF" nodename="body" />
  </layer>
  <layer name="stained" type="BSDF">
    <input name="top" type="BSDF" nodename="stain_mix" />
    <input name="base" type="BSDF" nodename="glazing" />
  </layer>
  <layer name="dusted" type="BSDF">
    <input name="top" type="BSDF" nodename="dust_mix" />
    <input name="base" type="BSDF" nodename="stained" />
  </layer>
  <surface name="s" type="surfaceshader">
    <input name="bsdf" type="BSDF" nodename="dusted" />
  </surface>"#;

#[test]
fn a_masked_coverage_keeps_what_lies_beneath_it() {
    let l = load("coverage", COVERAGE_STACK);
    // In numbers: each coverage passes `1 − mask` of what is below it, the
    // glaze passes `1 − E_R`, and the dummies contribute nothing else.
    let ls = leaves(&l, 0.5, 0.5);
    let w = |category: &str, i: usize| find(&ls, category)[i].weight.x;
    let (stain, glaze) = (0.7 * 0.4, 0.7 * 0.6);
    let body = glaze * closure::dielectric_refl_filter(1.0, 0.002f32.sqrt(), 1.48);
    assert_eq!(ls.len(), 4, "dust, stain, glaze and body");
    assert!(near(w("oren_nayar_diffuse_bsdf", 0), 0.3, 1e-6), "dust");
    assert!(near(w("oren_nayar_diffuse_bsdf", 1), stain, 1e-6), "stain");
    assert!(near(w("dielectric_bsdf", 0), glaze, 1e-6), "glaze");
    assert!(near(w("oren_nayar_diffuse_bsdf", 2), body, 1e-6), "body");
    // In the furnace: every layer is white, so nearly all the light comes
    // back (0.90 to 0.96: the dust's and stain's Oren–Nayar albedo is the
    // only loss) and never more than arrives. Rewriting the pruned dummies
    // away gave the dust layer's top the diffuse's throughput of 0, and this
    // read the dust alone, 0.23 to 0.27.
    for (offset, mean) in FURNACE_OFFSETS.into_iter().zip(furnace(&l)) {
        assert!(
            mean.max_element() <= 1.02 && mean.min_element() > 0.8,
            "offset {offset}: furnace mean {mean}"
        );
    }
}

fn subsurface_fixture(name: &str) -> Loaded {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/materialx_subsurface.mtlx");
    materialx::load(&path, Some(name), &|_, _| None).expect("loads")
}

/// `(color, radius, anisotropy, ior, alpha)` of the one random-walk leaf.
fn walk_leaf(l: &Loaded) -> (Vec3A, Vec3A, f32, f32, f32) {
    let ls = leaves(l, 0.5, 0.5);
    let walks: Vec<_> = ls
        .iter()
        .filter_map(|p| match p.lobe {
            Lobe::Subsurface {
                color,
                radius,
                anisotropy,
                ior,
                alpha,
            } => Some((color, radius, anisotropy, ior, alpha)),
            _ => None,
        })
        .collect();
    assert_eq!(
        walks.len(),
        1,
        "{:?}",
        ls.iter().map(|p| p.describe()).collect::<Vec<_>>()
    );
    walks[0]
}

/// A `subsurface_bsdf` is a random walk now, with its radius and anisotropy
/// as authored, entered through the dielectric layered over it — or, bare,
/// through Typhoon's defaults — and no longer reported as approximated.
#[test]
fn a_subsurface_bsdf_resolves_to_a_random_walk() {
    let jade = subsurface_fixture("mtlx_bare_jade");
    let (color, radius, g, ior, alpha) = walk_leaf(&jade);
    assert!((color - Vec3A::new(0.3, 0.75, 0.45)).abs().max_element() < 1e-5);
    assert!((radius - Vec3A::new(0.15, 0.3, 0.2)).abs().max_element() < 1e-5);
    assert!(near(g, 0.3, 1e-6) && near(ior, 1.5, 1e-6) && near(alpha, 0.25, 1e-6));
    assert!(jade.reported.is_empty(), "{:?}", jade.reported);

    // OpenPBR: radius × radius_scale, through the specular dielectric
    // (`specular_ior` 1.5 by default, roughness 0.4 → α 0.16).
    let skin = subsurface_fixture("mtlx_openpbr_skin");
    let (color, radius, _, ior, alpha) = walk_leaf(&skin);
    assert!((color - Vec3A::new(0.9, 0.6, 0.45)).abs().max_element() < 1e-5);
    assert!((radius - Vec3A::new(0.2, 0.1, 0.05)).abs().max_element() < 1e-5);
    assert!(
        near(ior, 1.5, 1e-5) && near(alpha, 0.16, 1e-5),
        "{ior} {alpha}"
    );
    assert!(skin.reported.is_empty(), "{:?}", skin.reported);

    // Standard Surface: radius × subsurface_scale.
    let marble = subsurface_fixture("mtlx_standard_marble");
    let (_, radius, _, ior, _) = walk_leaf(&marble);
    assert!((radius - Vec3A::splat(0.3)).abs().max_element() < 1e-5);
    assert!(near(ior, 1.5, 1e-5));
}

/// The walk is only a direction and a weight at the entry: no leaf value
/// toward any light, so NEE at the entry sees nothing of it.
#[test]
fn a_random_walk_leaf_has_no_value_toward_a_light() {
    let jade = subsurface_fixture("mtlx_bare_jade");
    let (r, rec) = (straight_down(), hit(0.5, 0.5));
    let wi = Vec3A::new(0.3, 0.2, 0.9).normalize();
    let (value, _) = jade.material.eval(&r, &rec, wi).expect("has leaves");
    assert_eq!(value, Vec3A::ZERO);
}

/// A zero radius exits where it enters: the leaf falls back to a diffuse in
/// its colour instead of starting a walk that could go nowhere.
#[test]
fn a_zero_radius_subsurface_is_a_diffuse() {
    let l = load(
        "sss0",
        r#"<subsurface_bsdf name="b" type="BSDF">
             <input name="color" type="color3" value="0.5, 0.5, 0.5" />
             <input name="radius" type="vector3" value="0, 0, 0" />
           </subsurface_bsdf>
           <surface name="s" type="surfaceshader">
             <input name="bsdf" type="BSDF" nodename="b" />
           </surface>"#,
    );
    let ls = leaves(&l, 0.5, 0.5);
    assert!(
        matches!(ls[0].lobe, Lobe::Diffuse { .. }),
        "{}",
        ls[0].describe()
    );
}

/// Every subsurface fixture material in the white furnace, through the whole
/// walk: nothing comes back brighter than the environment.
#[test]
fn every_subsurface_fixture_is_bounded_in_a_white_furnace() {
    for name in [
        "mtlx_openpbr_skin",
        "mtlx_standard_marble",
        "mtlx_bare_jade",
    ] {
        let loaded = subsurface_fixture(name);
        for (offset, mean) in FURNACE_OFFSETS.into_iter().zip(furnace(&loaded)) {
            assert!(
                mean.max_element() <= 1.02 && mean.min_element() >= 0.0,
                "{name} at offset {offset}: furnace mean {mean}"
            );
        }
    }
}

/// Through the integrator, a walk whose mean free path is small beside the
/// object (a unit sphere at radius 0.01 is nearly a slab) reflects what the
/// walk alone does from a straight-down entry — the refraction through IOR
/// 1.5 nearly is one: (0.78, 0.45, 0.16) for a colour of (0.8, 0.5, 0.2).
/// That is Typhoon's entry, and below Chiang's fit, which describes a diffuse
/// entry (`subsurface::tests`); the gap is measured and recorded in the
/// materials design record, not hidden in a tolerance.
#[test]
fn a_short_walk_reflects_its_colour_in_the_furnace() {
    let l = load(
        "sssfurnace",
        r#"<subsurface_bsdf name="b" type="BSDF">
             <input name="color" type="color3" value="0.8, 0.5, 0.2" />
             <input name="radius" type="vector3" value="0.01, 0.01, 0.01" />
           </subsurface_bsdf>
           <surface name="s" type="surfaceshader">
             <input name="bsdf" type="BSDF" nodename="b" />
           </surface>"#,
    );
    let [head_on, mid, _] = furnace(&l);
    for mean in [head_on, mid] {
        let err = (mean - Vec3A::new(0.777, 0.446, 0.163)).abs().max_element();
        assert!(err < 0.025, "furnace mean {mean}");
    }
}

/// The walk enters through the dielectric that is live at the hit, not the
/// first one in the tree: under a `mix` whose factor (here the `u`
/// coordinate) is 0, the foreground interface contributes nothing and must
/// not set the entry's IOR.
#[test]
fn an_inactive_dielectric_does_not_set_the_walk_entry() {
    let l = load(
        "sssiface",
        r#"<texcoord name="tc" type="vector2" />
           <extract name="u" type="float">
             <input name="in" type="vector2" nodename="tc" />
             <input name="index" type="integer" value="0" />
           </extract>
           <dielectric_bsdf name="glossy" type="BSDF">
             <input name="ior" type="float" value="2.5" />
             <input name="roughness" type="vector2" value="0.05, 0.05" />
           </dielectric_bsdf>
           <dielectric_bsdf name="satin" type="BSDF">
             <input name="ior" type="float" value="1.3" />
             <input name="roughness" type="vector2" value="0.3, 0.3" />
           </dielectric_bsdf>
           <mix name="coat" type="BSDF">
             <input name="fg" type="BSDF" nodename="glossy" />
             <input name="bg" type="BSDF" nodename="satin" />
             <input name="mix" type="float" nodename="u" />
           </mix>
           <subsurface_bsdf name="sss" type="BSDF">
             <input name="color" type="color3" value="0.8, 0.6, 0.5" />
             <input name="radius" type="vector3" value="0.1, 0.1, 0.1" />
           </subsurface_bsdf>
           <layer name="stack" type="BSDF">
             <input name="top" type="BSDF" nodename="coat" />
             <input name="base" type="BSDF" nodename="sss" />
           </layer>
           <surface name="s" type="surfaceshader">
             <input name="bsdf" type="BSDF" nodename="stack" />
           </surface>"#,
    );
    let entry_ior = |u: f32| {
        leaves(&l, u, 0.5)
            .iter()
            .find_map(|p| match p.lobe {
                Lobe::Subsurface { ior, .. } => Some(ior),
                _ => None,
            })
            .expect("a walk leaf")
    };
    assert!(near(entry_ior(0.0), 1.3, 1e-5), "{}", entry_ior(0.0));
    assert!(near(entry_ior(1.0), 2.5, 1e-5), "{}", entry_ior(1.0));
    // Mostly background: the heavier branch sets the interface.
    assert!(near(entry_ior(0.2), 1.3, 1e-5), "{}", entry_ior(0.2));
}
