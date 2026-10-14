//! `CRUST_NEURAL_OCCLUSION` end to end (the `neural-bvh-experiment` change's
//! Phase 2): which prototypes get a learned occlusion proxy, what the log and
//! `--stats` say about them, and that camera rays stay exact.
//!
//! Each proxy trains for its full 2 000 steps, which takes seconds in a
//! release build and minutes in a debug one, so these are `#[ignore]`d and run
//! explicitly:
//!
//! ```sh
//! cargo test --release -p crust-render --features neural-bvh --test neural_occlusion -- --ignored
//! ```
#![cfg(feature = "neural-bvh")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A `n × n` quad grid on the unit square, bumped so it is not flat:
/// `2 n²` triangles.
fn grid(n: usize) -> (String, String, String) {
    let mut points = Vec::new();
    for j in 0..=n {
        for i in 0..=n {
            let (x, z) = (i as f32 / n as f32 - 0.5, j as f32 / n as f32 - 0.5);
            let y = 0.15 * ((7.0 * x).sin() * (5.0 * z).cos());
            points.push(format!("({x}, {y}, {z})"));
        }
    }
    let row = n + 1;
    let mut indices = Vec::new();
    for j in 0..n {
        for i in 0..n {
            let a = j * row + i;
            indices.extend([a, a + 1, a + row + 1, a + row].map(|k| k.to_string()));
        }
    }
    let counts = vec!["4"; n * n].join(", ");
    (points.join(", "), indices.join(", "), counts)
}

/// Two prototypes of the same 3 200-triangle grid, each placed twice: one
/// opaque, one a thin-walled pane with straight transmission (a
/// pass-through). A light above, a floor below, and a data product of the
/// first-hit depth and normal beside the beauty.
fn stage(dir: &Path) -> PathBuf {
    let (points, indices, counts) = grid(40);
    let proto = |name: &str, look: &str| {
        format!(
            r#"
    class Xform "{name}"
    {{
        def Mesh "surface" (prepend apiSchemas = ["MaterialBindingAPI"])
        {{
            uniform token subdivisionScheme = "none"
            rel material:binding = </World/Looks/{look}>
            int[] faceVertexCounts = [{counts}]
            int[] faceVertexIndices = [{indices}]
            point3f[] points = [{points}]
        }}
    }}"#
        )
    };
    let place = |name: &str, proto: &str, x: f32| {
        format!(
            r#"
    def Xform "{name}" (instanceable = true; references = </World/{proto}>)
    {{
        double3 xformOp:translate = ({x}, 1, 0)
        float3 xformOp:scale = (2, 2, 2)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:scale"]
    }}"#
        )
    };
    let text = format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
    def Scope "Looks"
    {{
        def Material "Matte"
        {{
            token outputs:surface.connect = </World/Looks/Matte/Shader.outputs:surface>
            def Shader "Shader"
            {{
                uniform token info:id = "crust:openpbr"
                color3f inputs:baseColor = (0.6, 0.6, 0.6)
                token outputs:surface
            }}
        }}
        def Material "Pane"
        {{
            token outputs:surface.connect = </World/Looks/Pane/Shader.outputs:surface>
            def Shader "Shader"
            {{
                uniform token info:id = "crust:openpbr"
                float inputs:transmissionWeight = 1
                float inputs:specularRoughness = 0
                bool inputs:geometryThinWalled = 1
                token outputs:surface
            }}
        }}
    }}
{opaque}
{glass}
{a}
{b}
{c}
{d}
    def Mesh "Floor" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token subdivisionScheme = "none"
        rel material:binding = </World/Looks/Matte>
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-20, 0, -20), (20, 0, -20), (20, 0, 20), (-20, 0, 20)]
    }}

    def SphereLight "Key"
    {{
        float inputs:intensity = 400
        float inputs:radius = 0.3
        double3 xformOp:translate = (0, 4, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}

    def Camera "Cam"
    {{
        float focalLength = 18
        double3 xformOp:translate = (0, 3, 9)
        float xformOp:rotateX = -18
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateX"]
    }}
}}

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/Cam>
        int2 resolution = (64, 40)
        rel products = [</Render/Products/beauty>, </Render/Products/data>]
    }}
    def Scope "Products"
    {{
        def RenderProduct "beauty"
        {{
            token productName = "beauty.exr"
            rel orderedVars = [</Render/Products/Vars/C>]
        }}
        def RenderProduct "data"
        {{
            token productName = "data.exr"
            rel orderedVars = [</Render/Products/Vars/Z>, </Render/Products/Vars/N>]
        }}
        def Scope "Vars"
        {{
            def RenderVar "C"
            {{
                uniform token dataType = "color4f"
                uniform string sourceName = "Ci"
            }}
            def RenderVar "Z"
            {{
                uniform token dataType = "float"
                uniform string sourceName = "depth"
            }}
            def RenderVar "N"
            {{
                uniform token dataType = "normal3f"
                uniform string sourceName = "normal"
            }}
        }}
    }}
}}
"#,
        opaque = proto("_Opaque", "Matte"),
        glass = proto("_Glass", "Pane"),
        a = place("OpaqueA", "_Opaque", -3.0),
        b = place("OpaqueB", "_Opaque", -1.0),
        c = place("GlassA", "_Glass", 1.0),
        d = place("GlassB", "_Glass", 3.0),
    );
    let path = dir.join("scene.usda");
    std::fs::write(&path, text).expect("write the stage");
    path
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("crust_neural_occlusion")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// `crust render --stats` of the stage in `dir`, with the switch set to
/// `switch` (unset for `None`).
fn render(dir: &Path, scene: &Path, switch: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_crust"));
    cmd.current_dir(dir)
        .args(["render", "-i"])
        .arg(scene)
        .args(["-s", "4", "--stats", "-l", "info"])
        .env("CRUST_NEURAL_MIN_TRIS", "1000")
        .env_remove("CRUST_NEURAL_OCCLUSION");
    if let Some(v) = switch {
        cmd.env("CRUST_NEURAL_OCCLUSION", v);
    }
    let out = cmd.output().expect("run crust");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
#[ignore = "trains a proxy: run in release with --ignored"]
fn only_the_opaque_prototype_gets_a_proxy_and_camera_rays_stay_exact() {
    let on_dir = work_dir("on");
    let off_dir = work_dir("off");
    let scene = stage(&on_dir);

    let on = text(&render(&on_dir, &scene, Some("on")));
    let off = text(&render(&off_dir, &scene, None));

    // One INFO line says the render is approximate, and how many proxies:
    // the opaque prototype's, never the pass-through pane's.
    let info: Vec<&str> = on
        .lines()
        .filter(|l| l.contains("Neural occlusion:"))
        .collect();
    assert_eq!(info.len(), 1, "{on}");
    assert!(info[0].contains(" 1 prototype prox"), "{}", info[0]);
    assert!(info[0].contains("approximate"), "{}", info[0]);

    // The stats report: the proxy count with its training time, and its
    // bytes as a kernel-memory row.
    let rows: Vec<&str> = on
        .lines()
        .filter(|l| l.contains("occlusion proxies"))
        .collect();
    assert_eq!(rows.len(), 2, "a count row and a memory row: {on}");
    assert!(rows.iter().any(|r| r.contains("trained in")), "{rows:?}");
    assert!(rows.iter().any(|r| r.contains("MiB")), "{rows:?}");

    // Off: no line and no row.
    assert!(!off.contains("Neural occlusion:"), "{off}");
    assert!(!off.contains("occlusion proxies"), "{off}");

    // The render's shadow rays really do ask the proxy: the beauty moves.
    // (Nothing above would notice a proxy that is built and never asked —
    // a scene whose prototype is in some other occluder's shadow, say.)
    let beauty = Command::new(env!("CARGO_BIN_EXE_crust"))
        .arg("diff")
        .arg(on_dir.join("beauty.exr"))
        .arg(off_dir.join("beauty.exr"))
        .output()
        .expect("run crust diff");
    assert_eq!(
        beauty.status.code(),
        Some(1),
        "the proxy changed no pixel: {}",
        text(&beauty)
    );

    // Camera rays never meet the proxy: the first-hit data is bit-identical.
    let diff = Command::new(env!("CARGO_BIN_EXE_crust"))
        .arg("diff")
        .arg(on_dir.join("data.exr"))
        .arg(off_dir.join("data.exr"))
        .output()
        .expect("run crust diff");
    assert_eq!(
        diff.status.code(),
        Some(0),
        "first-hit depth and normal differ: {}",
        text(&diff)
    );
}

#[test]
#[ignore = "renders twice: run in release with --ignored"]
fn off_is_the_exact_renderer() {
    let unset = work_dir("unset");
    let off = work_dir("explicit_off");
    let scene = stage(&unset);
    render(&unset, &scene, None);
    render(&off, &scene, Some("off"));
    for file in ["beauty.exr", "data.exr"] {
        let diff = Command::new(env!("CARGO_BIN_EXE_crust"))
            .arg("diff")
            .arg(unset.join(file))
            .arg(off.join(file))
            .output()
            .expect("run crust diff");
        assert_eq!(diff.status.code(), Some(0), "{file}: {}", text(&diff));
    }
}
