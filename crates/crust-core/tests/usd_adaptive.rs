//! Adaptive subdivision: each subdivision mesh refined per placement, as far as
//! its size on screen asks (`crust:subdivisionEdgeLength`,
//! `UsdImportOptions::subdivision_edge_length`).
//!
//! Every stage here renders through a camera at the origin looking down −Z at
//! 640×360 with USD's default lens (focal length 50, horizontal aperture
//! 20.955), so one world unit at distance `d` covers `1527 / d` pixels. The
//! cube cage has edges of length 2, so at a 2 px target its level is
//! `ceil(log2(1527 / d))`: the maximum (3) anywhere near the camera, 0 beyond
//! about 1527 units.

use crust_core::{Material, NoAssets, Ray, Scene, UsdImportOptions, Vec3A};
use std::path::{Path, PathBuf};

/// The 2×2×2 cube cage, as a Catmull-Clark mesh, in the body of a `Mesh`.
const CUBE: &str = r#"
        uniform token subdivisionScheme = "catmullClark"
        int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]
        int[] faceVertexIndices = [0, 1, 3, 2, 2, 3, 5, 4, 4, 5, 7, 6, 6, 7, 1, 0, 1, 7, 5, 3, 6, 0, 2, 4]
        point3f[] points = [(-1, -1, 1), (1, -1, 1), (-1, 1, 1), (1, 1, 1), (-1, 1, -1), (1, 1, -1), (-1, -1, -1), (1, -1, -1)]
"#;

/// Triangles of the cube at each level: 12 · 4^L.
fn cube_triangles(level: u32) -> usize {
    12 << (2 * level)
}

const CAMERA: &str = r#"
    def Camera "Cam" {}
"#;

/// A `RenderSettings` prim at 640×360 through `/W/Cam`, authoring `extra`.
fn settings(extra: &str) -> String {
    format!(
        r#"def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </W/Cam>
        uniform int2 resolution = (640, 360)
        {extra}
    }}
}}"#
    )
}

/// Two looks, so a prototype of two parts can show each part keeps its own.
const LOOKS: &str = r#"
    def Scope "Looks" {
        def Material "Red" {
            token outputs:surface.connect = </W/Looks/Red/S.outputs:surface>
            def Shader "S" {
                uniform token info:id = "crust:openpbr"
                color3f inputs:baseColor = (0.8, 0.1, 0.1)
                token outputs:surface
            }
        }
        def Material "Blue" {
            token outputs:surface.connect = </W/Looks/Blue/S.outputs:surface>
            def Shader "S" {
                uniform token info:id = "crust:openpbr"
                color3f inputs:baseColor = (0.1, 0.1, 0.8)
                token outputs:surface
            }
        }
    }
"#;

fn write_stage(name: &str, body: &str, settings: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_usd_adaptive_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    let text = format!(
        "#usda 1.0\n(\n    defaultPrim = \"W\"\n    upAxis = \"Y\"\n)\n\ndef Xform \"W\"\n{{\n{body}\n}}\n{settings}\n"
    );
    std::fs::write(&path, text).expect("write stage");
    path
}

fn load(path: &Path, options: &UsdImportOptions) -> Scene {
    Scene::from_usd_with_options(path, &NoAssets, options)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A cube mesh prim named `name` translated to `at`, authoring `extra`. A
/// `material:binding` in `extra` gets the `MaterialBindingAPI` it needs.
fn cube(name: &str, at: (f32, f32, f32), extra: &str) -> String {
    let api = if extra.contains("material:binding") {
        r#" (prepend apiSchemas = ["MaterialBindingAPI"])"#
    } else {
        ""
    };
    format!(
        r#"
    def Mesh "{name}"{api}
    {{
{CUBE}
        {extra}
        double3 xformOp:translate = ({}, {}, {})
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}"#,
        at.0, at.1, at.2
    )
}

fn unique_triangles(scene: &Scene) -> usize {
    let u = scene.stats.scene.unique;
    if u.total() == 0 {
        scene.stats.scene.top_level.triangles
    } else {
        u.triangles
    }
}

/// The distance to the first hit along a ray from the camera toward `p`.
fn hit_t(scene: &Scene, p: (f32, f32, f32)) -> Option<f32> {
    let dir = Vec3A::new(p.0, p.1, p.2).normalize();
    scene
        .world
        .intersect(&Ray::new(Vec3A::ZERO, dir), 1e-3, 1e5)
        .map(|h| h.rec.t)
}

fn material_at(scene: &Scene, p: (f32, f32, f32)) -> *const () {
    let dir = Vec3A::new(p.0, p.1, p.2).normalize();
    let hit = scene
        .world
        .intersect(&Ray::new(Vec3A::ZERO, dir), 1e-3, 1e5)
        .unwrap_or_else(|| panic!("nothing toward {p:?}"));
    hit.mat as *const dyn Material as *const ()
}

/// Two copies of one cage, one filling the frame and one far enough to need
/// no refinement: the near one is refined, the far one keeps its smooth cage —
/// and two far copies at the same distance still share one mesh.
#[test]
fn near_and_far_copies_of_one_cage() {
    let body = format!(
        "{CAMERA}{}{}{}",
        cube("Near", (0.0, 0.0, -6.0), ""),
        cube("FarA", (-20.0, 0.0, -3000.0), ""),
        cube("FarB", (20.0, 0.0, -3000.0), ""),
    );
    let path = write_stage(
        "near_far",
        &body,
        &settings("float crust:subdivisionEdgeLength = 2"),
    );
    let scene = load(&path, &UsdImportOptions::default());
    let sub = &scene.stats.subdivision;
    assert_eq!(
        sub.adaptive,
        Some((2.0, 3)),
        "adaptive, at most level 3 by default"
    );
    assert_eq!(
        sub.levels,
        vec![2, 0, 0, 1],
        "two far reads at level 0, the near one at the maximum"
    );
    assert_eq!(
        unique_triangles(&scene),
        cube_triangles(3) + cube_triangles(0),
        "the far copies share one level-0 mesh"
    );
    assert!(hit_t(&scene, (0.0, 0.0, -6.0)).is_some());
    assert!(hit_t(&scene, (-20.0, 0.0, -3000.0)).is_some());
}

/// The host's edge length overrides the stage's, and `--subdiv-level` caps it.
#[test]
fn the_level_setting_caps_adaptive_refinement() {
    let body = format!("{CAMERA}{}", cube("Near", (0.0, 0.0, -6.0), ""));
    let path = write_stage("capped", &body, &settings(""));
    let options = UsdImportOptions {
        subdivision_edge_length: Some(2.0),
        subdivision_level: Some(2),
        ..UsdImportOptions::default()
    };
    let scene = load(&path, &options);
    assert_eq!(scene.stats.subdivision.adaptive, Some((2.0, 2)));
    assert_eq!(unique_triangles(&scene), cube_triangles(2));
}

/// Without a camera named before the traversal, adaptive mode warns and the
/// uniform level applies.
#[test]
fn no_camera_known_before_traversal_falls_back_to_the_uniform_level() {
    let body = format!("{CAMERA}{}", cube("Near", (0.0, 0.0, -6.0), ""));
    // No `rel camera`: the importer would only find the camera by meeting it.
    let no_camera = r#"def Scope "Render"
{
    def RenderSettings "settings"
    {
        uniform int2 resolution = (640, 360)
        float crust:subdivisionEdgeLength = 2
    }
}"#;
    let path = write_stage("no_camera", &body, no_camera);
    let scene = load(&path, &UsdImportOptions::default());
    assert_eq!(scene.stats.subdivision.adaptive, None);
    assert_eq!(
        unique_triangles(&scene),
        cube_triangles(0),
        "the default level 0"
    );
}

/// A bad authored edge length is ignored: uniform subdivision.
#[test]
fn a_bad_edge_length_is_ignored() {
    let body = format!("{CAMERA}{}", cube("Near", (0.0, 0.0, -6.0), ""));
    let path = write_stage(
        "bad_length",
        &body,
        &settings("float crust:subdivisionEdgeLength = -2"),
    );
    let scene = load(&path, &UsdImportOptions::default());
    assert_eq!(scene.stats.subdivision.adaptive, None);
    assert_eq!(unique_triangles(&scene), cube_triangles(0));
}

/// A two-part prototype, Red and Blue cubes side by side.
fn pair_prototype(path: &str) -> String {
    format!(
        r#"
            def Xform "Pair" {{
{}{}
            }}"#,
        cube(
            "L",
            (-1.5, 0.0, 0.0),
            &format!("rel material:binding = <{path}/Looks/Red>")
        )
        .replace("\n", "\n        "),
        cube(
            "R",
            (1.5, 0.0, 0.0),
            &format!("rel material:binding = <{path}/Looks/Blue>")
        )
        .replace("\n", "\n        "),
    )
}

/// A `PointInstancer` placing one prototype near the camera and twice far
/// from it: the near placement draws a refined version, the far ones share
/// one unrefined version, and every part of each keeps its material.
#[test]
fn instances_at_different_distances_get_their_own_version() {
    let body = format!(
        r#"{CAMERA}{LOOKS}
    def PointInstancer "Scatter" {{
        rel prototypes = [</W/Scatter/Protos/Pair>]
        int[] protoIndices = [0, 0, 0]
        point3f[] positions = [(0, 0, -8), (-30, 0, -3000), (30, 0, -3000)]
        def Scope "Protos" {{{}
        }}
    }}"#,
        pair_prototype("/W")
    );
    let path = write_stage(
        "instancer",
        &body,
        &settings("float crust:subdivisionEdgeLength = 2"),
    );
    let scene = load(&path, &UsdImportOptions::default());
    let sub = &scene.stats.subdivision;
    assert_eq!(
        sub.prototype_versions, 2,
        "near, and one for both far placements"
    );
    assert_eq!(sub.rate_dependent_versions, 2);
    // Every part of every placement is there and keeps its look.
    let red = material_at(&scene, (-1.5, 0.0, -8.0));
    let blue = material_at(&scene, (1.5, 0.0, -8.0));
    assert_ne!(red, blue);
    for x in [-30.0, 30.0] {
        assert_eq!(material_at(&scene, (x - 1.5, 0.0, -3000.0)), red);
        assert_eq!(material_at(&scene, (x + 1.5, 0.0, -3000.0)), blue);
    }
    assert_eq!(
        unique_triangles(&scene),
        2 * cube_triangles(3) + 2 * cube_triangles(0),
        "each version's two parts, once"
    );
}

/// A prototype with no subdivision mesh has one version however its
/// placements spread.
#[test]
fn a_prototype_without_subdivision_meshes_has_one_version() {
    let flat_cube = CUBE.replace("catmullClark", "none");
    let body = format!(
        r#"{CAMERA}
    def PointInstancer "Scatter" {{
        rel prototypes = [</W/Scatter/Protos/Box>]
        int[] protoIndices = [0, 0, 0]
        point3f[] positions = [(0, 0, -8), (0, 0, -300), (0, 0, -3000)]
        def Scope "Protos" {{
            def Mesh "Box" {{
{flat_cube}
            }}
        }}
    }}"#
    );
    let path = write_stage(
        "rate_independent",
        &body,
        &settings("float crust:subdivisionEdgeLength = 2"),
    );
    let scene = load(&path, &UsdImportOptions::default());
    assert_eq!(scene.stats.subdivision.prototype_versions, 1);
    assert_eq!(scene.stats.subdivision.rate_dependent_versions, 0);
}

/// A cube scaled by 2 at a distance where it needs level 2, placed directly
/// and through a nested scatter: the nested placement composes its scale
/// into the outer placement's rate and gets at least the direct level.
#[test]
fn a_nested_scatter_is_conservative() {
    let scaled = |name: &str| {
        format!(
            r#"
    def Mesh "{name}"
    {{
{CUBE}
        double3 xformOp:translate = (0, 0, -1000)
        float3 xformOp:scale = (2, 2, 2)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:scale"]
    }}"#
        )
    };
    let direct = write_stage(
        "nested_direct",
        &format!("{CAMERA}{}", scaled("Box")),
        &settings("float crust:subdivisionEdgeLength = 2"),
    );
    let direct = load(&direct, &UsdImportOptions::default());
    let nested = write_stage(
        "nested_scatter",
        &format!(
            r#"{CAMERA}
    def PointInstancer "Outer" {{
        rel prototypes = [</W/Outer/Protos/Grove>]
        int[] protoIndices = [0]
        point3f[] positions = [(0, 0, -1000)]
        def Scope "Protos" {{
            def PointInstancer "Grove" {{
                rel prototypes = [</W/Outer/Protos/Grove/Protos/Box>]
                int[] protoIndices = [0]
                point3f[] positions = [(0, 0, 0)]
                float3[] scales = [(2, 2, 2)]
                def Scope "Protos" {{
                    def Mesh "Box" {{
{CUBE}
                    }}
                }}
            }}
        }}
    }}"#
        ),
        &settings("float crust:subdivisionEdgeLength = 2"),
    );
    let nested = load(&nested, &UsdImportOptions::default());
    let direct_tris = unique_triangles(&direct);
    assert_eq!(
        direct_tris,
        cube_triangles(2),
        "the direct placement's level"
    );
    assert!(
        unique_triangles(&nested) >= direct_tris,
        "nested {} < direct {direct_tris}",
        unique_triangles(&nested)
    );
}

/// The same placements imported streamed (one masked stage per top-level
/// subtree) and as a single stage (all of them under one subtree) are refined
/// to the same level: every placement's surface is hit at the same distance.
#[test]
fn deterministic_across_import_modes() {
    let placements = [
        ("A", (0.0, 0.0, -6.0)),
        ("B", (4.0, 0.0, -40.0)),
        ("C", (-6.0, 0.0, -300.0)),
        ("D", (40.0, 0.0, -3000.0)),
    ];
    // Native instances of one prototype class.
    let instance = |name: &str, at: (f32, f32, f32)| {
        format!(
            r#"
def Xform "{name}" (instanceable = true; references = </Proto>) {{
    double3 xformOp:translate = ({}, {}, {})
    uniform token[] xformOpOrder = ["xformOp:translate"]
}}"#,
            at.0, at.1, at.2
        )
    };
    let all: String = placements.iter().map(|&(n, at)| instance(n, at)).collect();
    let stage = |name: &str, placed: &str| {
        let path = std::env::temp_dir()
            .join("crust_usd_adaptive_tests")
            .join(format!("{name}.usda"));
        std::fs::create_dir_all(path.parent().unwrap()).expect("temp dir");
        let text = format!(
            "#usda 1.0\n(\n    upAxis = \"Y\"\n)\ndef Xform \"W\" {{{CAMERA}}}\nclass Xform \"Proto\" {{\n    def Mesh \"Box\" {{\n{CUBE}\n    }}\n}}\n{placed}\n{}\n",
            settings("float crust:subdivisionEdgeLength = 2")
        );
        std::fs::write(&path, text).expect("write stage");
        load(&path, &UsdImportOptions::default())
    };
    // Streams: seven root prims, one masked stage each.
    let streamed = stage("streamed", &all);
    // A single stage: the placements under one root, three roots in all.
    let single = stage("single", &format!("def Xform \"All\" {{{all}\n}}"));
    // Streaming is visible in the versions: each chunk is its own stage epoch,
    // so it builds its own, where the single stage shares A's and B's (both
    // clamped to the top of the cube's range).
    assert_eq!(single.stats.subdivision.prototype_versions, 3);
    assert_eq!(streamed.stats.subdivision.prototype_versions, 4);
    for &(name, at) in &placements {
        let (a, b) = (hit_t(&streamed, at), hit_t(&single, at));
        assert!(a.is_some(), "placement {name} is missing");
        assert_eq!(
            a.map(f32::to_bits),
            b.map(f32::to_bits),
            "placement {name} refined differently"
        );
    }
    assert_eq!(unique_triangles(&streamed), unique_triangles(&single));
}

/// `samples/subdivision_adaptive.usda`: five placements of one cage at
/// distances 8 to 400, refined to levels 3, 3, 2, 1 and 0.
#[test]
fn the_adaptive_sample_refines_each_placement_to_its_distance() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/subdivision_adaptive.usda");
    let scene = load(&path, &UsdImportOptions::default());
    let sub = &scene.stats.subdivision;
    assert_eq!(sub.adaptive, Some((8.0, 3)));
    assert_eq!(sub.levels, vec![1, 1, 1, 2]);
    assert_eq!(sub.prototype_versions, 5);
    // The two level-3 versions share one mesh; the floor adds 2.
    assert_eq!(
        unique_triangles(&scene),
        cube_triangles(3) + cube_triangles(2) + cube_triangles(1) + cube_triangles(0) + 2
    );
}
