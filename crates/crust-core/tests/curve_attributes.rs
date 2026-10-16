//! Curve attributes reaching materials: a curve hit's place on its strand
//! (`curve:v`, `curve:index`, `curve:random`), the curve primvars a material
//! reads by name, and the chart, end to end from USD.
//!
//! Each stage binds a pure emitter whose colour is the graph under test, so a
//! hit's `emitted_at` reads back exactly what the material computed there.

use crust_core::{Ray, Scene, Vec3A, WarningCode};
use std::path::PathBuf;

/// A directory of its own per test, for the stage and its `.mtlx`.
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir()
        .join("crust_curve_attribute_tests")
        .join(name);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// A stage of `body` under `/World`, with `/World/Looks/M` a pure emitter
/// whose colour is the `color3` node `value` of `graph`.
fn load(name: &str, graph: &str, body: &str) -> Scene {
    let d = dir(name);
    let mtlx = d.join("m.mtlx");
    std::fs::write(
        &mtlx,
        format!(
            r#"<?xml version="1.0"?>
<materialx version="1.39">
  {graph}
  <uniform_edf name="edf" type="EDF"><input name="color" type="color3" nodename="value" /></uniform_edf>
  <surface name="surf" type="surfaceshader"><input name="edf" type="EDF" nodename="edf" /></surface>
  <surfacematerial name="M" type="material">
    <input name="surfaceshader" type="surfaceshader" nodename="surf" />
  </surfacematerial>
</materialx>
"#
        ),
    )
    .expect("write mtlx");
    let stage = d.join("stage.usda");
    std::fs::write(
        &stage,
        format!(
            r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
    def Scope "Looks"
    {{
        def Material "M" (
            prepend references = @{}@</MaterialX/Materials/M>
        )
        {{
        }}
    }}
{body}
}}
"#,
            mtlx.display()
        ),
    )
    .expect("write stage");
    Scene::from_usd(&stage).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// A float `geompropvalue` of `prop` as a grey colour.
fn property(prop: &str) -> String {
    format!(
        r#"<geompropvalue name="value" type="color3">
             <input name="geomprop" type="string" value="{prop}" />
             <input name="default" type="color3" value="0.5, 0.5, 0.5" />
           </geompropvalue>"#
    )
}

/// What the material emits where a ray from `origin` along `dir` first hits.
fn emitted(scene: &Scene, origin: Vec3A, dir: Vec3A) -> Vec3A {
    let ray = Ray::new(origin, dir);
    let hit = scene
        .world
        .intersect(&ray, 1e-4, 1e4)
        .unwrap_or_else(|| panic!("no hit from {origin} along {dir}"));
    hit.mat.emitted_at(&ray, &hit.rec, 1.0)
}

/// What a ray straight down the -z axis at `(x, y)` reads.
fn at(scene: &Scene, x: f32, y: f32) -> Vec3A {
    emitted(scene, Vec3A::new(x, y, 5.0), -Vec3A::Z)
}

fn close(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

/// A `BasisCurves` prim bound to `M`, with `extra` authored on it.
fn curves(name: &str, ty: &str, basis: &str, counts: &str, points: &str, extra: &str) -> String {
    format!(
        r#"
    def BasisCurves "{name}" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token type = "{ty}"
        uniform token basis = "{basis}"
        int[] curveVertexCounts = [{counts}]
        point3f[] points = [{points}]
        float[] widths = [0.2] (interpolation = "constant")
        rel material:binding = </World/Looks/M>
        {extra}
    }}"#
    )
}

/// `n` evenly spaced collinear points along x from `x0`, at height `y`.
fn row(n: usize, x0: f32, y: f32) -> String {
    (0..n)
        .map(|i| format!("({}, {y}, 0)", x0 + i as f32))
        .collect::<Vec<_>>()
        .join(", ")
}

#[test]
fn v_runs_root_to_tip_on_a_cubic_strand() {
    // A uniform B-spline over evenly spaced collinear points is the straight
    // line, linear in its parameter: 7 points, 4 spans, x from 1 to 5.
    let s = load(
        "bspline_v",
        &property("curve:v"),
        &curves("C", "cubic", "bspline", "7", &row(7, 0.0, 0.0), ""),
    );
    for x in [1.05f32, 2.0, 2.5, 3.0, 4.2, 4.95] {
        let v = at(&s, x, 0.0).x;
        assert!(close(v, (x - 1.0) / 4.0, 2e-3), "x {x}: v {v}");
    }
}

#[test]
fn v_on_a_linear_curve_follows_its_points() {
    let s = load(
        "linear_v",
        &property("curve:v"),
        &curves("C", "linear", "bezier", "5", &row(5, 0.0, 0.0), ""),
    );
    // The third segment's midpoint: v = 2.5 / 4.
    assert!(close(at(&s, 2.5, 0.0).x, 0.625, 1e-3));
    assert!(close(at(&s, 0.1, 0.0).x, 0.025, 1e-3));
}

#[test]
fn v_on_a_trimmed_nurbs_curve_spans_the_trimmed_domain() {
    let body = r#"
    def NurbsCurves "N" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] curveVertexCounts = [4]
        int[] order = [4]
        double[] knots = [0, 0, 0, 0, 1, 1, 1, 1]
        double2[] ranges = [(0, 0.5)]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (2, 0, 0), (3, 0, 0)]
        float[] widths = [0.2] (interpolation = "constant")
        rel material:binding = </World/Looks/M>
    }"#;
    let s = load("nurbs_v", &property("curve:v"), body);
    // x = 3t over [0, 0.5]: v = t / 0.5.
    for x in [0.15f32, 0.75, 1.35] {
        let v = at(&s, x, 0.0).x;
        assert!(close(v, x / 1.5, 2e-3), "x {x}: v {v}");
    }
}

#[test]
fn a_nurbs_prim_of_mixed_orders_keeps_each_curve_s_v() {
    // An order-2 curve and an order-4 one in one prim: the linear one is
    // carried as a straight cubic so one table indexes both.
    let body = r#"
    def NurbsCurves "N" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] curveVertexCounts = [2, 4]
        int[] order = [2, 4]
        double[] knots = [0, 0, 1, 1, 0, 0, 0, 0, 1, 1, 1, 1]
        point3f[] points = [(0, 0, 0), (4, 0, 0), (0, 2, 0), (1, 2, 0), (2, 2, 0), (3, 2, 0)]
        float[] widths = [0.2] (interpolation = "constant")
        rel material:binding = </World/Looks/M>
    }"#;
    let s = load("nurbs_mixed", &property("curve:v"), body);
    assert!(close(at(&s, 1.0, 0.0).x, 0.25, 2e-3));
    assert!(close(at(&s, 2.25, 2.0).x, 0.75, 2e-3));
}

#[test]
fn curve_index_counts_within_the_prim() {
    let points = [row(4, 0.0, 0.0), row(4, 0.0, 1.0), row(4, 0.0, 2.0)].join(", ");
    let s = load(
        "index",
        &property("curve:index"),
        &curves("C", "linear", "bezier", "4, 4, 4", &points, ""),
    );
    for k in 0..3 {
        assert_eq!(at(&s, 1.5, k as f32).x, k as f32);
    }
}

#[test]
fn off_a_curve_the_property_reads_its_default() {
    let body = format!(
        r#"{}
    def Mesh "Floor" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, -1, -1), (1, -1, -1), (1, 1, -1), (-1, 1, -1)]
        rel material:binding = </World/Looks/M>
    }}"#,
        curves("C", "linear", "bezier", "2", "(5, 0, 0), (6, 0, 0)", "")
    );
    let s = load("default_off_curve", &property("curve:v"), &body);
    assert_eq!(at(&s, 0.0, 0.0), Vec3A::splat(0.5));
}

#[test]
fn instanced_strands_keep_their_prototype_s_v_and_index() {
    // The same two-curve clump placed by a native instance and by a
    // PointInstancer at two rotations: corresponding points read alike.
    let clump = curves(
        "Clump",
        "linear",
        "bezier",
        "2, 2",
        "(0, 0, 0), (4, 0, 0), (0, 1, 0), (4, 1, 0)",
        "",
    );
    let graph = r#"
      <geompropvalue name="v" type="float"><input name="geomprop" type="string" value="curve:v" /></geompropvalue>
      <geompropvalue name="i" type="float"><input name="geomprop" type="string" value="curve:index" /></geompropvalue>
      <combine3 name="value" type="color3">
        <input name="in1" type="float" nodename="v" />
        <input name="in2" type="float" nodename="i" />
        <input name="in3" type="float" value="0" />
      </combine3>"#;
    let body = format!(
        r#"
    def Xform "Protos"
    {{
        def Xform "P" {{ {clump} }}
    }}
    def Xform "Native" (instanceable = true; prepend references = </World/Protos/P>)
    {{
        double3 xformOp:translate = (0, 10, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def PointInstancer "Scatter"
    {{
        rel prototypes = [</World/Scatter/Protos/P>]
        int[] protoIndices = [0, 0]
        point3f[] positions = [(0, 20, 0), (0, 30, 0)]
        quath[] orientations = [(1, 0, 0, 0), (0.7071068, 0, 0, 0.7071068)]
        def Scope "Protos"
        {{
            def Xform "P" {{ {clump} }}
        }}
    }}"#
    );
    let s = load("instanced", graph, &body);
    let top = at(&s, 1.0, 1.0);
    assert!(close(top.x, 0.25, 1e-3) && top.y == 1.0, "{top}");
    assert_eq!(at(&s, 1.0, 11.0), top, "native instance");
    assert_eq!(at(&s, 1.0, 21.0), top, "PointInstancer, unrotated");
    // Rotated 90° about z: the strand along +x now runs along +y, and the
    // second curve sits at x = -1.
    assert_eq!(at(&s, -1.0, 31.0), top, "PointInstancer, rotated");
}

#[test]
fn curve_random_is_stable_and_differs_between_prims() {
    let two = [
        curves("A", "linear", "bezier", "2", "(0, 0, 0), (1, 0, 0)", ""),
        curves("B", "linear", "bezier", "2", "(0, 2, 0), (1, 2, 0)", ""),
    ]
    .join("");
    let s = load("random", &property("curve:random"), &two);
    let (a, b) = (at(&s, 0.5, 0.0).x, at(&s, 0.5, 2.0).x);
    assert!((0.0..1.0).contains(&a) && (0.0..1.0).contains(&b));
    assert_ne!(a, b, "two prims repeat each other");
    // Along the strand it does not change; across reloads neither.
    assert_eq!(at(&s, 0.9, 0.0).x, a);
    let again = load("random_again", &property("curve:random"), &two);
    assert_eq!(at(&again, 0.5, 0.0).x, a);
}

#[test]
fn curve_random_is_uniform_over_many_curves() {
    let n = 10_000;
    let counts = vec!["2"; n].join(", ");
    let points: String = (0..n)
        .map(|i| format!("({i}, 0, 0), ({i}.5, 0, 0)"))
        .collect::<Vec<_>>()
        .join(", ");
    let s = load(
        "random_uniform",
        &property("curve:random"),
        &curves("C", "linear", "bezier", &counts, &points, ""),
    );
    let mut bins = [0usize; 10];
    for i in 0..n {
        let r = at(&s, i as f32 + 0.25, 0.0).x;
        assert!((0.0..1.0).contains(&r), "curve {i}: {r}");
        bins[(r * 10.0) as usize] += 1;
    }
    for (k, &b) in bins.iter().enumerate() {
        // 10% is 3.3 standard deviations of a bin of 1000.
        assert!(b.abs_diff(n / 10) * 10 <= n / 10, "bin {k}: {b}");
    }
}

/// A `color3` `geompropvalue` of the primvar `name`, default black.
fn primvar(name: &str) -> String {
    format!(
        r#"<geompropvalue name="value" type="color3">
             <input name="geomprop" type="string" value="{name}" />
           </geompropvalue>"#
    )
}

#[test]
fn a_uniform_primvar_colours_each_strand() {
    let points = [row(2, 0.0, 0.0), row(2, 0.0, 1.0), row(2, 0.0, 2.0)].join(", ");
    let s = load(
        "uniform",
        &primvar("hairColor"),
        &curves(
            "C",
            "linear",
            "bezier",
            "2, 2, 2",
            &points,
            r#"color3f[] primvars:hairColor = [(1, 0, 0), (0, 1, 0), (0, 0, 1)] (interpolation = "uniform")"#,
        ),
    );
    assert_eq!(at(&s, 0.5, 0.0), Vec3A::X);
    assert_eq!(at(&s, 0.5, 1.0), Vec3A::Y);
    assert_eq!(at(&s, 0.5, 2.0), Vec3A::Z);
}

#[test]
fn a_vertex_ramp_follows_the_curve_s_basis() {
    // A B-spline's value at parameter t of span s is the basis-weighted sum
    // of its four control values — inside the span, not only at its ends.
    let control = [0.0f32, 0.0, 1.0, 3.0, 2.0, 2.0, 0.0];
    let authored = control
        .iter()
        .map(|c| format!("{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let s = load(
        "vertex_ramp",
        &primvar("ramp"),
        &curves(
            "C",
            "cubic",
            "bspline",
            "7",
            &row(7, 0.0, 0.0),
            &format!(r#"float[] primvars:ramp = [{authored}] (interpolation = "vertex")"#),
        ),
    );
    let bspline = |s: usize, t: f32| {
        let b = [
            (1.0 - t).powi(3) / 6.0,
            (3.0 * t.powi(3) - 6.0 * t * t + 4.0) / 6.0,
            (-3.0 * t.powi(3) + 3.0 * t * t + 3.0 * t + 1.0) / 6.0,
            t.powi(3) / 6.0,
        ];
        (0..4).map(|k| b[k] * control[s + k]).sum::<f32>()
    };
    for x in [1.3f32, 2.5, 2.9, 3.6, 4.4] {
        // x = 1 + s + t on this straight spline.
        let (s_idx, t) = ((x - 1.0).floor() as usize, (x - 1.0).fract());
        let got = at(&s, x, 0.0).x;
        assert!(close(got, bspline(s_idx, t), 3e-3), "x {x}: {got}");
    }
}

#[test]
fn varying_interpolates_linearly_within_a_span() {
    // 7 B-spline points: 4 spans, 5 span ends.
    let s = load(
        "varying",
        &primvar("w"),
        &curves(
            "C",
            "cubic",
            "bspline",
            "7",
            &row(7, 0.0, 0.0),
            r#"float[] primvars:w = [0, 1, 3, 3, 7] (interpolation = "varying")"#,
        ),
    );
    // Span 1 runs 1 → 3: a quarter of the way along it, 1.5.
    assert!(close(at(&s, 2.25, 0.0).x, 1.5, 3e-3));
    assert!(close(at(&s, 4.5, 0.0).x, 5.0, 3e-3));
}

#[test]
fn a_float_primvar_into_a_colour_and_a_constant() {
    let s = load(
        "constant",
        &primvar("density"),
        &curves(
            "C",
            "linear",
            "bezier",
            "2",
            "(0, 0, 0), (1, 0, 0)",
            "float primvars:density = 0.4",
        ),
    );
    assert_eq!(at(&s, 0.5, 0.0), Vec3A::splat(0.4));
}

#[test]
fn indexed_primvars_are_expanded() {
    let points = [row(2, 0.0, 0.0), row(2, 0.0, 1.0)].join(", ");
    let s = load(
        "indexed",
        &primvar("tint"),
        &curves(
            "C",
            "linear",
            "bezier",
            "2, 2",
            &points,
            r#"color3f[] primvars:tint = [(1, 0, 0), (0, 0, 1)] (interpolation = "uniform")
        int[] primvars:tint:indices = [1, 0]"#,
        ),
    );
    assert_eq!(at(&s, 0.5, 0.0), Vec3A::Z);
    assert_eq!(at(&s, 0.5, 1.0), Vec3A::X);
}

fn warned(scene: &Scene, code: WarningCode) -> Vec<String> {
    scene
        .warnings
        .iter()
        .filter(|w| w.code == code)
        .map(|w| w.message.clone())
        .collect()
}

#[test]
fn an_unreadable_primvar_is_refused_and_reads_the_default() {
    for (name, authored) in [
        ("string", r#"string primvars:label = "x""#),
        (
            "matrix",
            "matrix4d primvars:label = ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1) )",
        ),
        (
            "facevarying",
            r#"float[] primvars:label = [1, 2] (interpolation = "faceVarying")"#,
        ),
        (
            "length",
            r#"float[] primvars:label = [1, 2, 3] (interpolation = "vertex")"#,
        ),
    ] {
        let s = load(
            &format!("refused_{name}"),
            &primvar("label"),
            &curves(
                "C",
                "linear",
                "bezier",
                "2",
                "(0, 0, 0), (1, 0, 0)",
                authored,
            ),
        );
        assert_eq!(at(&s, 0.5, 0.0), Vec3A::ZERO, "{name}");
        let w = warned(&s, WarningCode::CurvesUnsupportedPrimvar);
        assert_eq!(w.len(), 1, "{name}: {w:?}");
        assert!(w[0].contains("primvars:label"), "{name}: {w:?}");
    }
}

#[test]
fn an_authored_curve_namespace_primvar_is_refused() {
    let s = load(
        "reserved",
        &property("curve:v"),
        &curves(
            "C",
            "linear",
            "bezier",
            "2",
            "(0, 0, 0), (4, 0, 0)",
            r#"float[] primvars:curve:v = [0.9, 0.9] (interpolation = "vertex")"#,
        ),
    );
    assert!(close(at(&s, 1.0, 0.0).x, 0.25, 1e-3), "the strand's own v");
    let w = warned(&s, WarningCode::CurvesReservedPrimvar);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("primvars:curve:v"), "{w:?}");
}

#[test]
fn a_curve_s_st_feeds_its_chart() {
    // The chart through `texcoord`, a vector2 read as red and green.
    let graph = r#"
      <texcoord name="uv" type="vector2" />
      <convert name="value" type="color3"><input name="in" type="vector2" nodename="uv" /></convert>"#;
    let s = load(
        "chart",
        graph,
        &curves(
            "C",
            "linear",
            "bezier",
            "2",
            "(0, 0, 0), (4, 0, 0)",
            r#"texCoord2f[] primvars:st = [(0, 0.5), (1, 0.5)] (interpolation = "vertex")"#,
        ),
    );
    let c = at(&s, 1.0, 0.0);
    assert!(close(c.x, 0.25, 1e-3) && close(c.y, 0.5, 1e-6), "{c}");
    // No st: the chart's origin, and no warning.
    let bare = load(
        "chart_absent",
        graph,
        &curves("C", "linear", "bezier", "2", "(0, 0, 0), (4, 0, 0)", ""),
    );
    assert_eq!(at(&bare, 1.0, 0.0), Vec3A::ZERO);
    assert!(warned(&bare, WarningCode::CurvesUnsupportedPrimvar).is_empty());
}

#[test]
fn only_the_primvars_a_material_reads_are_decoded() {
    let one = r#"float[] primvars:p0 = [0.5, 0.5] (interpolation = "vertex")"#;
    let ten: String = (0..10)
        .map(|k| {
            format!(
                r#"float[] primvars:p{k} = [{k}, {k}] (interpolation = "vertex")
        "#
            )
        })
        .collect();
    let bytes = |name: &str, extra: &str| {
        let s = load(
            name,
            &primvar("p0"),
            &curves("C", "linear", "bezier", "2", "(0, 0, 0), (1, 0, 0)", extra),
        );
        s.stats.scene.strand_tables_bytes
    };
    let (a, b) = (bytes("decode_one", one), bytes("decode_ten", &ten));
    assert!(a > 0);
    assert_eq!(a, b, "the nine unread primvars were decoded");
}

#[test]
fn a_material_reading_nothing_builds_no_table() {
    // A constant emitter: no curve property, primvar or chart.
    let graph = r#"<constant name="value" type="color3"><input name="value" type="color3" value="1, 0, 0" /></constant>"#;
    let s = load(
        "no_table",
        graph,
        &curves(
            "C",
            "linear",
            "bezier",
            "2",
            "(0, 0, 0), (1, 0, 0)",
            r#"float[] primvars:p0 = [1, 1] (interpolation = "vertex")"#,
        ),
    );
    assert_eq!(s.stats.scene.strand_tables_bytes, 0);
    let ray = Ray::new(Vec3A::new(0.5, 0.0, 5.0), -Vec3A::Z);
    let hit = s.world.intersect(&ray, 1e-4, 1e4).expect("hits the strand");
    assert!(hit.rec.curve().is_none());
    assert_eq!(hit.mat.emitted_at(&ray, &hit.rec, 1.0), Vec3A::X);
}

#[test]
fn prototype_tables_are_counted_once() {
    // Two placements of one curves prototype share its table.
    let graph = property("curve:v");
    let clump = curves("Clump", "linear", "bezier", "2", "(0, 0, 0), (1, 0, 0)", "");
    let placed = |n: usize| {
        let positions = (0..n)
            .map(|i| format!("(0, {}, 0)", 2 * i))
            .collect::<Vec<_>>()
            .join(", ");
        let indices = vec!["0"; n].join(", ");
        format!(
            r#"
    def PointInstancer "Scatter"
    {{
        rel prototypes = [</World/Scatter/Protos/P>]
        int[] protoIndices = [{indices}]
        point3f[] positions = [{positions}]
        def Scope "Protos"
        {{
            def Xform "P" {{ {clump} }}
        }}
    }}"#
        )
    };
    let one = load("proto_one", &graph, &placed(1))
        .stats
        .scene
        .strand_tables_bytes;
    let five = load("proto_five", &graph, &placed(5))
        .stats
        .scene
        .strand_tables_bytes;
    assert!(one > 0);
    assert_eq!(one, five);
}

#[test]
fn curve_random_is_stable_across_frames() {
    // An animated groom of constant topology: its points move between
    // frames 1 and 2, its curve order does not.
    let body = r#"
    def BasisCurves "C" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        uniform token type = "linear"
        int[] curveVertexCounts = [2, 2, 2]
        point3f[] points.timeSamples = {
            1: [(0, 0, 0), (1, 0, 0), (0, 1, 0), (1, 1, 0), (0, 2, 0), (1, 2, 0)],
            2: [(0, 0.2, 0), (1, 0.2, 0), (0, 1.2, 0), (1, 1.2, 0), (0, 2.2, 0), (1, 2.2, 0)],
        }
        float[] widths = [0.2] (interpolation = "constant")
        rel material:binding = </World/Looks/M>
    }"#;
    load("frames", &property("curve:random"), body);
    let stage = dir("frames").join("stage.usda");
    let at_frame = |frame: f64| {
        let options = crust_core::UsdImportOptions {
            frame: Some(frame),
            ..Default::default()
        };
        Scene::from_usd_with_options(&stage, &crust_core::NoAssets, &options).expect("loads")
    };
    let (f1, f2) = (at_frame(1.0), at_frame(2.0));
    for k in 0..3 {
        let y = k as f32;
        assert_eq!(at(&f1, 0.5, y).x, at(&f2, 0.5, y + 0.2).x, "curve {k}");
    }
    assert_ne!(at(&f1, 0.5, 0.0).x, at(&f1, 0.5, 1.0).x);
}

/// `samples/hair_variation.usda`: one groom whose hair material reads its
/// root colour primvar, `curve:v` and `curve:random`.
#[test]
fn loads_hair_variation_usda() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/hair_variation.usda");
    let s = Scene::from_usd(&path).expect("loads");
    // This harness has no asset loader for the dome's map; `crust check`
    // (with one) reports nothing at all.
    let others: Vec<_> = s
        .warnings
        .iter()
        .filter(|w| w.code != WarningCode::AssetUnsupportedByHost)
        .collect();
    assert!(others.is_empty(), "{others:?}");
    assert!(
        s.stats.scene.strand_tables_bytes > 0,
        "the groom carries its table"
    );
    // Across the groom at three heights: the strands' mean v rises with
    // height, root to tip.
    let mut last = -1.0;
    for y in [0.2f32, 0.8, 1.4] {
        let vs: Vec<f32> = (0..600)
            .filter_map(|i| {
                let x = -3.0 + i as f32 * 0.01;
                let ray = Ray::new(Vec3A::new(x, y, 5.0), -Vec3A::Z);
                let hit = s.world.intersect(&ray, 1e-4, 1e4)?;
                hit.rec.curve().map(|c| c.v())
            })
            .collect();
        assert!(vs.len() > 20, "{} strands hit at height {y}", vs.len());
        let mean = vs.iter().sum::<f32>() / vs.len() as f32;
        assert!(mean > last, "mean v {mean} at height {y}");
        last = mean;
    }
}

#[test]
fn a_stable_id_seeds_variation_that_survives_reordering() {
    // The same three strands, authored in two orders, each with its own
    // `strandId`; the variation reads the id, not the curve's position.
    let graph = r#"
      <geompropvalue name="id" type="integer"><input name="geomprop" type="string" value="strandId" /></geompropvalue>
      <randomfloat name="r" type="float"><input name="seed" type="integer" nodename="id" /></randomfloat>
      <convert name="value" type="color3"><input name="in" type="float" nodename="r" /></convert>"#;
    let strands = |order: [usize; 3]| {
        let points: Vec<String> = order.iter().map(|&k| row(2, 0.0, k as f32)).collect();
        let ids: Vec<String> = order.iter().map(|&k| format!("{}", 100 + k)).collect();
        curves(
            "C",
            "linear",
            "bezier",
            "2, 2, 2",
            &points.join(", "),
            &format!(
                r#"int[] primvars:strandId = [{}] (interpolation = "uniform")"#,
                ids.join(", ")
            ),
        )
    };
    let a = load("stable_id_a", graph, &strands([0, 1, 2]));
    let b = load("stable_id_b", graph, &strands([2, 0, 1]));
    for y in [0.0f32, 1.0, 2.0] {
        assert_eq!(at(&a, 0.5, y).x, at(&b, 0.5, y).x, "strand at y {y}");
    }
    assert_ne!(at(&a, 0.5, 0.0).x, at(&a, 0.5, 1.0).x);
    // While `curve:random` follows the order.
    let ra = load(
        "stable_id_ra",
        &property("curve:random"),
        &strands([0, 1, 2]),
    );
    let rb = load(
        "stable_id_rb",
        &property("curve:random"),
        &strands([2, 0, 1]),
    );
    assert_ne!(at(&ra, 0.5, 0.0).x, at(&rb, 0.5, 0.0).x);
}
