//! Implicit-shape tessellation against OpenUSD's own GeomUtil generators.
//!
//! `data/geomutil_oracle.txt` holds cases (a generator, its radial and cap
//! counts, its dimensions and `axis`) with the mesh GeomUtil produced for
//! each: topology, points and normals, written as Storm writes them. The
//! generator is `scripts/geomutil_oracle.py`, which builds
//! `scripts/geomutil_oracle/probe.cpp` against a pinned conda-forge
//! `openusd`. Here each case is tessellated by crust's transcription
//! (`usd_import/implicit.rs`) and compared:
//!
//! - face counts and indices exactly;
//! - points within [`TOLERANCE`] of the case's extent (its largest
//!   coordinate): GeomUtil computes in `float` when it fills a
//!   `VtVec3fArray`, crust in `f64`, so the two agree to float rounding of
//!   the angles, not bit for bit — `cos(π/2)` is −4.4e-8 there and 6e-17 here;
//! - vertex normals within [`TOLERANCE`], where GeomUtil's interpolation is
//!   `vertex`. A `uniform` or `constant` normal (cube, plane) is a faceted
//!   surface, which crust expresses as no normals at all.
//!
//! The fixture is committed, so this needs neither a C++ compiler nor OpenUSD.

use crust_core::implicit_shapes::{Axis, Tessellation, capsule, cone, cuboid, cylinder, plane};

const FIXTURE: &str = include_str!("data/geomutil_oracle.txt");

/// The largest difference a point coordinate may have, relative to its case's
/// extent; also the absolute one a unit normal may have.
const TOLERANCE: f32 = 1e-6;

struct Case<'a> {
    id: &'a str,
    family: &'a str,
    crust: Tessellation,
    counts: Vec<i32>,
    indices: Vec<i32>,
    points: Vec<[f32; 3]>,
    interpolation: &'a str,
    normals: Vec<[f32; 3]>,
}

fn floats(s: &str) -> Vec<f32> {
    s.split_whitespace()
        .map(|t| t.parse().unwrap_or_else(|e| panic!("{t}: {e}")))
        .collect()
}

fn ints(s: &str) -> Vec<i32> {
    s.split_whitespace()
        .map(|t| t.parse().unwrap_or_else(|e| panic!("{t}: {e}")))
        .collect()
}

fn vecs(s: &str) -> Vec<[f32; 3]> {
    let f = floats(s);
    assert_eq!(f.len() % 3, 0, "a whole number of vectors");
    f.chunks(3).map(|c| [c[0], c[1], c[2]]).collect()
}

fn axis(s: &str) -> Axis {
    match s {
        "X" => Axis::X,
        "Y" => Axis::Y,
        "Z" => Axis::Z,
        _ => panic!("axis {s}"),
    }
}

/// crust's tessellation of a case's inputs: `<family> <params>`.
fn tessellate(family: &str, params: &[&str]) -> Tessellation {
    let f = |i: usize| -> f64 { params[i].parse().unwrap() };
    let n = |i: usize| -> usize { params[i].parse().unwrap() };
    match family {
        "cube" => cuboid(f(0)),
        "plane" => plane(f(0), f(1), axis(params[2])),
        "cylinder" => cylinder(n(0), f(1), f(2), f(3), axis(params[4])),
        "cone" => cone(n(0), f(1), f(2), axis(params[3])),
        "capsule" => capsule(n(0), n(1), f(2), f(3), f(4), axis(params[5])),
        _ => panic!("family {family}"),
    }
}

fn cases() -> Vec<Case<'static>> {
    FIXTURE
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|line| {
            let (inputs, answer) = line.split_once(" || ").expect("inputs || answer");
            let mut toks = inputs.split_whitespace();
            let id = toks.next().expect("id");
            let family = toks.next().expect("family");
            let params: Vec<&str> = toks.collect();
            let fields: Vec<&str> = answer.split(" | ").collect();
            assert_eq!(fields.len(), 6, "{id}: six answer fields");
            Case {
                id,
                family,
                crust: tessellate(family, &params),
                counts: ints(fields[1]),
                indices: ints(fields[2]),
                points: vecs(fields[3]),
                interpolation: fields[4].trim(),
                normals: vecs(fields[5]),
            }
        })
        .collect()
}

fn max_diff(a: &[[f32; 3]], b: &[[f32; 3]]) -> f32 {
    a.iter()
        .zip(b)
        .flat_map(|(p, q)| (0..3).map(move |i| (p[i] - q[i]).abs()))
        .fold(0.0, f32::max)
}

#[test]
fn every_case_matches_geomutil() {
    let cases = cases();
    assert!(!cases.is_empty());
    let mut failures = Vec::new();
    for c in &cases {
        let t = &c.crust;
        if t.counts != c.counts || t.indices != c.indices {
            failures.push(format!("{}: topology differs", c.id));
            continue;
        }
        if t.points.len() != c.points.len() {
            failures.push(format!(
                "{}: {} points, GeomUtil has {}",
                c.id,
                t.points.len(),
                c.points.len()
            ));
            continue;
        }
        let extent = c
            .points
            .iter()
            .flatten()
            .fold(0.0f32, |m, v| m.max(v.abs()));
        let points = max_diff(&t.points, &c.points);
        if points > TOLERANCE * extent {
            failures.push(format!(
                "{}: points differ by {points} (extent {extent})",
                c.id
            ));
        }
        match (&t.normals, c.interpolation) {
            (Some(n), "vertex") => {
                let normals = max_diff(n, &c.normals);
                if n.len() != c.normals.len() || normals > TOLERANCE {
                    failures.push(format!("{}: normals differ by {normals}", c.id));
                }
            }
            (None, "uniform" | "constant") => {}
            (n, interp) => failures.push(format!(
                "{}: crust has {} normals, GeomUtil's are {interp}",
                c.id,
                if n.is_some() { "vertex" } else { "no" }
            )),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The fixture holds what the requirement names: every family, every axis,
/// a cone and both kinds of unequal radii, N = 3 and M = 1.
#[test]
fn the_fixture_covers_what_it_pins() {
    let cases = cases();
    let has = |pred: &dyn Fn(&Case) -> bool, what: &str| {
        assert!(cases.iter().any(pred), "no case with {what}");
    };
    for family in ["cube", "plane", "cylinder", "cone", "capsule"] {
        has(&|c| c.family == family, family);
    }
    for a in ["-X", "-Y", "-Z"] {
        for family in ["plane", "cylinder", "cone", "capsule"] {
            has(
                &|c| c.family == family && c.id.ends_with(a),
                &format!("{family} on axis {a}"),
            );
        }
    }
    has(&|c| c.id == "cylinder1-cone", "radiusTop 0");
    has(&|c| c.id == "capsule1-narrowing", "an unequal capsule");
    has(
        &|c| c.id == "capsule1-widening",
        "an unequal capsule, widening",
    );
    has(&|c| c.id.starts_with("cylinder-n3-"), "N = 3");
    has(&|c| c.id.contains("-m1"), "M = 1");
}
