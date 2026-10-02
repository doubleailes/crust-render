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
        sub.per_face_meshes, 3,
        "each placement read, tessellated per face"
    );
    assert_eq!(sub.per_face_fallbacks, 0);
    assert!(sub.levels.is_empty(), "no mesh took the per-mesh level");
    // The near cube's edges are all split 8 times, the far ones' not at all:
    // 12 edges per read.
    assert_eq!(sub.rate_bins.first(), Some(&24), "two far reads at rate 1");
    assert_eq!(sub.rate_bins.get(3), Some(&12), "the near read at rate 8");
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

/// A `PointInstancer` placing one prototype near the camera and twice far from
/// it shares it: every placement draws one level-0 version, and every part of
/// it keeps its material.
#[test]
fn a_shared_prototype_gets_the_uniform_level() {
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
    assert_eq!(sub.shared_meshes, 2, "the prototype's two parts, read once");
    assert_eq!(sub.shared_level, 0);
    assert_eq!(sub.per_face_meshes, 0);
    assert_eq!(unique_triangles(&scene), 2 * cube_triangles(0));
    let red = material_at(&scene, (-1.5, 0.0, -8.0));
    let blue = material_at(&scene, (1.5, 0.0, -8.0));
    assert_ne!(red, blue);
    for x in [-30.0, 30.0] {
        assert_eq!(material_at(&scene, (x - 1.5, 0.0, -3000.0)), red);
        assert_eq!(material_at(&scene, (x + 1.5, 0.0, -3000.0)), blue);
    }
}

/// A cube scaled by 2 at a distance where it needs level 2, placed directly
/// and through a nested scatter that places each prototype once: the nested
/// placement is unshared, composes its transform to world, and is refined as
/// the direct one.
#[test]
fn a_nested_scatter_placed_once_is_rated_in_world_space() {
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
    assert_eq!(
        unique_triangles(&nested),
        direct_tris,
        "placed once at every level, the nested box is unshared and rated as the direct one"
    );
}

/// A stage whose subdivision prototype is placed once in each of two top-level
/// subtrees, written with `extra_roots` empty root prims besides its three.
fn two_subtrees(name: &str, extra_roots: usize) -> PathBuf {
    let instance = |name: &str, z: f32| {
        format!(
            r#"
def Xform "{name}" (instanceable = true; references = </W/Proto>) {{
    double3 xformOp:translate = (0, 0, {z})
    uniform token[] xformOpOrder = ["xformOp:translate"]
}}"#
        )
    };
    let extra: String = (0..extra_roots)
        .map(|k| format!("\ndef Xform \"Empty{k}\" {{}}"))
        .collect();
    let path = std::env::temp_dir()
        .join("crust_usd_adaptive_tests")
        .join(format!("{name}.usda"));
    std::fs::create_dir_all(path.parent().unwrap()).expect("temp dir");
    let text = format!(
        r#"#usda 1.0
(
    upAxis = "Y"
    renderSettingsPrimPath = "/W/Render/settings"
)
def Xform "W" {{
    def Camera "Cam" {{}}
    class Xform "Proto" {{
        def Mesh "Box" {{
{CUBE}
        }}
    }}
    def Scope "Render" {{
        def RenderSettings "settings" {{
            rel camera = </W/Cam>
            uniform int2 resolution = (640, 360)
            float crust:subdivisionEdgeLength = 2
        }}
    }}
}}{}{}{extra}
"#,
        instance("A", -6.0),
        instance("B", -300.0)
    );
    std::fs::write(&path, text).expect("write stage");
    path
}

/// Placements are counted per top-level subtree in every import mode: a
/// prototype placed once in each of two subtrees is unshared in both, whether
/// the stage is imported as one stage (three roots) or streamed (five).
#[test]
fn placements_are_counted_per_subtree_in_every_import_mode() {
    let single = load(&two_subtrees("single", 0), &UsdImportOptions::default());
    let streamed = load(&two_subtrees("streamed", 2), &UsdImportOptions::default());
    for (scene, mode) in [(&single, "single-stage"), (&streamed, "streamed")] {
        let sub = &scene.stats.subdivision;
        assert_eq!(sub.per_face_meshes, 2, "{mode}: both placements unshared");
        assert_eq!(sub.shared_meshes, 0, "{mode}");
    }
    for (name, at) in [("A", (0.0, 0.0, -6.0)), ("B", (0.0, 0.0, -300.0))] {
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

/// An `instanceable` prim that is its prototype's one placement is refined like
/// the same mesh placed directly.
#[test]
fn an_instance_placed_once_is_refined_as_a_direct_mesh() {
    let direct = write_stage(
        "once_direct",
        &format!("{CAMERA}{}", cube("Box", (0.0, 0.0, -6.0), "")),
        &settings("float crust:subdivisionEdgeLength = 2"),
    );
    let direct = load(&direct, &UsdImportOptions::default());
    let instanced = write_stage(
        "once_instanced",
        &format!(
            r#"{CAMERA}
    class Xform "Proto" {{
        def Mesh "Box" {{
{CUBE}
        }}
    }}
    def Xform "Placed" (instanceable = true; references = </W/Proto>) {{
        double3 xformOp:translate = (0, 0, -6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}"#
        ),
        &settings("float crust:subdivisionEdgeLength = 2"),
    );
    let instanced = load(&instanced, &UsdImportOptions::default());
    assert_eq!(instanced.stats.subdivision.per_face_meshes, 1);
    assert_eq!(instanced.stats.subdivision.shared_meshes, 0);
    assert_eq!(unique_triangles(&instanced), unique_triangles(&direct));
    for at in [(0.0, 0.0, -6.0), (0.4, 0.3, -6.0), (-0.6, -0.2, -6.0)] {
        let (a, b) = (hit_t(&instanced, at).unwrap(), hit_t(&direct, at).unwrap());
        assert!((a - b).abs() < 1e-4, "at {at:?}: instanced {a}, direct {b}");
    }
}

/// `--subdiv-level 2` caps an unshared mesh at a rate of 4 per edge and refines
/// every shared prototype to level 2.
#[test]
fn the_level_setting_caps_unshared_and_sets_the_shared_level() {
    let body = format!(
        r#"{CAMERA}{}
    def PointInstancer "Scatter" {{
        rel prototypes = [</W/Scatter/Protos/Box>]
        int[] protoIndices = [0, 0]
        point3f[] positions = [(-30, 0, -3000), (30, 0, -3000)]
        def Scope "Protos" {{
            def Mesh "Box" {{
{CUBE}
            }}
        }}
    }}"#,
        cube("Near", (0.0, 0.0, -6.0), "")
    );
    let path = write_stage("level_two", &body, &settings(""));
    let options = UsdImportOptions {
        subdivision_edge_length: Some(2.0),
        subdivision_level: Some(2),
        ..UsdImportOptions::default()
    };
    let scene = load(&path, &options);
    let sub = &scene.stats.subdivision;
    assert_eq!(sub.adaptive, Some((2.0, 2)));
    assert_eq!((sub.shared_meshes, sub.shared_level), (1, 2));
    assert_eq!(sub.per_face_meshes, 1);
    assert_eq!(
        sub.rate_bins.len(),
        3,
        "no edge past a rate of 4: {:?}",
        sub.rate_bins
    );
    assert_eq!(unique_triangles(&scene), cube_triangles(2) * 2);
}

/// `samples/subdivision_adaptive.usda`: five direct references to one cage at
/// distances 8 to 400, tessellated at rates 8, 8, 4, 2 and 1.
#[test]
fn the_adaptive_sample_refines_each_placement_to_its_distance() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/subdivision_adaptive.usda");
    let scene = load(&path, &UsdImportOptions::default());
    let sub = &scene.stats.subdivision;
    assert_eq!(sub.adaptive, Some((8.0, 3)));
    // One cube read per prim, at rates 8, 8, 4, 2, 1: the cube's edges are all
    // the same length, so a rate of 2^L is exactly the per-mesh level L.
    assert_eq!(sub.per_face_meshes, 5);
    assert_eq!(sub.rate_bins, vec![12, 12, 12, 24]);
    // The two rate-8 cubes share one mesh; the floor adds 2.
    assert_eq!(
        unique_triangles(&scene),
        cube_triangles(3) + cube_triangles(2) + cube_triangles(1) + cube_triangles(0) + 2
    );
}

/// A 4-wide strip of unit quads, curved, from beside the camera to `length`
/// units away, at `y = −1`.
fn strip(length: usize) -> String {
    let mut points = Vec::new();
    for j in 0..=length {
        for i in 0..=4 {
            let (x, z) = (i as f32 - 2.0, -(j as f32));
            let y = -1.0 + 0.3 * (x * 1.3).sin() + 0.3 * (z * 0.7).sin();
            points.push(format!("({x}, {y}, {z})"));
        }
    }
    let mut indices = Vec::new();
    for j in 0..length {
        for i in 0..4 {
            let a = j * 5 + i;
            indices.extend([a, a + 1, a + 6, a + 5].map(|v| v.to_string()));
        }
    }
    format!(
        r#"
    def Mesh "Strip"
    {{
        uniform token subdivisionScheme = "catmullClark"
        int[] faceVertexCounts = [{}]
        int[] faceVertexIndices = [{}]
        point3f[] points = [{}]
    }}"#,
        vec!["4"; 4 * length].join(", "),
        indices.join(", "),
        points.join(", ")
    )
}

/// One mesh spanning near and far is fine near the camera and coarse far
/// away — where the per-mesh level refines all of it to the ceiling.
#[test]
fn a_large_mesh_is_fine_near_and_coarse_far() {
    let length = 3000;
    let path = write_stage(
        "strip",
        &format!("{CAMERA}{}", strip(length)),
        &settings("float crust:subdivisionEdgeLength = 2"),
    );
    let scene = load(&path, &UsdImportOptions::default());
    let sub = &scene.stats.subdivision;
    assert_eq!(sub.per_face_meshes, 1);
    let rate_one = sub.rate_bins.first().copied().unwrap_or(0);
    let rate_eight = sub.rate_bins.get(3).copied().unwrap_or(0);
    assert!(
        rate_one > 0 && rate_eight > 0,
        "edge rates {:?}",
        sub.rate_bins
    );
    // Unit edges project below 2 px beyond ~760 units: three quarters of the
    // strip stays at its cage resolution.
    let per_mesh = 4 * length * 2 * 64;
    let tris = unique_triangles(&scene);
    assert!(
        tris * 8 < per_mesh,
        "{tris} triangles against the per-mesh level's {per_mesh}"
    );
}

/// Rays aimed at points along the strip's shared cage edges, where its rate
/// falls from 8 to 1, all hit: no crack between faces of different rates.
#[test]
fn faces_of_different_rates_meet_without_a_crack() {
    let path = write_stage(
        "strip_cracks",
        &format!("{CAMERA}{}", strip(1200)),
        &settings("float crust:subdivisionEdgeLength = 2"),
    );
    let scene = load(&path, &UsdImportOptions::default());
    let down = -Vec3A::Y;
    let mut rays = 0;
    // Along the lines x = −1, 0, 1 (cage edges running down the strip) and
    // z = −k (edges across it), where the rates step down.
    for k in 0..4000 {
        let z = -400.0 - k as f32 * 0.137;
        for x in [-1.0f32, 0.0, 1.0] {
            for (px, pz) in [(x, z), (x + 0.37, z.round())] {
                let hit =
                    scene
                        .world
                        .intersect(&Ray::new(Vec3A::new(px, 10.0, pz), down), 1e-3, 100.0);
                assert!(
                    hit.is_some(),
                    "a ray at ({px}, {pz}) passed through the strip"
                );
                rays += 1;
            }
        }
    }
    assert_eq!(rays, 24_000);
}

/// A `loop` cage keeps the per-mesh level, counted; a face-varying chart is
/// tessellated per face, through the patch table's face-varying patches.
#[test]
fn a_loop_mesh_falls_back_and_a_face_varying_chart_does_not() {
    let dir = std::env::temp_dir().join("crust_usd_adaptive_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(
        dir.join("adaptive_flat.mtlx"),
        r#"<?xml version="1.0"?>
<materialx version="1.38">
  <oren_nayar_diffuse_bsdf name="flat_diffuse" type="BSDF">
    <input name="color" type="color3" value="0.8, 0.8, 0.8" />
  </oren_nayar_diffuse_bsdf>
  <surface name="flat_surface" type="surfaceshader">
    <input name="bsdf" type="BSDF" nodename="flat_diffuse" />
  </surface>
  <surfacematerial name="mtlx_flat" type="material">
    <input name="surfaceshader" type="surfaceshader" nodename="flat_surface" />
  </surfacematerial>
</materialx>
"#,
    )
    .expect("write mtlx");
    let body = format!(
        r#"{CAMERA}
    def Scope "Looks" {{
        def Material "Mtl" (
            prepend references = @adaptive_flat.mtlx@</MaterialX/Materials/mtlx_flat>
        ) {{
        }}
    }}
    def Mesh "Charted" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, -1, -6), (1, -1, -6), (1, 1, -6), (-1, 1, -6)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (interpolation = "faceVarying")
        uniform token subdivisionScheme = "catmullClark"
        rel material:binding = </W/Looks/Mtl>
    }}
    def Mesh "Loop"
    {{
        int[] faceVertexCounts = [3, 3, 3, 3]
        int[] faceVertexIndices = [0, 1, 2, 0, 2, 3, 0, 3, 1, 1, 3, 2]
        point3f[] points = [(4, 0, -8), (5, 0, -9), (3, 0, -9), (4, 1.5, -8.6)]
        uniform token subdivisionScheme = "loop"
    }}
    def Mesh "Plain"
    {{
{CUBE}
        double3 xformOp:translate = (-4, 0, -8)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}"#
    );
    let path = write_stage(
        "fallbacks",
        &body,
        &settings("float crust:subdivisionEdgeLength = 2"),
    );
    let scene = load(&path, &UsdImportOptions::default());
    let sub = &scene.stats.subdivision;
    assert_eq!(
        sub.per_face_meshes, 2,
        "the plain cube and the charted quad"
    );
    assert_eq!(sub.per_face_fallbacks, 1, "the loop mesh");
    assert_eq!(
        sub.levels.iter().sum::<u64>(),
        1,
        "the fallback read at a per-mesh level"
    );
    // The charted quad still renders.
    assert!(hit_t(&scene, (0.0, 0.0, -6.0)).is_some());
}

/// Geometry wholly outside the camera's view is not refined: the same cube in
/// front of the camera is split 8 times per edge, behind it once.
#[test]
fn geometry_out_of_view_is_not_refined() {
    let body = format!(
        "{CAMERA}{}{}",
        cube("Ahead", (0.0, 0.0, -6.0), ""),
        cube("Behind", (0.0, 0.0, 6.0), "")
    );
    let path = write_stage(
        "frustum",
        &body,
        &settings("float crust:subdivisionEdgeLength = 2"),
    );
    let scene = load(&path, &UsdImportOptions::default());
    let sub = &scene.stats.subdivision;
    assert_eq!(sub.per_face_meshes, 2);
    assert_eq!(
        sub.rate_bins.first(),
        Some(&12),
        "the cube behind, at rate 1"
    );
    assert_eq!(sub.rate_bins.get(3), Some(&12), "the cube ahead, at rate 8");
    assert_eq!(
        unique_triangles(&scene),
        cube_triangles(3) + cube_triangles(0)
    );
    // Still there for rays that leave the frustum (reflections, shadows).
    let hit = scene
        .world
        .intersect(&Ray::new(Vec3A::ZERO, Vec3A::Z), 1e-3, 100.0);
    assert!(hit.is_some(), "the cube behind the camera is still hit");
}
