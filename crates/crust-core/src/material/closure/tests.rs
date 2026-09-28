use std::f32::consts::PI;

use glam::Vec3A;

use super::*;
use crate::PathSampler;
use crust_mtlx::{Compiler, Doc, ShadeCtx, flatten};

/// A fresh QMC domain per draw, as the integrator derives one per vertex.
struct S(i32);
impl S {
    fn next(&mut self) -> PathSampler {
        self.0 += 1;
        PathSampler::new(0, 0, 0, self.0)
    }
}

/// Compiles `root` of an inline document and evaluates its program.
fn tree(doc: &str, root: &str) -> (Closures, Vec<Val>) {
    let d = Doc::parse(doc).expect("document parses");
    let loader = |_: &str, _: Option<&str>| None;
    let mut c = Compiler::new(&d, &loader);
    let node = d.find("", root).expect("root").clone();
    let mut out = Closures::default();
    flatten(&mut c, &node, &mut out);
    let mut slots = Vec::new();
    c.program.eval(
        &ShadeCtx {
            uv: (0.5, 0.5),
            normal: Vec3A::Z,
            tangent: Vec3A::X,
            view: -Vec3A::Z,
            position: Vec3A::ZERO,
            uv_width: 0.0,
        },
        &mut slots,
    );
    (out, slots)
}

fn hit(front: bool) -> HitRecord {
    let mut rec = HitRecord::new();
    rec.p = Vec3A::ZERO;
    rec.normal = Vec3A::Z;
    rec.tangent = Vec3A::X;
    rec.front_face = front;
    rec
}

/// A ray arriving at the origin at `theta` from the normal.
fn arriving(theta: f32) -> Ray {
    let d = Vec3A::new(theta.sin(), 0.0, theta.cos());
    Ray::new(d, -d)
}

fn resolved(doc: &str, root: &str, theta: f32, front: bool) -> ResolvedClosure {
    let (cl, slots) = tree(doc, root);
    ResolvedClosure::resolve(&cl, &slots, &arriving(theta), &hit(front))
}

/// `E[value / pdf]` over `n` BSDF samples: the directional albedo.
fn albedo(c: &ResolvedClosure, theta: f32, front: bool, n: usize) -> Vec3A {
    let mut s = S(0);
    let r = arriving(theta);
    let rec = hit(front);
    let mut sum = Vec3A::ZERO;
    for _ in 0..n {
        if let Some(x) = c.scatter(&r, &rec, s.next()) {
            sum += if x.delta { x.value } else { x.value / x.pdf };
        }
    }
    sum / n as f32
}

fn doc(body: &str) -> String {
    format!("<materialx>{body}</materialx>")
}

const LEAVES: &[(&str, &str)] = &[
    (
        "eon",
        r#"<oren_nayar_diffuse_bsdf name="x" type="BSDF">
             <input name="color" type="color3" value="1, 1, 1" />
             <input name="roughness" type="float" value="0.7" />
             <input name="energy_compensation" type="boolean" value="true" />
           </oren_nayar_diffuse_bsdf>"#,
    ),
    (
        "oren-nayar",
        r#"<oren_nayar_diffuse_bsdf name="x" type="BSDF">
             <input name="color" type="color3" value="1, 1, 1" />
             <input name="roughness" type="float" value="0.5" />
           </oren_nayar_diffuse_bsdf>"#,
    ),
    (
        "burley",
        r#"<burley_diffuse_bsdf name="x" type="BSDF">
             <input name="color" type="color3" value="1, 1, 1" />
           </burley_diffuse_bsdf>"#,
    ),
    (
        "dielectric R",
        r#"<dielectric_bsdf name="x" type="BSDF">
             <input name="roughness" type="vector2" value="0.2, 0.2" />
           </dielectric_bsdf>"#,
    ),
    (
        "dielectric RT",
        r#"<dielectric_bsdf name="x" type="BSDF">
             <input name="roughness" type="vector2" value="0.1, 0.1" />
             <input name="scatter_mode" type="string" value="RT" />
           </dielectric_bsdf>"#,
    ),
    (
        "white schlick",
        r#"<generalized_schlick_bsdf name="x" type="BSDF">
             <input name="roughness" type="vector2" value="0.3, 0.3" />
           </generalized_schlick_bsdf>"#,
    ),
    (
        "conductor",
        r#"<conductor_bsdf name="x" type="BSDF">
             <input name="roughness" type="vector2" value="0.25, 0.25" />
           </conductor_bsdf>"#,
    ),
    (
        "sheen",
        r#"<sheen_bsdf name="x" type="BSDF">
             <input name="roughness" type="float" value="0.4" />
           </sheen_bsdf>"#,
    ),
    (
        "translucent",
        r#"<translucent_bsdf name="x" type="BSDF" />"#,
    ),
];

#[test]
fn no_leaf_reflects_more_than_it_receives() {
    for (name, body) in LEAVES {
        for theta in [0.1f32, 0.8, 1.3] {
            let c = resolved(&doc(body), "x", theta, true);
            let a = albedo(&c, theta, true, 4096);
            assert!(
                a.max_element() < 1.03,
                "{name} at θ={theta}: albedo {a} exceeds 1"
            );
        }
    }
}

#[test]
fn energy_conserving_leaves_are_white_on_white() {
    // EON with white albedo and a compensated F=1 microfacet both reflect
    // everything they receive; a smooth-enough RT dielectric reflects plus
    // transmits all of it.
    for (name, theta, tol) in [
        ("eon", 0.4f32, 0.03f32),
        ("white schlick", 0.4, 0.05),
        ("dielectric RT", 0.4, 0.05),
    ] {
        let body = LEAVES.iter().find(|(n, _)| *n == name).unwrap().1;
        let c = resolved(&doc(body), "x", theta, true);
        let a = albedo(&c, theta, true, 8192);
        assert!(
            (a - Vec3A::ONE).abs().max_element() < tol,
            "{name}: albedo {a}, expected white"
        );
    }
}

#[test]
fn every_sample_agrees_with_eval() {
    let mixed = doc(r#"
      <oren_nayar_diffuse_bsdf name="d" type="BSDF">
        <input name="color" type="color3" value="0.6, 0.3, 0.2" />
        <input name="energy_compensation" type="boolean" value="true" />
      </oren_nayar_diffuse_bsdf>
      <dielectric_bsdf name="g" type="BSDF">
        <input name="roughness" type="vector2" value="0.3, 0.1" />
      </dielectric_bsdf>
      <sheen_bsdf name="s" type="BSDF"><input name="weight" type="float" value="0.5" /></sheen_bsdf>
      <layer name="l" type="BSDF">
        <input name="top" type="BSDF" nodename="g" />
        <input name="base" type="BSDF" nodename="d" />
      </layer>
      <layer name="x" type="BSDF">
        <input name="top" type="BSDF" nodename="s" />
        <input name="base" type="BSDF" nodename="l" />
      </layer>"#);
    let mut docs: Vec<String> = LEAVES.iter().map(|(_, b)| doc(b)).collect();
    docs.push(mixed);
    for d in &docs {
        let c = resolved(d, "x", 0.6, true);
        let r = arriving(0.6);
        let rec = hit(true);
        let mut s = S(0);
        let mut checked = 0;
        for _ in 0..256 {
            let Some(x) = c.scatter(&r, &rec, s.next()) else {
                continue;
            };
            if x.delta {
                continue;
            }
            let wi = x.ray.direction().normalize();
            let (v, p) = c
                .eval(&r, &rec, wi)
                .expect("a continuous closure is evaluable");
            let tol = 1e-3 * (1.0 + x.value.max_element());
            assert!(
                (v - x.value).abs().max_element() < tol,
                "{v} vs {}",
                x.value
            );
            assert!((p - x.pdf).abs() < 1e-3 * (1.0 + p), "{p} vs {}", x.pdf);
            checked += 1;
        }
        assert!(checked > 64, "too few valid samples in {d}");
    }
}

#[test]
fn the_mixture_pdf_integrates_to_one() {
    // A reflect-only tree: every leaf's density is a normalised distribution
    // over the sphere, so their mixture integrates to one. Estimated with
    // uniform-sphere directions (pdf 1/4π).
    let body = doc(r#"
      <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
      <dielectric_bsdf name="g" type="BSDF">
        <input name="roughness" type="vector2" value="0.5, 0.5" />
      </dielectric_bsdf>
      <layer name="x" type="BSDF">
        <input name="top" type="BSDF" nodename="g" />
        <input name="base" type="BSDF" nodename="d" />
      </layer>"#);
    let c = resolved(&body, "x", 0.5, true);
    let r = arriving(0.5);
    let rec = hit(true);
    let n = 200_000;
    let mut sum = 0.0f64;
    for i in 0..n {
        // A stratified uniform sphere.
        let u = (i as f32 + 0.5) / n as f32;
        let z = 1.0 - 2.0 * u;
        let phi = 2.0 * PI * ((i as f32 * 0.618_034) % 1.0);
        let s = (1.0 - z * z).max(0.0).sqrt();
        let wi = Vec3A::new(s * phi.cos(), s * phi.sin(), z);
        sum += c.eval(&r, &rec, wi).unwrap().1 as f64;
    }
    let integral = sum / n as f64 * 4.0 * std::f64::consts::PI;
    // `eval` floors the pdf at 1e-4 on the lower hemisphere.
    assert!((integral - 1.0).abs() < 0.03, "∫pdf = {integral}");
}

#[test]
fn a_coat_dims_what_lies_beneath_it_at_grazing_angles() {
    let body = doc(r#"
      <dielectric_bsdf name="g" type="BSDF">
        <input name="roughness" type="vector2" value="0.0001, 0.0001" />
      </dielectric_bsdf>
      <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
      <layer name="x" type="BSDF">
        <input name="top" type="BSDF" nodename="g" />
        <input name="base" type="BSDF" nodename="d" />
      </layer>"#);
    let base_weight = |theta: f32| {
        let c = resolved(&body, "x", theta, true);
        let d = c
            .leaves()
            .iter()
            .find(|l| l.category == "oren_nayar_diffuse_bsdf")
            .unwrap();
        d.weight.x
    };
    let normal = base_weight(0.0);
    let grazing = base_weight(1.45);
    // `1 − E_R`: about 0.96 head on for IOR 1.5, far less at grazing.
    let expected = dielectric_refl_filter(1.0, 0.01, 1.5);
    assert!((normal - expected).abs() < 1e-4, "{normal} vs {expected}");
    assert!((0.93..0.97).contains(&normal), "head-on 1 − E_R = {normal}");
    assert!(
        grazing < normal - 0.2,
        "grazing {grazing} vs normal {normal}"
    );
}

#[test]
fn an_opaque_top_hides_the_base_entirely() {
    // MaterialX: a conductor's throughput is 0 whatever its weight.
    let body = doc(r#"
      <conductor_bsdf name="c" type="BSDF"><input name="weight" type="float" value="0.5" /></conductor_bsdf>
      <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
      <layer name="x" type="BSDF">
        <input name="top" type="BSDF" nodename="c" />
        <input name="base" type="BSDF" nodename="d" />
      </layer>"#);
    let c = resolved(&body, "x", 0.3, true);
    assert_eq!(
        c.leaves().len(),
        1,
        "the base is invisible under an opaque top"
    );
    assert_eq!(c.leaves()[0].category, "conductor_bsdf");
}

#[test]
fn a_multiply_scales_response_but_not_throughput() {
    let body = doc(r#"
      <dielectric_bsdf name="g" type="BSDF" />
      <multiply name="half" type="BSDF">
        <input name="in1" type="BSDF" nodename="g" />
        <input name="in2" type="float" value="0.5" />
      </multiply>
      <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
      <layer name="x" type="BSDF">
        <input name="top" type="BSDF" nodename="half" />
        <input name="base" type="BSDF" nodename="d" />
      </layer>
      <layer name="y" type="BSDF">
        <input name="top" type="BSDF" nodename="g" />
        <input name="base" type="BSDF" nodename="d" />
      </layer>"#);
    let weight = |root: &str, cat: &str| {
        resolved(&body, root, 0.3, true)
            .leaves()
            .iter()
            .find(|l| l.category == cat)
            .unwrap()
            .weight
            .x
    };
    assert!((weight("x", "dielectric_bsdf") - 0.5).abs() < 1e-6);
    let (dx, dy) = (
        weight("x", "oren_nayar_diffuse_bsdf"),
        weight("y", "oren_nayar_diffuse_bsdf"),
    );
    assert!((dx - dy).abs() < 1e-6, "{dx} vs {dy}");
}

#[test]
fn a_coat_normal_perturbs_only_the_coat() {
    let body = doc(r#"
      <constant name="n" type="vector3"><input name="value" type="vector3" value="0.3, 0, 0.954" /></constant>
      <dielectric_bsdf name="g" type="BSDF">
        <input name="normal" type="vector3" nodename="n" />
      </dielectric_bsdf>
      <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
      <layer name="x" type="BSDF">
        <input name="top" type="BSDF" nodename="g" />
        <input name="base" type="BSDF" nodename="d" />
      </layer>"#);
    let c = resolved(&body, "x", 0.3, true);
    for l in c.leaves() {
        if l.category == "dielectric_bsdf" {
            assert!((l.frame.n - Vec3A::new(0.3, 0.0, 0.954).normalize()).length() < 1e-5);
        } else {
            assert_eq!(
                l.frame.n,
                Vec3A::Z,
                "the base keeps the interpolated normal"
            );
        }
    }
}

#[test]
fn an_authored_tangent_orients_the_leaf_frame() {
    let body = doc(r#"
      <constant name="t" type="vector3"><input name="value" type="vector3" value="0, 1, 0" /></constant>
      <dielectric_bsdf name="x" type="BSDF">
        <input name="roughness" type="vector2" value="0.5, 0.05" />
        <input name="tangent" type="vector3" nodename="t" />
      </dielectric_bsdf>"#);
    let c = resolved(&body, "x", 0.3, true);
    assert!((c.leaves()[0].frame.t - Vec3A::Y).length() < 1e-6);
    // The highlight stretches along the tangent: an in-plane light along +Y
    // sees more of the lobe than one along +X at the same elevation.
    let r = arriving(0.0);
    let rec = hit(true);
    let el = 0.6f32;
    let along_y = c
        .eval(&r, &rec, Vec3A::new(0.0, el.sin(), el.cos()))
        .unwrap()
        .0;
    let along_x = c
        .eval(&r, &rec, Vec3A::new(el.sin(), 0.0, el.cos()))
        .unwrap()
        .0;
    assert!(along_y.x > along_x.x * 2.0, "{along_y} vs {along_x}");
}

const GLASS: &str = r#"<materialx>
  <dielectric_bsdf name="g" type="BSDF">
    <input name="roughness" type="vector2" value="0.05, 0.05" />
    <input name="scatter_mode" type="string" value="RT" />
  </dielectric_bsdf>
  <anisotropic_vdf name="v" type="VDF">
    <input name="absorption" type="vector3" value="0, 1, 2" />
  </anisotropic_vdf>
  <layer name="l" type="BSDF">
    <input name="top" type="BSDF" nodename="g" />
    <input name="base" type="VDF" nodename="v" />
  </layer>
  <surface name="thick" type="surfaceshader">
    <input name="bsdf" type="BSDF" nodename="l" />
  </surface>
  <surface name="thin" type="surfaceshader">
    <input name="bsdf" type="BSDF" nodename="l" />
    <input name="thin_walled" type="boolean" value="true" />
  </surface>
</materialx>"#;

#[test]
fn a_refracted_ray_carries_the_interior_medium() {
    let c = resolved(GLASS, "thick", 0.2, true);
    assert!(c.transmits());
    let m = c.medium().expect("the VDF is the interior");
    assert_eq!(m.sigma_a, Vec3A::new(0.0, 1.0, 2.0));
    let mut s = S(0);
    let (r, rec) = (arriving(0.2), hit(true));
    let mut refracted = 0;
    for _ in 0..256 {
        let Some(x) = c.scatter(&r, &rec, s.next()) else {
            continue;
        };
        if x.ray.direction().z < 0.0 {
            assert!(
                x.ray.medium().is_some(),
                "a refracted ray enters the medium"
            );
            refracted += 1;
        } else {
            assert!(x.ray.medium().is_none(), "a reflected ray stays outside");
        }
    }
    assert!(refracted > 128, "glass mostly refracts: {refracted}");
    let guided = c.make_ray(&rec, Vec3A::new(0.0, 0.1, -1.0).normalize());
    assert!(
        guided.medium().is_some(),
        "make_ray tags a guided refraction too"
    );
}

#[test]
fn a_thin_wall_transmits_straight_through_without_a_medium() {
    let c = resolved(GLASS, "thin", 0.4, true);
    assert!(c.medium().is_none());
    let mut s = S(0);
    let (r, rec) = (arriving(0.4), hit(true));
    let mut through = 0;
    for _ in 0..256 {
        if let Some(x) = c.scatter(&r, &rec, s.next())
            && x.delta
        {
            assert!((x.ray.direction().normalize() - r.direction()).length() < 1e-5);
            assert!(x.ray.medium().is_none());
            through += 1;
        }
    }
    assert!(through > 128, "a clear sheet mostly transmits: {through}");
}

#[test]
fn the_bsdl_filter_matches_its_grid_points() {
    // Grid point (ior index 0, roughness 0, cos index 1) of the baked table.
    let eta = 1.001f32;
    let f = dielectric_refl_filter(1.0 / 15.0, 0.0, eta);
    assert!((f - 0.991_504_25).abs() < 1e-6, "{f}");
    // A smooth IOR-1.5 interface head on: 1 − 0.04.
    let head_on = dielectric_refl_filter(1.0, 0.0, 1.5);
    assert!((head_on - 0.96).abs() < 5e-3, "{head_on}");
    // Grazing reflects more, so the filter drops.
    assert!(dielectric_refl_filter(0.05, 0.0, 1.5) < 0.6);
    // Symmetric in η ↔ 1/η, as BSDL's index is.
    assert_eq!(
        dielectric_refl_filter(0.5, 0.3, 1.5),
        dielectric_refl_filter(0.5, 0.3, 1.0 / 1.5)
    );
}
