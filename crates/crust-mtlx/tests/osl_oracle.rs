//! crust-mtlx's node semantics against the MaterialX reference implementation.
//!
//! `data/osl_oracle.txt` holds one-node cases whose expected value came from
//! MaterialX's own OSL code generator, compiled with `oslc` and run with
//! `testshade` (`scripts/osl_oracle.py`, which is also where the inputs are
//! chosen). Here each case is rebuilt as a `.mtlx` document, compiled with
//! [`Compiler`] and evaluated with [`Program::eval`], and the lanes compared.
//!
//! The fixture is committed, so this needs neither OSL nor MaterialX. A
//! mismatch is either a crust bug or a deliberate deviation; the deliberate
//! ones are listed in [`deviation`] with the reason, and nothing else may
//! differ.

use crust_mtlx::{Compiler, Doc, ShadeCtx, TextureRef, Val};
use glam::Vec3A;
use std::collections::BTreeMap;

const FIXTURE: &str = include_str!("data/osl_oracle.txt");

fn decline(_: &str, _: Option<&str>) -> Option<TextureRef> {
    None
}

struct Case<'a> {
    line: usize,
    nodedef: &'a str,
    category: &'a str,
    out_type: &'a str,
    output: Option<&'a str>,
    /// `(name, type, lanes)`, lanes as authored in the fixture.
    inputs: Vec<(&'a str, &'a str, Vec<f32>)>,
    /// The raw text of each input's value, for the document.
    texts: Vec<&'a str>,
    expected: Vec<f32>,
}

impl Case<'_> {
    fn input(&self, name: &str) -> Option<&[f32]> {
        self.inputs
            .iter()
            .find(|(n, _, _)| *n == name)
            .map(|(_, _, v)| v.as_slice())
    }
}

fn parse_lane(s: &str) -> f32 {
    match s {
        "inf" => f32::INFINITY,
        "-inf" => f32::NEG_INFINITY,
        "nan" | "-nan" => f32::NAN,
        _ => s.parse().unwrap_or_else(|_| panic!("bad lane {s:?}")),
    }
}

fn cases() -> Vec<Case<'static>> {
    FIXTURE
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.starts_with('#') && !l.trim().is_empty())
        .map(|(i, l)| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 6, "line {}: {l:?}", i + 1);
            let mut inputs = Vec::new();
            let mut texts = Vec::new();
            if f[4] != "-" {
                for spec in f[4].split(';') {
                    let (name, rest) = spec.split_once(':').expect("name:type=value");
                    let (ty, value) = rest.split_once('=').expect("type=value");
                    inputs.push((name, ty, value.split(',').map(parse_lane).collect()));
                    texts.push(value);
                }
            }
            Case {
                line: i + 1,
                nodedef: f[0],
                category: f[1],
                out_type: f[2],
                output: (f[3] != "-").then_some(f[3]),
                inputs,
                texts,
                expected: f[5].split(' ').map(parse_lane).collect(),
            }
        })
        .collect()
}

fn v3(l: &[f32]) -> Vec3A {
    Vec3A::new(l[0], l[1], l[2])
}

/// Rebuilds the case as a one-node document and evaluates it.
fn eval(case: &Case) -> Val {
    let node_type = if case.output.is_some() {
        "multioutput"
    } else {
        case.out_type
    };
    let mut xml = format!(
        r#"<materialx><{} name="n" type="{node_type}">"#,
        case.category
    );
    for ((name, ty, _), text) in case.inputs.iter().zip(&case.texts) {
        xml += &format!(r#"<input name="{name}" type="{ty}" value="{text}" />"#);
    }
    xml += &format!("</{}></materialx>", case.category);

    // `normalmap`'s frame is an input in MaterialX but the shading point in
    // crust, which takes the bitangent as `normal x tangent` — what the
    // fixture's frames author.
    let (normal, tangent) = match (case.input("normal"), case.input("tangent")) {
        (Some(n), Some(t)) => (v3(n), v3(t)),
        _ => (Vec3A::Z, Vec3A::X),
    };
    let ctx = ShadeCtx {
        uv: (0.0, 0.0),
        normal,
        tangent,
        view: -Vec3A::Z,
        position: Vec3A::ZERO,
        uv_width: 0.0,
    };

    let doc = Doc::parse(&xml).unwrap_or_else(|e| panic!("line {}: {e:?}", case.line));
    let loader = decline;
    let mut c = Compiler::new(&doc, &loader);
    let slot = c.compile_named("", "n", case.output);
    assert!(
        c.unsupported().is_empty(),
        "line {}: {} compiled as unsupported",
        case.line,
        case.nodedef
    );
    let mut slots = Vec::new();
    c.program().eval(&ctx, &mut slots);
    slots[slot as usize]
}

/// Whether two lanes agree: equal as non-finite values, or within a tolerance
/// scaled to the magnitude. The two sides use different `libm`s (OSL's LLVM
/// intrinsics against Rust's), so transcendentals may differ in the last ulps.
fn close(got: f32, want: f32) -> bool {
    if want.is_nan() {
        return got.is_nan();
    }
    if !want.is_finite() || !got.is_finite() {
        return got == want;
    }
    (got - want).abs() <= 1e-5 * want.abs().max(1.0)
}

/// Lane `i` of input `name`, broadcasting a one-lane value, or `default` when
/// the input is unauthored.
fn lane(case: &Case, name: &str, i: usize, default: f32) -> f32 {
    match case.input(name) {
        Some([x]) => *x,
        Some(l) => l.get(i).copied().unwrap_or(default),
        None => default,
    }
}

/// A difference crust makes on purpose in lane `i`, with the reason. `None`
/// means the lane must match the reference. Each rule names the one input
/// condition under which it applies, so it cannot hide a difference anywhere
/// else — and every one is a guard or a clamp that keeps a value finite or
/// physical where the reference would not.
fn deviation(case: &Case, i: usize, want: f32) -> Option<&'static str> {
    match case.category {
        "divide" if lane(case, "in2", i, 1.0) == 0.0 => {
            Some("divide: a zero divisor gives 0, not ±inf / NaN")
        }
        "remap" if lane(case, "inhigh", i, 1.0) == lane(case, "inlow", i, 0.0) => {
            Some("remap: an empty input range gives outlow, not ±inf / NaN")
        }
        // OSL's own `mod` is inconsistent here: the float one returns the
        // dividend, which crust follows everywhere; the vector one gives NaN.
        "modulo" if lane(case, "in2", i, 1.0) == 0.0 && want.is_nan() => {
            Some("modulo: a zero divisor returns the dividend, not NaN")
        }
        // crust never reads the `nodedef` attribute, and with no `in` there is
        // nothing to tell `convert_color3_color4` from `convert_float_color4`.
        "convert" if case.inputs.is_empty() => {
            Some("convert: an unauthored `in` is a zero float, so a widened alpha is 0, not 1")
        }
        "normalmap" if 2.0 * lane(case, "in", 2, 1.0) - 1.0 < 1e-4 => {
            Some("normalmap: the decoded z is raised to 1e-4, off the tangent plane")
        }
        "artistic_ior"
            if !(0.0..=1.0).contains(&lane(case, "edge_color", i, [0.998, 0.981, 0.751][i])) =>
        {
            Some("artistic_ior: edge_color is clamped to [0, 1]")
        }
        _ => None,
    }
}

/// The fixture header's `N random cases per signature, S signatures`.
fn declared_coverage() -> (usize, usize) {
    let line = FIXTURE
        .lines()
        .find(|l| l.contains("random cases per signature"))
        .expect("the fixture header states its coverage");
    let number_before = |word: &str| -> usize {
        let head = &line[..line
            .find(word)
            .unwrap_or_else(|| panic!("no {word:?} in {line:?}"))];
        head.split_whitespace()
            .last()
            .and_then(|n| n.trim_start_matches(',').parse().ok())
            .unwrap_or_else(|| panic!("no count before {word:?} in {line:?}"))
    };
    (
        number_before("random cases per signature"),
        number_before("signatures."),
    )
}

/// Every signature the generator enumerated is in the fixture with all its
/// cases — the defaults case plus the random ones, per output — so a
/// fixture that lost rows cannot pass by checking less.
#[test]
fn the_fixture_is_complete() {
    let (random, signatures) = declared_coverage();
    let mut per_output: BTreeMap<(&str, Option<&str>), usize> = BTreeMap::new();
    for case in cases() {
        *per_output.entry((case.nodedef, case.output)).or_default() += 1;
    }
    let nodedefs: std::collections::BTreeSet<&str> = per_output.keys().map(|k| k.0).collect();
    assert_eq!(nodedefs.len(), signatures, "signatures in the fixture");
    for ((nd, output), n) in &per_output {
        assert_eq!(*n, random + 1, "{nd} {output:?}: cases");
    }
}

#[test]
fn nodes_match_the_materialx_osl_reference() {
    let cases = cases();

    let mut failures: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut deviations: BTreeMap<&str, usize> = BTreeMap::new();
    for case in &cases {
        let got = eval(case);
        let lanes: Vec<f32> = (0..case.expected.len()).map(|i| got.v[i]).collect();
        // The width is part of the value: a `float` zero where a `vector4`
        // was due matches four zero lanes but reads as a scalar downstream.
        let mut wrong = got.arity as usize != case.expected.len();
        for (i, (&g, &w)) in lanes.iter().zip(&case.expected).enumerate() {
            if close(g, w) {
                continue;
            }
            match deviation(case, i, w) {
                Some(why) => *deviations.entry(why).or_default() += 1,
                None => wrong = true,
            }
        }
        if !wrong {
            continue;
        }
        let inputs: Vec<String> = case
            .inputs
            .iter()
            .zip(&case.texts)
            .map(|((n, _, _), t)| format!("{n}={t}"))
            .collect();
        failures.entry(case.nodedef).or_default().push(format!(
            "line {}: {}{} -> crust {lanes:?} ({} lanes), OSL {:?}",
            case.line,
            inputs.join(" "),
            case.output.map(|o| format!(" [{o}]")).unwrap_or_default(),
            got.arity,
            case.expected
        ));
    }

    for (why, n) in &deviations {
        eprintln!("{n:4} lanes deliberately differ — {why}");
    }
    if !failures.is_empty() {
        let mut report = String::new();
        for (nd, lines) in &failures {
            report += &format!("{nd} ({} cases)\n", lines.len());
            for l in lines.iter().take(4) {
                report += &format!("    {l}\n");
            }
        }
        panic!(
            "{} of {} signatures differ from the MaterialX OSL reference:\n{report}",
            failures.len(),
            cases
                .iter()
                .map(|c| c.nodedef)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        );
    }
}
