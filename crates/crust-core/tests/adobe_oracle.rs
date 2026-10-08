//! Native `OpenPBR` against Adobe's OpenPBR BSDF reference.
//!
//! `data/adobe_oracle.txt` holds cases (material inputs, a view direction,
//! light directions) with the values Adobe's `openpbr-bsdf` gives for them:
//! the emission toward the view, the directional albedo over a fixed grid,
//! and the BSDF value times the cosine toward each light. The generator is
//! `scripts/adobe_oracle.py`, which builds `scripts/adobe_oracle/probe.cpp`
//! against Adobe's sources at a pinned commit. Here each case is shaded by
//! crust's `OpenPBR` through the `Material` trait and compared.
//!
//! The fixture is committed, so this needs neither a C++ compiler nor
//! Adobe's sources. The pdf is not compared: it is crust's own sampling
//! density, which only has to agree with crust's own values
//! (`material/openpbr/tests.rs` checks that).
//!
//! crust does not match Adobe everywhere. Every known difference is a
//! [`Deviation`]: a named condition on a case's inputs, the gap it stands
//! for, and the largest error it excuses. A case no deviation applies to must
//! match within [`TOLERANCE`]. A deviation that no longer excuses anything is
//! stale and fails the test, so closing a gap means deleting its rule here
//! and its entry in `docs/openpbr_reference_alignment.md`.

use std::collections::BTreeMap;
use std::f32::consts::PI;

use crust_core::{HitRecord, Material, OpenPBR, Ray};
use glam::Vec3A;

const FIXTURE: &str = include_str!("data/adobe_oracle.txt");

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
    emission: Vec3A,
    albedo: Vec3A,
    values: Vec<Vec3A>,
}

impl Case<'_> {
    /// The authored value of `name`, or `None` when the case leaves it at
    /// its default.
    fn input(&self, name: &str) -> Option<&str> {
        self.inputs
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
    }

    /// The scalar input `name` as crust reads it, authored or default.
    fn f(&self, name: &str) -> f32 {
        scalar(&material(self), name)
    }

    fn back_facing(&self) -> bool {
        self.view.z < 0.0
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
            assert_eq!((q.len(), a.len()), (3, 3), "{l:?}");
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
                emission: vec3s(a[0])[0],
                albedo: vec3s(a[1])[0],
                values: vec3s(a[2]),
            }
        })
        .collect()
}

/// The case's inputs on crust's `OpenPBR`, every other input at its default.
fn material(case: &Case) -> OpenPBR {
    let mut m = OpenPBR::default();
    for (name, value) in &case.inputs {
        let v = floats(&value.replace(',', " "));
        let c = || Vec3A::new(v[0], v[1], v[2]);
        match *name {
            "base_weight" => m.base_weight = v[0],
            "base_color" => m.base_color = c(),
            "base_diffuse_roughness" => m.base_diffuse_roughness = v[0],
            "base_metalness" => m.base_metalness = v[0],
            "specular_weight" => m.specular_weight = v[0],
            "specular_color" => m.specular_color = c(),
            "specular_roughness" => m.specular_roughness = v[0],
            "specular_ior" => m.specular_ior = v[0],
            "specular_roughness_anisotropy" => m.specular_roughness_anisotropy = v[0],
            "transmission_weight" => m.transmission_weight = v[0],
            "transmission_color" => m.transmission_color = c(),
            "transmission_depth" => m.transmission_depth = v[0],
            "transmission_scatter" => m.transmission_scatter = c(),
            "transmission_scatter_anisotropy" => m.transmission_scatter_anisotropy = v[0],
            "transmission_dispersion_scale" => m.transmission_dispersion_scale = v[0],
            "transmission_dispersion_abbe_number" => m.transmission_dispersion_abbe_number = v[0],
            "subsurface_weight" => m.subsurface_weight = v[0],
            "subsurface_color" => m.subsurface_color = c(),
            "subsurface_radius" => m.subsurface_radius = v[0],
            "subsurface_radius_scale" => m.subsurface_radius_scale = c(),
            "subsurface_scatter_anisotropy" => m.subsurface_scatter_anisotropy = v[0],
            "fuzz_weight" => m.fuzz_weight = v[0],
            "fuzz_color" => m.fuzz_color = c(),
            "fuzz_roughness" => m.fuzz_roughness = v[0],
            "coat_weight" => m.coat_weight = v[0],
            "coat_color" => m.coat_color = c(),
            "coat_roughness" => m.coat_roughness = v[0],
            "coat_roughness_anisotropy" => m.coat_roughness_anisotropy = v[0],
            "coat_ior" => m.coat_ior = v[0],
            "coat_darkening" => m.coat_darkening = v[0],
            "thin_film_weight" => m.thin_film_weight = v[0],
            "thin_film_thickness" => m.thin_film_thickness = v[0],
            "thin_film_ior" => m.thin_film_ior = v[0],
            "emission_luminance" => m.emission_luminance = v[0],
            "emission_color" => m.emission_color = c(),
            "geometry_opacity" => m.geometry_opacity = v[0],
            "geometry_thin_walled" => m.geometry_thin_walled = v[0] != 0.0,
            other => panic!("{}: crust maps no input {other:?}", case.id),
        }
    }
    m
}

/// The scalar parameter `name` of `m`, for deviation conditions.
fn scalar(m: &OpenPBR, name: &str) -> f32 {
    match name {
        "base_weight" => m.base_weight,
        "base_metalness" => m.base_metalness,
        "base_diffuse_roughness" => m.base_diffuse_roughness,
        "specular_weight" => m.specular_weight,
        "specular_roughness" => m.specular_roughness,
        "specular_roughness_anisotropy" => m.specular_roughness_anisotropy,
        "transmission_weight" => m.transmission_weight,
        "transmission_depth" => m.transmission_depth,
        "subsurface_weight" => m.subsurface_weight,
        "fuzz_weight" => m.fuzz_weight,
        "fuzz_roughness" => m.fuzz_roughness,
        "coat_weight" => m.coat_weight,
        "coat_roughness" => m.coat_roughness,
        "coat_roughness_anisotropy" => m.coat_roughness_anisotropy,
        "thin_film_weight" => m.thin_film_weight,
        "emission_luminance" => m.emission_luminance,
        other => panic!("no scalar {other:?}"),
    }
}

/// The ray toward the shading point along the case's view direction, and the
/// hit it makes: outward normal +Z, so a view below the plane meets the back
/// face, which the hit record face-forwards as crust's intersector does.
fn hit(case: &Case) -> (Ray, HitRecord) {
    let mut rec = HitRecord::new();
    rec.p = Vec3A::ZERO;
    rec.front_face = !case.back_facing();
    rec.normal = if rec.front_face { Vec3A::Z } else { -Vec3A::Z };
    (Ray::new(case.view, -case.view), rec)
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
    emission: Vec3A,
    albedo: Vec3A,
    values: Vec<Vec3A>,
}

fn shade(case: &Case) -> Shaded {
    let m = material(case);
    let (ray, rec) = hit(case);
    let eval = |l: Vec3A| m.eval(&ray, &rec, l).map_or(Vec3A::ZERO, |(f, _)| f);
    let side = if case.back_facing() { -1.0 } else { 1.0 };
    let mut sum = glam::DVec3::ZERO;
    for i in 0..GRID {
        for j in 0..GRID {
            let l = grid_direction(i, j, side);
            if l.z.abs() > 0.0 {
                sum += (eval(l) / l.z.abs()).as_dvec3();
            }
        }
    }
    Shaded {
        emission: m.emitted_at(&ray, &rec, case.view.z.abs()),
        albedo: (sum * (std::f64::consts::PI / (GRID * GRID) as f64)).as_vec3a(),
        values: case.lights.iter().map(|&l| eval(l)).collect(),
    }
}

/// How far crust is from the reference on one case, in three measures that
/// stay meaningful where values are near zero and where they peak:
/// the albedo's absolute error, the emission's error relative to
/// `max(1, |emission|)`, and each value's error relative to
/// `max(0.05, |value|)`.
#[derive(Clone, Copy, Debug, Default)]
struct Errors {
    albedo: f32,
    emission: f32,
    value: f32,
}

impl Errors {
    fn of(case: &Case, got: &Shaded) -> Errors {
        let rel = |g: Vec3A, w: Vec3A, floor: f32| {
            ((g - w).abs() / w.abs().max(Vec3A::splat(floor))).max_element()
        };
        Errors {
            albedo: (got.albedo - case.albedo).abs().max_element(),
            emission: rel(got.emission, case.emission, 1.0),
            value: got
                .values
                .iter()
                .zip(&case.values)
                .map(|(&g, &w)| rel(g, w, 0.05))
                .fold(0.0, f32::max),
        }
    }

    fn worst(self) -> f32 {
        self.albedo.max(self.emission).max(self.value)
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

/// Whether crust shades a diffuse slab: some dielectric, opaque,
/// non-subsurface base.
fn has_diffuse(c: &Case) -> bool {
    c.f("base_weight") > 0.0
        && c.f("base_metalness") < 1.0
        && c.f("transmission_weight") < 1.0
        && c.f("subsurface_weight") < 1.0
}

fn has_dielectric_specular(c: &Case) -> bool {
    c.f("specular_weight") > 0.0 && c.f("base_metalness") < 1.0
}

/// The gaps `docs/openpbr_reference_alignment.md` lists, each as the input
/// condition under which crust's answer differs from Adobe's. A case may be
/// under several; its error is excused up to the sum of their bounds.
const DEVIATIONS: &[Deviation] = &[
    Deviation {
        name: "diffuse-flat-coupling",
        gap: "the diffuse is scaled by a flat 1 - F_avg, even with no specular interface",
        applies: has_diffuse,
        bound: 0.169,
    },
    Deviation {
        name: "dielectric-specular",
        gap: "Schlick Fresnel and no multiple-scattering compensation on the dielectric lobe",
        applies: has_dielectric_specular,
        bound: 0.0317,
    },
    Deviation {
        name: "specular-diffuse-coupling",
        gap: "the diffuse under a specular lobe is not scaled by its directional energy complement",
        applies: |c| has_diffuse(c) && (has_dielectric_specular(c) || c.f("base_metalness") > 0.0),
        bound: 2.19,
    },
    Deviation {
        name: "metal-no-mms",
        gap: "no multiple-scattering compensation on the metal lobe",
        applies: |c| c.f("base_metalness") > 0.0,
        bound: 11.9,
    },
    Deviation {
        name: "coat",
        gap: "the coat lobe's Fresnel and multiple scattering",
        applies: |c| c.f("coat_weight") > 0.0,
        bound: 0.178,
    },
    Deviation {
        name: "coat-over-base",
        gap: "no coat-induced roughening of the base, and the coat's own darkening model",
        applies: |c| {
            c.f("coat_weight") > 0.0
                && (has_diffuse(c)
                    || has_dielectric_specular(c)
                    || c.f("base_metalness") > 0.0
                    || c.f("transmission_weight") > 0.0)
        },
        bound: 0.709,
    },
    Deviation {
        name: "transmission",
        gap: "the transmission lobe",
        applies: |c| c.f("transmission_weight") > 0.0 && c.f("base_metalness") < 1.0,
        bound: 0.103,
    },
    Deviation {
        name: "transmission-under-specular",
        gap: "specular_weight scales the dielectric lobe instead of remapping its F0 to an IOR, which moves the transmission too",
        applies: |c| {
            c.f("transmission_weight") > 0.0
                && has_dielectric_specular(c)
                && c.f("specular_weight") < 1.0
        },
        bound: 0.887,
    },
    Deviation {
        name: "subsurface-as-diffuse",
        gap: "subsurface without transmission is a tinted diffuse, not a refracting volume",
        applies: |c| {
            c.f("subsurface_weight") > 0.0
                && c.f("base_weight") > 0.0
                && c.f("transmission_weight") < 1.0
                && c.f("base_metalness") < 1.0
        },
        bound: 2.73,
    },
    Deviation {
        name: "thin-film",
        gap: "thin-film interference differs from Adobe's, which also reaches the base without a specular lobe",
        applies: |c| c.f("thin_film_weight") > 0.0,
        bound: 0.345,
    },
    Deviation {
        name: "anisotropy",
        gap: "anisotropic GGX differs from Adobe's",
        applies: |c| {
            c.f("specular_roughness_anisotropy") > 0.0 || c.f("coat_roughness_anisotropy") > 0.0
        },
        bound: 0.603,
    },
    Deviation {
        name: "interior",
        gap: "a closed surface hit from inside still emits, and keeps its coat and fuzz",
        applies: |c| c.back_facing() && c.input("geometry_thin_walled") != Some("1"),
        bound: 3.06,
    },
];

/// Prints every case's applicable deviations and errors, and for each
/// deviation the worst error among the cases it alone applies to: the
/// numbers its `bound` is set from. Run it after regenerating the fixture
/// or closing a gap:
///
/// ```text
/// cargo test -p crust-core --test adobe_oracle -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a measurement, not a check"]
fn report_deviations() {
    let mut alone: BTreeMap<&str, f32> = BTreeMap::new();
    for case in &cases() {
        let errors = Errors::of(case, &shade(case));
        let rules: Vec<&str> = DEVIATIONS
            .iter()
            .filter(|d| (d.applies)(case))
            .map(|d| d.name)
            .collect();
        if let [only] = rules[..] {
            let e = alone.entry(only).or_default();
            *e = e.max(errors.worst());
        }
        println!(
            "{:<24} worst {:>8.4}  albedo {:.4} emission {:.4} value {:.4}  {}",
            case.id,
            errors.worst(),
            errors.albedo,
            errors.emission,
            errors.value,
            rules.join(", ")
        );
    }
    println!("\nworst error where a deviation applies alone (its bound):");
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

/// The fixture header's `N cases`.
fn declared_cases() -> usize {
    let line = FIXTURE
        .lines()
        .find(|l| l.starts_with('#') && l.contains(" cases,"))
        .expect("the fixture header states its case count");
    line.trim_start_matches('#')
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no case count in {line:?}"))
}

/// Every case the generator wrote is in the fixture, with an answer for each
/// light, so a fixture that lost rows cannot pass by checking less.
#[test]
fn the_fixture_is_complete() {
    let cases = cases();
    assert_eq!(cases.len(), declared_cases());
    for case in &cases {
        assert_eq!(case.values.len(), case.lights.len(), "{}", case.id);
        assert!(!case.lights.is_empty(), "{}", case.id);
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
fn openpbr_matches_the_adobe_reference() {
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
                "{:<24} worst {:.4} (albedo {:.4}, emission {:.4}, value {:.4}) > {bound:.4} from [{}]",
                case.id,
                e.worst(),
                e.albedo,
                e.emission,
                e.value,
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
