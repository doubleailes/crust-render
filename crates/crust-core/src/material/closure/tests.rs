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
    let mut c = Compiler::new(&d, &crust_mtlx::Host::new(&loader));
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
    ResolvedClosure::resolve(
        &cl,
        &slots,
        &arriving(theta),
        &hit(front),
        utils::Luma::REC709,
    )
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
    (
        "hair",
        r#"<chiang_hair_bsdf name="x" type="BSDF">
             <input name="absorption_coefficient" type="vector3" value="0.1, 0.2, 0.4" />
             <input name="cuticle_angle" type="float" value="0.52" />
           </chiang_hair_bsdf>"#,
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

/// The DPEL Teapot's dust and stain layers, and the Lion's: an opaque diffuse
/// mixed by a mask against a zero-weight dielectric, layered over the rest of
/// the look. `dummy` is the branch that makes the mask a *coverage*: the
/// dielectric at weight 0 is MaterialX's `BSDF(0, 1)` — no response, full
/// throughput — so the layer's top lets `1 − m` of the base through.
const COVERAGE: &str = r#"
  <oren_nayar_diffuse_bsdf name="dust" type="BSDF">
    <input name="color" type="color3" value="1, 1, 1" />
    <input name="roughness" type="float" value="0.544" />
  </oren_nayar_diffuse_bsdf>
  <dielectric_bsdf name="dummy" type="BSDF">
    <input name="ior" type="float" value="1" />
    <input name="weight" type="float" WEIGHT />
  </dielectric_bsdf>
  <constant name="zero" type="float"><input name="value" type="float" value="0" /></constant>
  <mix name="coverage" type="BSDF">
    <input name="FG" type="BSDF" nodename="dust" />
    <input name="BG" type="BSDF" nodename="dummy" />
    <input name="mix" type="float" value="0.25" />
  </mix>
  <oren_nayar_diffuse_bsdf name="body" type="BSDF">
    <input name="color" type="color3" value="0.005, 0.024, 0.074" />
  </oren_nayar_diffuse_bsdf>
  <layer name="x" type="BSDF">
    <input name="top" type="BSDF" nodename="coverage" />
    <input name="base" type="BSDF" nodename="body" />
  </layer>"#;

/// [`COVERAGE`] with the dummy's weight a literal 0 (`pruned`, which the
/// compiler drops) or connected to a constant 0 (kept as a live leaf), and
/// the dust as the mix's `fg` or its `bg`.
fn coverage(pruned: bool, dust_is_fg: bool) -> String {
    let weight = if pruned {
        r#"value="0""#
    } else {
        r#"nodename="zero""#
    };
    let (fg, bg) = if dust_is_fg {
        ("fg", "bg")
    } else {
        ("bg", "fg")
    };
    doc(&COVERAGE
        .replace("WEIGHT", weight)
        .replace("FG", fg)
        .replace("BG", bg))
}

/// `(category, weight)` of every resolved leaf.
fn weights(c: &ResolvedClosure) -> Vec<(&'static str, Vec3A)> {
    c.leaves().iter().map(|l| (l.category, l.weight)).collect()
}

#[test]
fn a_pruned_mix_branch_keeps_its_share_of_the_throughput() {
    // `mix(T_bg, T_fg, m)` with the dummy's throughput at 1 and the dust's at
    // 0: the base keeps `1 − 0.25` when the dust is `fg`, `0.25` when it is
    // `bg`. Rewritten as `multiply(dust, m)`, the top took the diffuse's
    // throughput alone, 0, and the DPEL assets rendered black but for their
    // dust.
    for (dust_is_fg, dust, body) in [(true, 0.25, 0.75), (false, 0.75, 0.25)] {
        let c = resolved(&coverage(true, dust_is_fg), "x", 0.3, true);
        assert_eq!(
            weights(&c),
            vec![
                ("oren_nayar_diffuse_bsdf", Vec3A::splat(dust)),
                ("oren_nayar_diffuse_bsdf", Vec3A::splat(body)),
            ],
            "dust as {}",
            if dust_is_fg { "fg" } else { "bg" }
        );
    }
}

#[test]
fn pruning_a_zero_weight_dielectric_does_not_change_the_closure() {
    // Pruning is a compile-time shortcut, so a literal-zero weight must
    // resolve exactly as the same weight arriving through a connection, which
    // the compiler cannot prune and the walk evaluates as a live leaf.
    // (Exact for the leaves MaterialX leaves at throughput 1 when their
    // weight is 0: dielectric, generalized Schlick and sheen.)
    for dust_is_fg in [true, false] {
        for theta in [0.0f32, 0.8, 1.4] {
            let pruned = resolved(&coverage(true, dust_is_fg), "x", theta, true);
            let live = resolved(&coverage(false, dust_is_fg), "x", theta, true);
            assert_eq!(weights(&pruned), weights(&live), "theta {theta}");
        }
    }
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

/// A `T`-mode dielectric pays its own `(1 − F)` — MaterialX GLSL, OSL, BSDL
/// and Typhoon all do — so from inside a glass, past the critical angle, it
/// transmits nothing. Without the factor, a glass's exit leaf transmitted
/// `1 − E_R(front)` beside a reflection leaf already reflecting the total
/// internal reflection, and a glass sphere returned 1.16× a white furnace.
#[test]
fn a_transmission_only_dielectric_pays_its_fresnel_loss() {
    let body = doc(r#"<dielectric_bsdf name="g" type="BSDF">
             <input name="roughness" type="vector2" value="0.01, 0.01" />
             <input name="scatter_mode" type="string" value="T" />
           </dielectric_bsdf>
           <surface name="s" type="surfaceshader"><input name="bsdf" type="BSDF" nodename="g" /></surface>"#);
    let head_on = albedo(&resolved(&body, "s", 0.0, true), 0.0, true, 512);
    assert!((head_on.x - 0.96).abs() < 0.01, "front, head on: {head_on}");
    // Leaving the interior at 60°, beyond asin(1/1.5) ≈ 41.8°.
    let theta = 60f32.to_radians();
    let tir = albedo(&resolved(&body, "s", theta, false), theta, false, 512);
    assert!(tir.max_element() < 1e-3, "total internal reflection: {tir}");
}

/// Task 2.4's measurement, kept as a bound: BSDL's baked `E_R` describes
/// BSDL's own GGX (with its Turquin multiple-scattering term), not crust's
/// leaf (MaterialX's analytic fit for the compensation), so the two are not
/// the same integral. This measures the gap over the table's range — the
/// layer throughput a top leaf hands its base is `1 − E_table` while the top
/// itself reflects `E_leaf`, so `E_leaf − E_table` is exactly the energy a
/// layer creates (positive) or loses (negative). `--nocapture` prints the grid.
#[test]
fn the_bsdl_table_tracks_the_leaf_it_stands_for() {
    let mut worst = (0.0f32, 0.0, 0.0, 0.0);
    for ior in [1.2f32, 1.5, 2.0, 3.0] {
        for r in [0.05f32, 0.2, 0.4, 0.6, 0.8, 1.0] {
            let a = r * r;
            let body = doc(&format!(
                r#"<dielectric_bsdf name="g" type="BSDF">
                     <input name="ior" type="float" value="{ior}" />
                     <input name="roughness" type="vector2" value="{a}, {a}" />
                   </dielectric_bsdf>
                   <surface name="s" type="surfaceshader"><input name="bsdf" type="BSDF" nodename="g" /></surface>"#
            ));
            let mut row = String::new();
            for cos in [0.1f32, 0.3, 0.6, 1.0] {
                let theta = cos.acos();
                let e_leaf = albedo(&resolved(&body, "s", theta, true), theta, true, 4096).x;
                let e_table = 1.0 - dielectric_refl_filter(cos, r, ior);
                let gap = e_leaf - e_table;
                row += &format!("  cos {cos:.1}: {e_leaf:.4} vs {e_table:.4} ({gap:+.4})");
                if gap.abs() > worst.0.abs() {
                    worst = (gap, ior, r, cos);
                }
            }
            println!("ior {ior:.1} r {r:.2}{row}");
        }
    }
    println!(
        "worst gap {:+.4} at ior {} r {} cos {}",
        worst.0, worst.1, worst.2, worst.3
    );
    assert!(worst.0.abs() < 0.025, "table vs leaf: {worst:?}");
}

/// The same measurement for the throughputs that come from MaterialX's
/// analytic fits rather than a table — generalized Schlick
/// (`mx_ggx_dir_albedo` × compensation) and sheen
/// (`mx_imageworks_sheen_dir_albedo`). The `E` a layer uses is read back as
/// `1 − weight` of a white diffuse base under the leaf.
#[test]
fn the_materialx_fits_track_the_leaves_they_stand_for() {
    let leaves = [
        (
            "schlick",
            r#"<generalized_schlick_bsdf name="t" type="BSDF">
                 <input name="color0" type="color3" value="0.04, 0.04, 0.04" />
                 <input name="roughness" type="vector2" value="{a}, {a}" />
               </generalized_schlick_bsdf>"#,
            0.015f32,
        ),
        (
            "sheen",
            r#"<sheen_bsdf name="t" type="BSDF">
                 <input name="roughness" type="float" value="{r}" />
               </sheen_bsdf>"#,
            // The fit overestimates `E` by up to 0.035 at low roughness and
            // grazing: a sheen layer there loses energy, never creates it.
            0.04,
        ),
    ];
    for (kind, leaf, bound) in leaves {
        let mut worst = (0.0f32, 0.0, 0.0);
        for r in [0.1f32, 0.3, 0.5, 0.8] {
            let top = leaf
                .replace("{a}", &(r * r).to_string())
                .replace("{r}", &r.to_string());
            let alone = doc(&format!(
                r#"{top}<surface name="s" type="surfaceshader"><input name="bsdf" type="BSDF" nodename="t" /></surface>"#
            ));
            let layered = doc(&format!(
                r#"{top}<oren_nayar_diffuse_bsdf name="d" type="BSDF"><input name="color" type="color3" value="1, 1, 1" /></oren_nayar_diffuse_bsdf>
                   <layer name="l" type="BSDF"><input name="top" type="BSDF" nodename="t" /><input name="base" type="BSDF" nodename="d" /></layer>
                   <surface name="s" type="surfaceshader"><input name="bsdf" type="BSDF" nodename="l" /></surface>"#
            ));
            let mut row = String::new();
            for cos in [0.1f32, 0.3, 0.6, 1.0] {
                let theta = cos.acos();
                let e_leaf = albedo(&resolved(&alone, "s", theta, true), theta, true, 4096).x;
                let c = resolved(&layered, "s", theta, true);
                let base = c
                    .leaves()
                    .iter()
                    .find(|l| l.category == "oren_nayar_diffuse_bsdf")
                    .expect("the base leaf");
                let e_fit = 1.0 - base.weight.x;
                let gap = e_leaf - e_fit;
                row += &format!("  cos {cos:.1}: {e_leaf:.4} vs {e_fit:.4} ({gap:+.4})");
                if gap.abs() > worst.0.abs() {
                    worst = (gap, r, cos);
                }
            }
            println!("{kind} r {r:.1}{row}");
        }
        println!(
            "{kind}: worst gap {:+.4} at r {} cos {}",
            worst.0, worst.1, worst.2
        );
        assert!(worst.0.abs() < bound, "{kind}: {worst:?}");
    }
}

/// The closure's ~1.9 KB of leaves lives behind a pooled box, so the
/// `ShadingPoint` every material shares stays small; inline, it was 1984 bytes
/// and a vertex paid seven whole-struct moves (see `PooledClosure`).
#[test]
fn a_shading_point_carries_the_closure_by_pointer() {
    assert!(size_of::<ResolvedClosure>() > 1024);
    assert!(
        size_of::<crate::ShadingPoint>() <= 512,
        "ShadingPoint is {} bytes",
        size_of::<crate::ShadingPoint>()
    );
}

/// A closure dropped goes back to its thread's pool and the next resolve
/// reuses it, overwriting every field a stale one could leak.
#[test]
fn a_recycled_closure_answers_like_a_fresh_one() {
    let two = doc(r#"<dielectric_bsdf name="g" type="BSDF" />
           <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
           <layer name="l" type="BSDF"><input name="top" type="BSDF" nodename="g" /><input name="base" type="BSDF" nodename="d" /></layer>
           <surface name="s" type="surfaceshader"><input name="bsdf" type="BSDF" nodename="l" /></surface>"#);
    let one = doc(r#"<oren_nayar_diffuse_bsdf name="d" type="BSDF" />
           <surface name="s" type="surfaceshader"><input name="bsdf" type="BSDF" nodename="d" /></surface>"#);
    let (r, rec) = (arriving(0.3), hit(true));
    let (c2, s2) = tree(&two, "s");
    let (c1, s1) = tree(&one, "s");
    drop(PooledClosure::resolve(
        &c2,
        &s2,
        &r,
        &rec,
        utils::Luma::REC709,
    ));
    let reused = PooledClosure::resolve(&c1, &s1, &r, &rec, utils::Luma::REC709);
    let fresh = ResolvedClosure::resolve(&c1, &s1, &r, &rec, utils::Luma::REC709);
    assert_eq!(reused.leaves().len(), 1);
    let wi = Vec3A::new(0.2, 0.1, 0.9).normalize();
    assert_eq!(reused.eval(&r, &rec, wi), fresh.eval(&r, &rec, wi));
    assert!(!reused.transmits() && reused.medium().is_none());
}

/// A coat over a specular over a diffuse, the shape of `standard_surface`.
const COATED: &str = r#"
  <dielectric_bsdf name="coat" type="BSDF">
    <input name="roughness" type="vector2" value="0.0001, 0.0001" />
  </dielectric_bsdf>
  <dielectric_bsdf name="spec" type="BSDF">
    <input name="roughness" type="vector2" value="0.3, 0.3" />
  </dielectric_bsdf>
  <dielectric_bsdf name="refr" type="BSDF">
    <input name="roughness" type="vector2" value="0.1, 0.1" />
    <input name="scatter_mode" type="string" value="T" />
  </dielectric_bsdf>
  <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
  <mix name="body" type="BSDF">
    <input name="fg" type="BSDF" nodename="refr" />
    <input name="bg" type="BSDF" nodename="d" />
    <input name="mix" type="float" value="0.5" />
  </mix>
  <layer name="base" type="BSDF">
    <input name="top" type="BSDF" nodename="spec" />
    <input name="base" type="BSDF" nodename="body" />
  </layer>
  <layer name="x" type="BSDF">
    <input name="top" type="BSDF" nodename="coat" />
    <input name="base" type="BSDF" nodename="base" />
  </layer>"#;

#[test]
fn the_lobe_split_sums_to_eval_bitwise() {
    use crate::lpe::LobeSplit;
    let mut split = LobeSplit::default();
    let mut checked = 0;
    for (name, leaf) in LEAVES {
        for (body, root) in [(doc(leaf), "x"), (doc(COATED), "x")] {
            let c = resolved(&body, root, 0.6, true);
            let r = arriving(0.6);
            let rec = hit(true);
            for k in 0..32 {
                let phi = k as f32 * 0.7;
                let z = 1.0 - 2.0 * ((k as f32 + 0.5) / 32.0);
                let s = (1.0 - z * z).sqrt();
                let wi = Vec3A::new(s * phi.cos(), s * phi.sin(), z);
                let (value, _) = c.eval(&r, &rec, wi).expect("a leaf");
                assert!(c.eval_lobes(wi, &mut split));
                assert_eq!(split.len(), c.leaves().len());
                let total = split.total();
                assert_eq!(
                    [total.x.to_bits(), total.y.to_bits(), total.z.to_bits()],
                    [value.x.to_bits(), value.y.to_bits(), value.z.to_bits()],
                    "{name}: {total} vs {value}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 0);
}

#[test]
fn a_reflecting_interface_over_another_is_the_coat() {
    use crate::lpe::{LobeLabel, Scatter};
    let c = resolved(&doc(COATED), "x", 0.3, true);
    let labels: Vec<_> = c
        .leaves()
        .iter()
        .map(|l| {
            (
                l.event(false).label,
                l.event(false).scatter,
                l.event(true).label,
            )
        })
        .collect();
    assert_eq!(
        labels,
        [
            // The coat: a mirror, so singular.
            (LobeLabel::Coat, Scatter::Singular, LobeLabel::Transmission),
            // The base specular over a transmission-only interface stays
            // specular.
            (
                LobeLabel::Specular,
                Scatter::Glossy,
                LobeLabel::Transmission
            ),
            (
                LobeLabel::Specular,
                Scatter::Glossy,
                LobeLabel::Transmission
            ),
            (LobeLabel::Diffuse, Scatter::Diffuse, LobeLabel::Diffuse),
        ]
    );
    // Alone, a dielectric is the specular.
    let alone = resolved(&doc(LEAVES[3].1), "x", 0.3, true);
    assert_eq!(alone.leaves()[0].event(false).label, LobeLabel::Specular);
    // The albedo is within [0, 1] and tracks the tints.
    let a = c.albedo();
    assert!(
        a.min_element() >= 0.0 && a.max_element() <= 1.0 && a.x > 0.1,
        "{a}"
    );
}

#[test]
fn the_diffuse_filter_sums_diffuse_leaves_only() {
    // Two half-weight diffuse leaves of one colour: the colour, once.
    let halves = doc(r#"
      <oren_nayar_diffuse_bsdf name="a" type="BSDF">
        <input name="color" type="color3" value="0.6, 0.3, 0.2" />
      </oren_nayar_diffuse_bsdf>
      <oren_nayar_diffuse_bsdf name="b" type="BSDF">
        <input name="color" type="color3" value="0.6, 0.3, 0.2" />
      </oren_nayar_diffuse_bsdf>
      <mix name="x" type="BSDF">
        <input name="fg" type="BSDF" nodename="a" />
        <input name="bg" type="BSDF" nodename="b" />
        <input name="mix" type="float" value="0.5" />
      </mix>"#);
    let f = resolved(&halves, "x", 0.3, true).diffuse_filter();
    assert!(
        (f - Vec3A::new(0.6, 0.3, 0.2)).abs().max_element() < 1e-6,
        "{f}"
    );

    // Translucent and subsurface leaves transmit: no diffuse reflection.
    for (name, leaf) in LEAVES {
        let c = resolved(&doc(leaf), "x", 0.3, true);
        let f = c.diffuse_filter();
        let diffuse = c
            .leaves()
            .iter()
            .any(|l| matches!(l.lobe, Lobe::Diffuse { .. }));
        assert_eq!(f != Vec3A::ZERO, diffuse, "{name}: {f}");
    }
}

/// A fibre beside a diffuse: the mixture's density, the fibre's over the
/// whole sphere and the diffuse's over the hemisphere, integrates to one.
#[test]
fn a_hair_mixture_pdf_integrates_to_one() {
    let body = doc(r#"
      <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
      <chiang_hair_bsdf name="h" type="BSDF">
        <input name="roughness_R" type="vector2" value="0.3, 0.4" />
        <input name="roughness_TT" type="vector2" value="0.1, 0.4" />
        <input name="roughness_TRT" type="vector2" value="0.9, 0.4" />
      </chiang_hair_bsdf>
      <mix name="x" type="BSDF">
        <input name="fg" type="BSDF" nodename="h" />
        <input name="bg" type="BSDF" nodename="d" />
        <input name="mix" type="float" value="0.25" />
      </mix>"#);
    let c = resolved(&body, "x", 0.5, true);
    assert_eq!(c.leaves().len(), 2);
    let n = 400_000;
    let mut sum = 0.0f64;
    for i in 0..n {
        let u = (i as f32 + 0.5) / n as f32;
        let z = 1.0 - 2.0 * u;
        let phi = 2.0 * PI * ((i as f32 * 0.618_034) % 1.0);
        let s = (1.0 - z * z).max(0.0).sqrt();
        let wi = Vec3A::new(s * phi.cos(), s * phi.sin(), z);
        // Below the floor `eval` applies, so the lower hemisphere counts
        // only the fibre's own density.
        let (_, p) = c.eval_pdf(wi);
        sum += p as f64;
    }
    let integral = sum / n as f64 * 4.0 * std::f64::consts::PI;
    assert!((integral - 1.0).abs() < 0.03, "∫pdf = {integral}");
}

/// A fibre's continuation ray passes out of curve tubes; any other closure's
/// does not.
#[test]
fn a_hair_vertex_passes_out_of_curves() {
    let hair = LEAVES.iter().find(|(n, _)| *n == "hair").unwrap().1;
    let c = resolved(&doc(hair), "x", 0.4, true);
    assert!(c.passes_out_of_curves());
    let (r, rec) = (arriving(0.4), hit(true));
    let mut s = S(0);
    let mut seen = 0;
    for _ in 0..64 {
        if let Some(x) = c.scatter(&r, &rec, s.next()) {
            assert!(x.ray.rt().ignore_curve_exits);
            seen += 1;
        }
    }
    assert!(seen > 32);
    for (name, body) in LEAVES.iter().filter(|(n, _)| *n != "hair") {
        let c = resolved(&doc(body), "x", 0.4, true);
        assert!(!c.passes_out_of_curves(), "{name}");
        if let Some(x) = c.scatter(&r, &rec, S(0).next()) {
            assert!(!x.ray.rt().ignore_curve_exits, "{name}");
        }
    }
}

/// A fibre's sample on the far side of its normal is a transmission, on the
/// near side a reflection, both glossy.
#[test]
fn a_hair_leaf_is_classified_by_hemisphere() {
    use crate::lpe::{LobeLabel, Scatter};
    let hair = LEAVES.iter().find(|(n, _)| *n == "hair").unwrap().1;
    let c = resolved(&doc(hair), "x", 0.4, true);
    let leaf = &c.leaves()[0];
    let (near, far) = (leaf.event(false), leaf.event(true));
    assert_eq!(
        (near.label, near.scatter),
        (LobeLabel::Specular, Scatter::Glossy)
    );
    assert_eq!(
        (far.label, far.scatter),
        (LobeLabel::Transmission, Scatter::Glossy)
    );
    assert!(!near.transmit && far.transmit);
}

/// A fibre beside a transmitting leaf: a continuation ray into the tube
/// carries one of the two shares — the fibre's passing out of the strand,
/// the other's meeting its far wall — picked in proportion to them and
/// scaled by the inverse probability. Each sample's value is the chosen
/// share over its probability, the split sums to it bit for bit, both
/// choices happen, and a ray out of the tube or a non-mixed fibre is left as
/// it was.
#[test]
fn a_mixed_fibre_vertex_splits_its_continuation_by_share() {
    use crate::lpe::LobeSplit;
    let body = doc(r#"
      <chiang_hair_bsdf name="h" type="BSDF" />
      <translucent_bsdf name="t" type="BSDF" />
      <mix name="x" type="BSDF">
        <input name="fg" type="BSDF" nodename="h" />
        <input name="bg" type="BSDF" nodename="t" />
        <input name="mix" type="float" value="0.5" />
      </mix>"#);
    let c = resolved(&body, "x", 0.4, true);
    assert!(c.passes_out_of_curves() && c.mixes_hair());
    assert_eq!(c.hair_leaves(), 0b01);
    let (r, rec) = (arriving(0.4), hit(true));
    let mut s = S(0);
    let mut split = LobeSplit::default();
    let (mut passed, mut walled) = (0, 0);
    for _ in 0..512 {
        let Some(x) = c.scatter_split(&r, &rec, s.next(), &mut split) else {
            continue;
        };
        let wi = x.ray.direction().normalize();
        let total = split.total();
        assert_eq!(
            [total.x.to_bits(), total.y.to_bits(), total.z.to_bits()],
            [
                x.value.x.to_bits(),
                x.value.y.to_bits(),
                x.value.z.to_bits()
            ],
        );
        if rec.normal.dot(wi) >= 0.0 {
            // Out of the tube the flag changes nothing; the value is eval's.
            let (v, _) = c.eval_pdf(wi);
            assert_eq!(x.value, v);
            continue;
        }
        let (hair, other) = c.eval_hair_split(wi);
        let (h, o) = (hair.element_sum(), other.element_sum());
        if x.ray.rt().ignore_curve_exits {
            passed += 1;
            let want = hair * ((h + o) / h);
            assert!((x.value - want).abs().max_element() <= 1e-5 * want.max_element());
        } else {
            walled += 1;
            let want = other * ((h + o) / o);
            assert!((x.value - want).abs().max_element() <= 1e-5 * want.max_element());
        }
    }
    assert!(
        passed > 20 && walled > 20,
        "passed {passed}, walled {walled}"
    );

    // A fibre on its own: every ray passes, its value is eval's.
    let hair = LEAVES.iter().find(|(n, _)| *n == "hair").unwrap().1;
    let c = resolved(&doc(hair), "x", 0.4, true);
    assert!(!c.mixes_hair());
    for _ in 0..64 {
        if let Some(x) = c.scatter(&r, &rec, s.next()) {
            assert!(x.ray.rt().ignore_curve_exits);
            assert_eq!(x.value, c.eval_pdf(x.ray.direction().normalize()).0);
        }
    }
}

/// Which neighbours make a fibre vertex mixed: a leaf that can send light
/// into the tube (a refracting dielectric, a translucent), not one that only
/// reflects.
#[test]
fn only_a_transmitting_neighbour_mixes_a_fibre_vertex() {
    let with = |neighbour: &str| {
        let body = doc(&format!(
            r#"
      <chiang_hair_bsdf name="h" type="BSDF" />
      {neighbour}
      <mix name="x" type="BSDF">
        <input name="fg" type="BSDF" nodename="h" />
        <input name="bg" type="BSDF" nodename="n" />
        <input name="mix" type="float" value="0.5" />
      </mix>"#
        ));
        resolved(&body, "x", 0.4, true).mixes_hair()
    };
    assert!(with(
        r#"<dielectric_bsdf name="n" type="BSDF">
             <input name="scatter_mode" type="string" value="RT" />
           </dielectric_bsdf>"#
    ));
    assert!(with(r#"<translucent_bsdf name="n" type="BSDF" />"#));
    assert!(!with(r#"<dielectric_bsdf name="n" type="BSDF" />"#));
    assert!(!with(r#"<oren_nayar_diffuse_bsdf name="n" type="BSDF" />"#));
}
