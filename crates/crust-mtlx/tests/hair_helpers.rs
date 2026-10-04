//! MaterialX's three hair helper nodes against their reference.
//!
//! `data/hair_helpers.txt` holds one-node cases whose expected value is
//! MaterialX's genglsl (`mx_chiang_hair_bsdf.glsl`), transcribed in float64 by
//! `scripts/hair_reference.py`: these nodes' genosl implementations are
//! placeholders, so the OSL oracle (`osl_oracle.rs`) cannot pin them. The
//! fixture has `osl_oracle.txt`'s format, and each case is rebuilt, compiled
//! and evaluated the same way.

use crust_mtlx::{Compiler, Doc, Host, ShadeCtx, TextureRef, Val};
use glam::Vec3A;

const FIXTURE: &str = include_str!("data/hair_helpers.txt");

fn decline(_: &str, _: Option<&str>) -> Option<TextureRef> {
    None
}

struct Case<'a> {
    line: usize,
    category: &'a str,
    out_type: &'a str,
    output: Option<&'a str>,
    /// `(name, type, value text)`.
    inputs: Vec<(&'a str, &'a str, &'a str)>,
    expected: Vec<f32>,
}

fn cases() -> Vec<Case<'static>> {
    FIXTURE
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.starts_with('#') && !l.trim().is_empty())
        .map(|(i, l)| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 6, "line {}: {l:?}", i + 1);
            let inputs = if f[4] == "-" {
                Vec::new()
            } else {
                f[4].split(';')
                    .map(|spec| {
                        let (name, rest) = spec.split_once(':').expect("name:type=value");
                        let (ty, value) = rest.split_once('=').expect("type=value");
                        (name, ty, value)
                    })
                    .collect()
            };
            Case {
                line: i + 1,
                category: f[1],
                out_type: f[2],
                output: (f[3] != "-").then_some(f[3]),
                inputs,
                expected: f[5].split(' ').map(|x| x.parse().unwrap()).collect(),
            }
        })
        .collect()
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
    for (name, ty, value) in &case.inputs {
        xml += &format!(r#"<input name="{name}" type="{ty}" value="{value}" />"#);
    }
    xml += &format!("</{}></materialx>", case.category);
    let ctx = ShadeCtx {
        uv: (0.0, 0.0),
        normal: Vec3A::Z,
        tangent: Vec3A::X,
        view: -Vec3A::Z,
        position: Vec3A::ZERO,
        uv_width: 0.0,
    };
    let doc = Doc::parse(&xml).unwrap_or_else(|e| panic!("line {}: {e:?}", case.line));
    let loader = decline;
    let mut c = Compiler::new(&doc, &Host::new(&loader));
    let slot = c.compile_named("", "n", case.output);
    assert!(
        c.unsupported.is_empty(),
        "line {}: {} compiled as unsupported",
        case.line,
        case.category
    );
    let mut slots = Vec::new();
    c.program.eval(&ctx, &mut slots);
    slots[slot as usize]
}

/// `osl_oracle.rs`'s tolerance: relative, with an absolute floor at 1.
fn close(got: f32, want: f32) -> bool {
    got.is_finite() && (got - want).abs() <= 1e-5 * want.abs().max(1.0)
}

#[test]
fn the_hair_reference_cases_pass() {
    let cases = cases();
    let mut failures = Vec::new();
    for case in &cases {
        let got = eval(case);
        let lanes = case.expected.len();
        if got.arity as usize != lanes {
            failures.push(format!(
                "line {}: width {} want {lanes}",
                case.line, got.arity
            ));
            continue;
        }
        for (i, &want) in case.expected.iter().enumerate() {
            if !close(got.v[i], want) {
                failures.push(format!(
                    "line {} {} {:?} lane {i}: got {} want {want}",
                    case.line, case.category, case.output, got.v[i]
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every signature is in the fixture with all its cases, so a truncated or
/// regenerated-short file cannot pass by testing less.
#[test]
fn the_fixture_is_complete() {
    let cases = cases();
    let count = |category: &str, output: Option<&str>| {
        cases
            .iter()
            .filter(|c| c.category == category && c.output == output)
            .count()
    };
    // The default case, four edge cases and sixteen random ones.
    for output in ["roughness_R", "roughness_TT", "roughness_TRT"] {
        assert_eq!(count("chiang_hair_roughness", Some(output)), 21, "{output}");
    }
    assert_eq!(count("chiang_hair_absorption_from_color", None), 21);
    assert_eq!(count("deon_hair_absorption_from_melanin", None), 21);
}
