//! Mesh primvars reaching materials: MaterialX `geompropvalue` of a primvar
//! on a mesh or an implicit shape, end to end from USD — every
//! interpolation, indexed values, refinement, placements, and the opacity a
//! primvar drives.
//!
//! Each stage binds a pure emitter whose colour is the graph under test, so a
//! hit's `emitted_at` reads back exactly what the material computed there.

use crust_core::{Ray, Scene, Vec3A, WarningCode};
use std::path::PathBuf;

/// A directory of its own per test, for the stage and its `.mtlx`.
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir()
        .join("crust_mesh_primvar_tests")
        .join(name);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// A stage of `body` under `/World`, with `/World/Looks/M` a pure emitter
/// whose colour is the `color3` node `value` of `graph`, and whose opacity is
/// the `float` node `opacity` when `graph` has one.
fn load(name: &str, graph: &str, body: &str) -> Scene {
    let d = dir(name);
    let mtlx = d.join("m.mtlx");
    let opacity = if graph.contains(r#"name="opacity""#) {
        r#"<input name="opacity" type="float" nodename="opacity" />"#
    } else {
        ""
    };
    std::fs::write(
        &mtlx,
        format!(
            r#"<?xml version="1.0"?>
<materialx version="1.39">
  {graph}
  <uniform_edf name="edf" type="EDF"><input name="color" type="color3" nodename="value" /></uniform_edf>
  <surface name="surf" type="surfaceshader">
    <input name="edf" type="EDF" nodename="edf" />
    {opacity}
  </surface>
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

/// A `color3` `geompropvalue` of `prim` as the emitted colour, defaulting to
/// mid-grey.
fn reading(prim: &str) -> String {
    format!(
        r#"<geompropvalue name="value" type="color3">
             <input name="geomprop" type="string" value="{prim}" />
             <input name="default" type="color3" value="0.5, 0.5, 0.5" />
           </geompropvalue>"#
    )
}

const GREY: Vec3A = Vec3A::splat(0.5);

/// What the material emits where a ray straight down the -z axis at `(x, y)`
/// first hits.
fn at(scene: &Scene, x: f32, y: f32) -> Vec3A {
    let ray = Ray::new(Vec3A::new(x, y, 5.0), -Vec3A::Z);
    let hit = scene
        .world
        .intersect(&ray, 1e-4, 1e4)
        .unwrap_or_else(|| panic!("no hit at ({x}, {y})"));
    hit.mat.emitted_at(&ray, &hit.rec, 1.0)
}

/// The material's opacity where a ray straight down at `(x, y)` first hits.
fn opacity_at(scene: &Scene, x: f32, y: f32) -> f32 {
    let ray = Ray::new(Vec3A::new(x, y, 5.0), -Vec3A::Z);
    let hit = scene.world.intersect(&ray, 1e-4, 1e4).expect("a hit");
    hit.mat.opacity(&ray, &hit.rec)
}

fn close(a: Vec3A, b: Vec3A, tol: f32) -> bool {
    (a - b).abs().max_element() <= tol
}

/// The warnings about primvars (the stages here have no camera, which is
/// warned about too).
fn primvar_warnings(scene: &Scene) -> Vec<String> {
    let mut w = warned(scene, WarningCode::MeshUnsupportedPrimvar);
    w.extend(warned(scene, WarningCode::PrimvarMismatch));
    w
}

fn warned(scene: &Scene, code: WarningCode) -> Vec<String> {
    scene
        .warnings
        .iter()
        .filter(|w| w.code == code)
        .map(|w| w.message.clone())
        .collect()
}

/// A mesh prim `name` bound to `M`: two unit quads side by side in the
/// z = 0 plane, x in [0, 2], as a faceted (`none`) mesh unless `extra`
/// authors a scheme, with `extra` authored on it.
fn quads(name: &str, extra: &str) -> String {
    let scheme = if extra.contains("subdivisionScheme") {
        ""
    } else {
        r#"uniform token subdivisionScheme = "none""#
    };
    format!(
        r#"
    def Mesh "{name}" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        int[] faceVertexCounts = [4, 4]
        int[] faceVertexIndices = [0, 1, 4, 3, 1, 2, 5, 4]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (2, 0, 0), (0, 1, 0), (1, 1, 0), (2, 1, 0)]
        {scheme}
        rel material:binding = </World/Looks/M>
        {extra}
    }}"#
    )
}

/// `x + 2y` at every point of [`quads`]: a linear function, which every
/// triangulation interpolates exactly.
const RAMP: &str = "float[] primvars:ramp = [0, 1, 2, 2, 3, 4] (interpolation = \"vertex\")";

#[test]
fn a_vertex_primvar_is_interpolated_across_the_mesh() {
    let s = load("vertex", &reading("ramp"), &quads("Q", RAMP));
    for (x, y) in [(0.25, 0.25), (0.8, 0.1), (1.5, 0.5), (1.9, 0.9), (0.1, 0.7)] {
        let want = Vec3A::splat(x + 2.0 * y);
        assert!(
            close(at(&s, x, y), want, 1e-4),
            "({x}, {y}): {}",
            at(&s, x, y)
        );
    }
    assert!(primvar_warnings(&s).is_empty(), "{:?}", s.warnings);
}

#[test]
fn varying_reads_as_vertex() {
    let s = load(
        "varying",
        &reading("ramp"),
        &quads("Q", &RAMP.replace("vertex", "varying")),
    );
    assert!(close(at(&s, 1.5, 0.25), Vec3A::splat(2.0), 1e-4));
}

#[test]
fn uniform_constant_and_face_varying_primvars() {
    let s = load(
        "interpolations",
        r#"<geompropvalue name="face" type="float"><input name="geomprop" type="string" value="face" /></geompropvalue>
           <geompropvalue name="all" type="float"><input name="geomprop" type="string" value="all" /></geompropvalue>
           <geompropvalue name="corner" type="float"><input name="geomprop" type="string" value="corner" /></geompropvalue>
           <combine3 name="value" type="color3">
             <input name="in1" type="float" nodename="face" />
             <input name="in2" type="float" nodename="all" />
             <input name="in3" type="float" nodename="corner" />
           </combine3>"#,
        &quads(
            "Q",
            r#"float[] primvars:face = [0.25, 0.75] (interpolation = "uniform")
        float primvars:all = 0.5 (interpolation = "constant")
        float[] primvars:corner = [0, 0, 0, 0, 1, 1, 1, 1] (interpolation = "faceVarying")"#,
        ),
    );
    // Each face its own value, and the face-varying one discontinuous across
    // the shared edge, which a vertex primvar could not be.
    assert!(close(at(&s, 0.5, 0.5), Vec3A::new(0.25, 0.5, 0.0), 1e-5));
    assert!(close(at(&s, 1.5, 0.5), Vec3A::new(0.75, 0.5, 1.0), 1e-5));
    assert!(close(at(&s, 0.99, 0.5), Vec3A::new(0.25, 0.5, 0.0), 1e-5));
    assert!(close(at(&s, 1.01, 0.5), Vec3A::new(0.75, 0.5, 1.0), 1e-5));
}

#[test]
fn indexed_primvars_are_expanded() {
    let s = load(
        "indexed",
        &reading("tint"),
        &quads(
            "Q",
            r#"color3f[] primvars:tint = [(1, 0, 0), (0, 0, 1)] (interpolation = "uniform")
        int[] primvars:tint:indices = [1, 0]"#,
        ),
    );
    assert_eq!(at(&s, 0.5, 0.5), Vec3A::Z);
    assert_eq!(at(&s, 1.5, 0.5), Vec3A::X);
}

#[test]
fn display_color_is_read_like_any_primvar() {
    let s = load(
        "display_color",
        &reading("displayColor"),
        &quads(
            "Q",
            r#"color3f[] primvars:displayColor = [(0.2, 0.4, 0.6)] (interpolation = "constant")"#,
        ),
    );
    assert!(close(at(&s, 1.0, 0.5), Vec3A::new(0.2, 0.4, 0.6), 1e-6));
}

#[test]
fn a_colour_primvar_is_converted_into_the_working_space() {
    // sRGB-encoded mid-grey is linear 0.214.
    let s = load(
        "colour_space",
        &reading("tint"),
        &quads(
            "Q",
            r#"color3f[] primvars:tint = [(0.5, 0.5, 0.5)] (
            colorSpace = "srgb_texture"
            interpolation = "constant"
        )"#,
        ),
    );
    assert!(
        close(at(&s, 0.5, 0.5), Vec3A::splat(0.214), 1e-3),
        "{}",
        at(&s, 0.5, 0.5)
    );
}

#[test]
fn a_float2_primvar_converts_into_a_colour() {
    let s = load(
        "float2",
        &reading("pair"),
        &quads(
            "Q",
            r#"float2[] primvars:pair = [(0.25, 0.5)] (interpolation = "constant")"#,
        ),
    );
    // A `vector2` into a `color3` keeps its two lanes; the third is zero.
    assert!(close(at(&s, 0.5, 0.5), Vec3A::new(0.25, 0.5, 0.0), 1e-6));
}

#[test]
fn an_unauthored_primvar_reads_the_default_silently() {
    let s = load("unauthored", &reading("missing"), &quads("Q", RAMP));
    assert_eq!(at(&s, 0.5, 0.5), GREY);
    assert!(primvar_warnings(&s).is_empty(), "{:?}", s.warnings);
}

#[test]
fn an_unreadable_primvar_reads_the_default_and_warns() {
    for (name, extra) in [
        (
            "short",
            r#"float[] primvars:label = [1, 2] (interpolation = "vertex")"#,
        ),
        ("string", r#"string primvars:label = "a""#),
        (
            "indices",
            r#"float[] primvars:label = [1] (interpolation = "uniform")
        int[] primvars:label:indices = [0, 3]"#,
        ),
        // Values enough to read unindexed: refused, not read as if the
        // indices were absent.
        (
            "indices_type",
            r#"float[] primvars:label = [1, 1] (interpolation = "uniform")
        int64[] primvars:label:indices = [0, 1]"#,
        ),
        // As on curves: an empty array is told, not read as unauthored.
        (
            "empty",
            r#"float[] primvars:label = [] (interpolation = "vertex")"#,
        ),
    ] {
        let s = load(
            &format!("unreadable_{name}"),
            &reading("label"),
            &quads("Q", extra),
        );
        assert_eq!(at(&s, 0.5, 0.5), GREY, "{name}");
        let w = warned(&s, WarningCode::MeshUnsupportedPrimvar);
        assert_eq!(w.len(), 1, "{name}: {w:?}");
        assert!(w[0].contains("primvars:label"), "{name}: {w:?}");
    }
}

#[test]
fn a_subdivided_mesh_refines_its_primvars() {
    let s = load(
        "subdivided",
        r#"<geompropvalue name="r" type="float"><input name="geomprop" type="string" value="flat" /></geompropvalue>
           <geompropvalue name="g" type="float"><input name="geomprop" type="string" value="face" /></geompropvalue>
           <combine3 name="value" type="color3">
             <input name="in1" type="float" nodename="r" />
             <input name="in2" type="float" nodename="g" />
           </combine3>"#,
        &quads(
            "Q",
            r#"uniform token subdivisionScheme = "catmullClark"
        float[] primvars:flat = [0.3, 0.3, 0.3, 0.3, 0.3, 0.3] (interpolation = "vertex")
        float[] primvars:face = [0.25, 0.75] (interpolation = "uniform")"#,
        ),
    );
    // A constant field refines to itself; each refined face keeps its cage
    // face's uniform value. Sampled near the middle of each cage face, which
    // the limit surface still covers.
    assert!(close(at(&s, 0.6, 0.5), Vec3A::new(0.3, 0.25, 0.0), 1e-4));
    assert!(close(at(&s, 1.4, 0.5), Vec3A::new(0.3, 0.75, 0.0), 1e-4));
}

#[test]
fn every_placement_reads_the_primvars_of_its_mesh() {
    // A mirrored placement (baked, its winding swapped) and a mesh placed
    // twice (instanced) both read the ramp in the mesh's own frame.
    let mesh = |name: &str, xf: &str| {
        quads(name, RAMP).replace(
            "rel material:binding",
            &format!("matrix4d xformOp:transform = {xf}\n        uniform token[] xformOpOrder = [\"xformOp:transform\"]\n        rel material:binding"),
        )
    };
    let mirrored = mesh(
        "Mirrored",
        "( (-1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (10, 0, 0, 1) )",
    );
    let body = format!(
        "{mirrored}{}{}",
        mesh(
            "A",
            "( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 5, 0, 1) )"
        ),
        mesh(
            "B",
            "( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 10, 0, 1) )"
        ),
    );
    let s = load("placements", &reading("ramp"), &body);
    // Mirrored about x = 10: world x = 10 − local x.
    for (lx, ly) in [(0.25, 0.25), (0.8, 0.1), (1.5, 0.5), (1.9, 0.9)] {
        let want = Vec3A::splat(lx + 2.0 * ly);
        assert!(
            close(at(&s, 10.0 - lx, ly), want, 1e-4),
            "mirrored ({lx}, {ly})"
        );
        assert!(close(at(&s, lx, 5.0 + ly), want, 1e-4), "A ({lx}, {ly})");
        assert!(close(at(&s, lx, 10.0 + ly), want, 1e-4), "B ({lx}, {ly})");
    }
}

#[test]
fn a_primvar_drives_a_cutout() {
    // The issue's FX mesh: a per-point alpha fading the pieces.
    let s = load(
        "cutout",
        r#"<constant name="value" type="color3"><input name="value" type="color3" value="1, 1, 1" /></constant>
           <geompropvalue name="opacity" type="float">
             <input name="geomprop" type="string" value="Alpha" />
             <input name="default" type="float" value="1" />
           </geompropvalue>"#,
        &quads(
            "Q",
            r#"float[] primvars:Alpha = [0.1, 0.1, 0.7, 0.1, 0.1, 0.7] (interpolation = "vertex")"#,
        ),
    );
    assert!((opacity_at(&s, 0.5, 0.5) - 0.1).abs() < 1e-5);
    assert!((opacity_at(&s, 1.5, 0.5) - 0.4).abs() < 1e-4);
    assert!((opacity_at(&s, 2.0 - 1e-3, 0.5) - 0.7).abs() < 1e-3);
}

#[test]
fn implicit_shapes_read_constant_primvars() {
    let shape = |kind: &str, name: &str, at_x: f32, extra: &str| {
        format!(
            r#"
    def {kind} "{name}" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        double3 xformOp:translate = ({at_x}, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        rel material:binding = </World/Looks/M>
        color3f[] primvars:tint = [(0.1, 0.2, 0.3)] (interpolation = "constant")
        {extra}
    }}"#
        )
    };
    let body = format!(
        "{}{}{}",
        shape("Sphere", "Ball", 0.0, ""),
        shape("Cube", "Box", 5.0, ""),
        shape(
            "Cylinder",
            "Can",
            10.0,
            r#"float[] primvars:other = [1, 2] (interpolation = "uniform")"#
        ),
    );
    let s = load(
        "implicit",
        r#"<geompropvalue name="tint" type="color3"><input name="geomprop" type="string" value="tint" /></geompropvalue>
           <geompropvalue name="other" type="float"><input name="geomprop" type="string" value="other" /></geompropvalue>
           <add name="value" type="color3">
             <input name="in1" type="color3" nodename="tint" />
             <input name="in2" type="float" nodename="other" />
           </add>"#,
        &body,
    );
    let tint = Vec3A::new(0.1, 0.2, 0.3);
    assert!(close(at(&s, 0.0, 0.0), tint, 1e-6), "the analytic sphere");
    assert!(close(at(&s, 5.0, 0.0), tint, 1e-6), "the tessellated cube");
    // A uniform primvar on an implicit shape is refused: it reads its
    // default, zero, and the tint alone remains.
    assert!(close(at(&s, 10.0, 0.0), tint, 1e-6), "the cylinder");
    let w = warned(&s, WarningCode::MeshUnsupportedPrimvar);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("primvars:other"), "{w:?}");
}

#[test]
fn a_mesh_whose_material_reads_no_primvar_carries_no_table() {
    let s = load(
        "no_table",
        r#"<constant name="value" type="color3"><input name="value" type="color3" value="1, 0, 0" /></constant>"#,
        &quads("Q", RAMP),
    );
    let ray = Ray::new(Vec3A::new(0.5, 0.5, 5.0), -Vec3A::Z);
    let hit = s.world.intersect(&ray, 1e-4, 1e4).expect("a hit");
    assert!(hit.rec.mesh().is_none());
    assert_eq!(s.world.primvar_table_bytes(), 0);
}
