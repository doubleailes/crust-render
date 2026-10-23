//! MaterialX BSDF leaves against BSDL, the lobes MaterialX's OSL backend
//! renders with in OSL's `testrender`.
//!
//! `data/bsdl_oracle.txt` holds cases (one leaf with its inputs, a view
//! direction, light directions) with the values BSDL's `mtx::` lobes give for
//! them: the directional albedo over a fixed grid on both sides of the
//! surface, the layering filter `filter_o` (what the leaf lets through to a
//! layer's base), `albedo_impl` (the albedo testrender picks lobes by), and
//! the BSDF value times the cosine and the sampling density toward each
//! light. The generator is
//! `scripts/bsdl_oracle.py`, which builds `scripts/bsdl_oracle/probe.cpp`
//! against OSL's `src/libbsdl` at a pinned commit. Here each case is compiled
//! as a MaterialX document and collapsed by crust's closure evaluator.
//!
//! The fixture is committed, so this needs neither a C++ compiler nor OSL's
//! sources. The pdf is compared too: the microfacet leaves sample as BSDL
//! does (bounded VNDF), so their density is BSDL's toward every direction,
//! the ones its sampler never reaches included.
//!
//! crust does not match BSDL everywhere. Every known difference is a
//! [`Deviation`]: a named condition on a case's inputs, the gap it stands
//! for, and the largest error it excuses. A case no deviation applies to must
//! match within [`TOLERANCE`]. A deviation that no longer excuses anything is
//! stale and fails the test, so closing a gap means deleting its rule here
//! and its entry in `openspec/specs/materials/design.md`.

use std::collections::BTreeMap;
use std::f32::consts::PI;

use crust_core::closure::ResolvedClosure;
use crust_core::mtlx::{Closures, Compiler, Doc, Host, ShadeCtx, Val, flatten};
use crust_core::{HitRecord, Ray};
use glam::Vec3A;

const FIXTURE: &str = include_str!("data/bsdl_oracle.txt");

/// The side of the albedo grid; `probe.cpp`'s `GRID`.
const GRID: usize = 32;

/// The largest error a case may have when no deviation applies (see
/// [`Errors::worst`] for what is measured).
const TOLERANCE: f32 = 2e-3;

struct Case<'a> {
    id: &'a str,
    inputs: Vec<(&'a str, &'a str)>,
    view: Vec3A,
    lights: Vec<Vec3A>,
    albedo: Vec3A,
    filter: Vec3A,
    albedo_impl: Vec3A,
    values: Vec<Vec3A>,
    pdfs: Vec<f32>,
}

impl Case<'_> {
    /// The input `name`, as the fixture spells it.
    fn input(&self, name: &str) -> Option<&str> {
        self.inputs
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
    }

    fn leaf(&self) -> &str {
        self.input("leaf").expect("every case names its leaf")
    }

    fn is(&self, leaf: &str) -> bool {
        self.leaf() == leaf
    }

    fn back(&self) -> bool {
        self.input("back") == Some("1")
    }

    fn has_film(&self) -> bool {
        self.input("thinfilm_thickness").is_some()
    }
}

fn floats(s: &str) -> Vec<f32> {
    s.split_whitespace()
        .map(|x| x.parse().unwrap_or_else(|_| panic!("bad number {x:?}")))
        .collect()
}

fn vec3s(s: &str) -> Vec<Vec3A> {
    floats(s)
        .chunks(3)
        .map(|c| Vec3A::new(c[0], c[1], c[2]))
        .collect()
}

fn cases() -> Vec<Case<'static>> {
    FIXTURE
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let (question, answer) = l.split_once(" || ").expect("question || answer");
            let q: Vec<&str> = question.split(" | ").collect();
            let a: Vec<&str> = answer.split(" | ").collect();
            assert_eq!((q.len(), a.len()), (3, 5), "{l:?}");
            let mut head = q[0].split_whitespace();
            let id = head.next().expect("case id");
            let inputs = head
                .map(|t| t.split_once('=').expect("name=value"))
                .collect();
            Case {
                id,
                inputs,
                view: vec3s(q[1])[0].normalize(),
                lights: vec3s(q[2]).into_iter().map(|l| l.normalize()).collect(),
                albedo: vec3s(a[0])[0],
                filter: vec3s(a[1])[0],
                albedo_impl: vec3s(a[2])[0],
                values: vec3s(a[3]),
                pdfs: floats(a[4]),
            }
        })
        .collect()
}

/// The case's leaf as a MaterialX node named `x`, every input authored.
fn leaf_node(case: &Case) -> String {
    let mut inputs = String::new();
    let mut add = |name: &str, ty: &str, value: &str| {
        inputs += &format!(r#"<input name="{name}" type="{ty}" value="{value}" />"#);
    };
    let v = |name: &str| case.input(name).expect(name).replace(',', ", ");
    let category = match case.leaf() {
        "dielectric" => {
            add("tint", "color3", &v("tint"));
            add("ior", "float", &v("ior"));
            add("roughness", "vector2", &v("roughness"));
            add("scatter_mode", "string", &v("mode"));
            "dielectric_bsdf"
        }
        "conductor" => {
            add("ior", "color3", &v("ior"));
            add("extinction", "color3", &v("extinction"));
            add("roughness", "vector2", &v("roughness"));
            "conductor_bsdf"
        }
        "schlick" => {
            add("color0", "color3", &v("color0"));
            add("color82", "color3", "1, 1, 1");
            add("color90", "color3", &v("color90"));
            add("exponent", "float", &v("exponent"));
            add("roughness", "vector2", &v("roughness"));
            add("scatter_mode", "string", &v("mode"));
            "generalized_schlick_bsdf"
        }
        "sheen" => {
            add("color", "color3", &v("color"));
            add("roughness", "float", &v("roughness"));
            add("mode", "string", &v("sheen_mode"));
            "sheen_bsdf"
        }
        "oren_nayar" => {
            add("color", "color3", &v("color"));
            add("roughness", "float", &v("roughness"));
            let ec = if case.input("energy_compensation") == Some("1") {
                "true"
            } else {
                "false"
            };
            add("energy_compensation", "boolean", ec);
            "oren_nayar_diffuse_bsdf"
        }
        "burley" => {
            add("color", "color3", &v("color"));
            add("roughness", "float", &v("roughness"));
            "burley_diffuse_bsdf"
        }
        "translucent" => {
            add("color", "color3", &v("color"));
            "translucent_bsdf"
        }
        other => panic!("{}: no leaf {other:?}", case.id),
    };
    if case.has_film() {
        add("thinfilm_thickness", "float", &v("thinfilm_thickness"));
        add("thinfilm_ior", "float", &v("thinfilm_ior"));
    }
    format!(r#"<{category} name="x" type="BSDF">{inputs}</{category}>"#)
}

/// Compiles `root` of an inline document and evaluates its program.
fn tree(doc: &str, root: &str) -> (Closures, Vec<Val>) {
    let d = Doc::parse(doc).expect("document parses");
    let loader = |_: &str, _: crust_core::mtlx::TexelSpace<'_>| None;
    let mut c = Compiler::new(&d, &Host::new(&loader));
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
            curve: None,
            primvars: &[],
        },
        &mut slots,
    );
    (out, slots)
}

/// The ray toward the shading point along the case's view direction, and
/// the hit it makes: the ray-facing normal +Z and the tangent +X, on the
/// back face when the case says so.
fn hit(case: &Case) -> (Ray, HitRecord<'static>) {
    let mut rec = HitRecord::new();
    rec.p = Vec3A::ZERO;
    rec.normal = Vec3A::Z;
    rec.tangent = Vec3A::X;
    rec.front_face = !case.back();
    (Ray::new(case.view, -case.view), rec)
}

fn resolve(case: &Case, doc: &str, root: &str) -> ResolvedClosure {
    let (cl, slots) = tree(doc, root);
    let (ray, rec) = hit(case);
    ResolvedClosure::resolve(&cl, &slots, &ray, &rec, utils::Luma::REC709)
}

/// `probe.cpp`'s `grid_direction`: Malley's method on the midpoints of a
/// `GRID` × `GRID` square, in the hemisphere of sign `side`.
fn grid_direction(i: usize, j: usize, side: f32) -> Vec3A {
    let u = (i as f32 + 0.5) / GRID as f32;
    let v = (j as f32 + 0.5) / GRID as f32;
    let r = u.sqrt();
    let phi = 2.0 * PI * v;
    Vec3A::new(
        r * phi.cos(),
        r * phi.sin(),
        side * (1.0 - u).max(0.0).sqrt(),
    )
}

/// crust's answers to the case's questions.
struct Shaded {
    albedo: Vec3A,
    filter: Vec3A,
    albedo_impl: Vec3A,
    values: Vec<Vec3A>,
    pdfs: Vec<f32>,
}

fn shade(case: &Case) -> Shaded {
    let node = leaf_node(case);
    let alone = resolve(case, &format!("<materialx>{node}</materialx>"), "x");
    let (ray, rec) = hit(case);
    let eval = |l: Vec3A| alone.eval(&ray, &rec, l).map_or(Vec3A::ZERO, |(f, _)| f);
    // `eval` floors the density at 1e-4, which the comparison's 0.05 floor
    // absorbs.
    let pdf = |l: Vec3A| alone.eval(&ray, &rec, l).map_or(0.0, |(_, p)| p);
    let mut sum = glam::DVec3::ZERO;
    for side in [1.0, -1.0] {
        for i in 0..GRID {
            for j in 0..GRID {
                let l = grid_direction(i, j, side);
                if l.z.abs() > 0.0 {
                    sum += (eval(l) / l.z.abs()).as_dvec3();
                }
            }
        }
    }
    // The filter, read where it acts: the weight a layer gives a base under
    // the leaf, `1 − clamp(opacity)`, which is the filter for a leaf that
    // lets light through and 0 for an opaque one. The base is pushed after
    // the top's leaves, and dropped when its weight is 0.
    let layered = resolve(
        case,
        &format!(
            r#"<materialx>{node}
                 <oren_nayar_diffuse_bsdf name="base" type="BSDF" />
                 <layer name="l" type="BSDF">
                   <input name="top" type="BSDF" nodename="x" />
                   <input name="base" type="BSDF" nodename="base" />
                 </layer></materialx>"#
        ),
        "l",
    );
    let top = alone.leaves().len();
    Shaded {
        albedo: (sum * (std::f64::consts::PI / (GRID * GRID) as f64)).as_vec3a(),
        filter: layered
            .leaves()
            .get(top)
            .map_or(Vec3A::ZERO, |base| base.weight),
        albedo_impl: alone.leaves().first().map_or(Vec3A::ZERO, |l| l.albedo),
        values: case.lights.iter().map(|&l| eval(l)).collect(),
        pdfs: case.lights.iter().map(|&l| pdf(l)).collect(),
    }
}

/// How far crust is from the reference on one case, in five measures that
/// stay meaningful where values are near zero and where they peak: the
/// albedo's, the filter's and `albedo_impl`'s absolute errors, and each
/// value's and each pdf's error relative to `max(0.05, |reference|)`.
#[derive(Clone, Copy, Debug, Default)]
struct Errors {
    albedo: f32,
    filter: f32,
    albedo_impl: f32,
    value: f32,
    pdf: f32,
}

impl Errors {
    /// A non-finite value on either side is an infinite error: `f32::max`
    /// and `max_element` would otherwise drop a NaN and let it pass.
    fn of(case: &Case, got: &Shaded) -> Errors {
        let finite = |g: Vec3A, w: Vec3A| g.is_finite() && w.is_finite();
        let abs = |g: Vec3A, w: Vec3A| {
            if finite(g, w) {
                (g - w).abs().max_element()
            } else {
                f32::INFINITY
            }
        };
        let rel = |g: Vec3A, w: Vec3A| {
            if finite(g, w) {
                ((g - w).abs() / w.abs().max(Vec3A::splat(0.05))).max_element()
            } else {
                f32::INFINITY
            }
        };
        Errors {
            albedo: abs(got.albedo, case.albedo),
            filter: abs(got.filter, case.filter),
            albedo_impl: abs(got.albedo_impl, case.albedo_impl),
            value: got
                .values
                .iter()
                .zip(&case.values)
                .map(|(&g, &w)| rel(g, w))
                .fold(0.0, f32::max),
            pdf: got
                .pdfs
                .iter()
                .zip(&case.pdfs)
                .map(|(&g, &w)| rel(Vec3A::splat(g), Vec3A::splat(w)))
                .fold(0.0, f32::max),
        }
    }

    fn worst(self) -> f32 {
        self.albedo
            .max(self.filter)
            .max(self.albedo_impl)
            .max(self.value)
            .max(self.pdf)
    }
}

/// A known difference between crust and the reference.
///
/// `applies` is the condition on a case's inputs under which it can occur,
/// and `gap` what it is. `bound` is the largest error it excuses, measured
/// with [`report_deviations`]: the worst error among the cases it alone
/// applies to, or, for a gap no case isolates, what the cases it applies to
/// need beyond the other deviations' bounds; plus 2%, rounded up.
struct Deviation {
    name: &'static str,
    gap: &'static str,
    applies: fn(&Case) -> bool,
    bound: f32,
}

/// The gaps `openspec/specs/materials/design.md` lists, each as the input
/// condition under which crust's answer differs from BSDL's. A case may be
/// under several; its error is excused up to the sum of their bounds.
const DEVIATIONS: &[Deviation] = &[
    Deviation {
        name: "thin-film",
        gap: "a thin film colours crust's dielectric and conductor (MaterialX's GLSL Airy film); BSDL ignores it",
        applies: |c| c.has_film(),
        bound: 0.611,
    },
    Deviation {
        name: "conductor-selection",
        gap: "a conductor is selected by its compensated albedo toward the view, where BSDL's albedo_impl is a rough fit to the average Fresnel",
        applies: |c| c.is("conductor"),
        bound: 0.357,
    },
    Deviation {
        name: "back-face-reflection-filter",
        gap: "under a back face a reflection-only dielectric or Schlick lets through what its own lobe leaves (crust's DielectricReflBack table, nothing past the critical angle), where BSDL reads DielectricReflFront and lets 1 − F(front) through a lobe that totally reflects",
        applies: |c| c.back() && c.input("mode") == Some("R"),
        bound: 0.928,
    },
];

/// Prints every case's applicable deviations and errors, and for each
/// deviation the worst error among the cases it alone applies to: the
/// numbers its `bound` is set from. Run it after regenerating the fixture
/// or closing a gap:
///
/// ```text
/// cargo test -p crust-core --test bsdl_oracle -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a measurement, not a check"]
fn report_deviations() {
    let mut alone: BTreeMap<&str, f32> = BTreeMap::new();
    let mut unruled = 0.0f32;
    for case in &cases() {
        let errors = Errors::of(case, &shade(case));
        let rules: Vec<&str> = DEVIATIONS
            .iter()
            .filter(|d| (d.applies)(case))
            .map(|d| d.name)
            .collect();
        match rules[..] {
            [] => unruled = unruled.max(errors.worst()),
            [only] => {
                let e = alone.entry(only).or_default();
                *e = e.max(errors.worst());
            }
            _ => {}
        }
        println!(
            "{:<32} worst {:>8.4}  albedo {:.4} filter {:.4} impl {:.4} value {:.4} pdf {:.4}  {}",
            case.id,
            errors.worst(),
            errors.albedo,
            errors.filter,
            errors.albedo_impl,
            errors.value,
            errors.pdf,
            rules.join(", ")
        );
    }
    println!("\nworst error where no deviation applies: {unruled:.5}");
    println!("worst error where a deviation applies alone (its bound):");
    for d in DEVIATIONS {
        println!(
            "  {:<26} {:>16} ({})  {}",
            d.name,
            alone
                .get(d.name)
                .map_or("no isolated case".into(), |e| format!("{e:.4}")),
            d.bound,
            d.gap
        );
    }
}

/// The number before `word` on the fixture header line that holds it.
fn declared(word: &str) -> usize {
    let line = FIXTURE
        .lines()
        .find(|l| l.starts_with('#') && l.contains(word))
        .unwrap_or_else(|| panic!("the fixture header states its {word}"));
    let head = &line[..line.find(word).expect("found above")];
    head.split_whitespace()
        .last()
        .and_then(|n| n.trim_start_matches('#').parse().ok())
        .unwrap_or_else(|| panic!("no number before {word:?} in {line:?}"))
}

/// Every case the generator wrote is in the fixture, with every light it
/// drew and an answer for each, so a fixture that lost rows or directions
/// cannot pass by checking less.
#[test]
fn the_fixture_is_complete() {
    let cases = cases();
    assert_eq!(cases.len(), declared("cases,"));
    let lights = declared("light directions each");
    assert!(lights > 0);
    for case in &cases {
        assert_eq!(case.lights.len(), lights, "{}: light directions", case.id);
        assert_eq!(case.values.len(), lights, "{}: answers", case.id);
        assert_eq!(case.pdfs.len(), lights, "{}: densities", case.id);
    }
}

/// The fixture covers every leaf and mode the spec names: each scatter mode
/// of `dielectric_bsdf` on both faces, `conductor_bsdf`, each refracting and
/// reflecting `generalized_schlick_bsdf`, both sheen modes, Oren–Nayar with
/// and without energy compensation, Burley and translucent.
#[test]
fn every_leaf_and_mode_is_replayed() {
    let cases = cases();
    let count = |p: &dyn Fn(&Case) -> bool| cases.iter().filter(|c| p(c)).count();
    for mode in ["R", "T", "RT"] {
        for back in [false, true] {
            let n =
                count(&|c| c.is("dielectric") && c.input("mode") == Some(mode) && c.back() == back);
            assert!(n >= 10, "dielectric {mode} back={back}: {n} cases");
        }
    }
    for mode in ["R", "RT"] {
        let n = count(&|c| c.is("schlick") && c.input("mode") == Some(mode));
        assert!(n >= 10, "schlick {mode}: {n} cases");
    }
    for mode in ["conty_kulla", "zeltner"] {
        let n = count(&|c| c.is("sheen") && c.input("sheen_mode") == Some(mode));
        assert!(n >= 10, "sheen {mode}: {n} cases");
    }
    for ec in ["0", "1"] {
        let n = count(&|c| c.is("oren_nayar") && c.input("energy_compensation") == Some(ec));
        assert!(n >= 8, "oren_nayar ec={ec}: {n} cases");
    }
    for leaf in ["conductor", "burley", "translucent"] {
        let n = count(&|c| c.is(leaf));
        assert!(n >= 3, "{leaf}: {n} cases");
    }
    let anisotropic = count(&|c| {
        let r = floats(&c.input("roughness").unwrap_or("0").replace(',', " "));
        r.len() == 2 && r[0] != r[1]
    });
    assert!(anisotropic >= 8, "{anisotropic} anisotropic cases");
    assert!(count(&|c| c.has_film()) >= 3);
}

/// A NaN or infinity on either side fails the comparison: it is an infinite
/// error, never one that `f32::max` quietly drops.
#[test]
fn a_non_finite_value_is_an_error() {
    let case = &cases()[0];
    let exact = || Shaded {
        albedo: case.albedo,
        filter: case.filter,
        albedo_impl: case.albedo_impl,
        values: case.values.clone(),
        pdfs: case.pdfs.clone(),
    };
    assert_eq!(Errors::of(case, &exact()).worst(), 0.0);
    for bad in [f32::NAN, f32::INFINITY] {
        let mut values = case.values.clone();
        values[3] = Vec3A::new(0.1, bad, 0.1);
        let one_light = Shaded { values, ..exact() };
        assert_eq!(Errors::of(case, &one_light).worst(), f32::INFINITY);
        let mut pdfs = case.pdfs.clone();
        pdfs[3] = bad;
        let one_pdf = Shaded { pdfs, ..exact() };
        assert_eq!(Errors::of(case, &one_pdf).worst(), f32::INFINITY);
        for which in 0..3 {
            let mut s = exact();
            match which {
                0 => s.albedo = Vec3A::splat(bad),
                1 => s.filter = Vec3A::splat(bad),
                _ => s.albedo_impl = Vec3A::splat(bad),
            }
            assert_eq!(Errors::of(case, &s).worst(), f32::INFINITY, "{which}");
        }
    }
}

/// The cases a set of deviations leaves unexcused: each with its error, the
/// deviations that apply to it and the sum of their bounds.
fn unexcused<'a>(
    measured: &'a [(Case<'static>, Errors)],
    deviations: &[&Deviation],
) -> Vec<(&'a Case<'static>, Errors, Vec<&'static str>, f32)> {
    measured
        .iter()
        .filter_map(|(case, errors)| {
            let worst = errors.worst();
            if worst <= TOLERANCE {
                return None;
            }
            let rules: Vec<&Deviation> = deviations
                .iter()
                .copied()
                .filter(|d| (d.applies)(case))
                .collect();
            let bound: f32 = rules.iter().map(|d| d.bound).sum();
            (rules.is_empty() || worst > bound)
                .then(|| (case, *errors, rules.iter().map(|d| d.name).collect(), bound))
        })
        .collect()
}

#[test]
fn materialx_leaves_match_bsdl() {
    let measured: Vec<(Case, Errors)> = cases()
        .into_iter()
        .map(|case| {
            let errors = Errors::of(&case, &shade(&case));
            (case, errors)
        })
        .collect();
    let all: Vec<&Deviation> = DEVIATIONS.iter().collect();

    let failures: Vec<String> = unexcused(&measured, &all)
        .into_iter()
        .map(|(case, e, rules, bound)| {
            format!(
                "{:<32} worst {:.4} (albedo {:.4}, filter {:.4}, impl {:.4}, value {:.4}, pdf {:.4}) > {bound:.4} from [{}]",
                case.id,
                e.worst(),
                e.albedo,
                e.filter,
                e.albedo_impl,
                e.value,
                e.pdf,
                rules.join(", ")
            )
        })
        .collect();

    // A deviation every case passes without excuses nothing: its gap has
    // closed (or shrunk under the others' bounds), and its rule must go.
    let stale: Vec<&str> = DEVIATIONS
        .iter()
        .filter(|d| {
            let others: Vec<&Deviation> =
                all.iter().copied().filter(|o| o.name != d.name).collect();
            unexcused(&measured, &others).is_empty()
        })
        .map(|d| d.name)
        .collect();

    assert!(
        failures.is_empty() && stale.is_empty(),
        "{} cases differ from the reference beyond their deviations:\n{}\n\
         stale deviations (every case passes without them): {stale:?}",
        failures.len(),
        failures.join("\n")
    );
}
