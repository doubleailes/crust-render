//! Scalar displacement at import, driven by small stages written on the fly:
//! which material inputs displace, how far and in which frame, what is shared,
//! and what `--stats` says about it. Geometry is checked by rays and bounds,
//! in numbers, never by eye.

use crust_core::{
    AssetLoader, ColorSpace, EnvironmentMap, NoAssets, PtexTexture, Ray, Scene, Texture2D,
    UsdImportOptions, Vec3A,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn write_stage(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_usd_displacement_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, text).expect("write stage");
    path
}

fn stage(body: &str) -> String {
    format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
{body}
}}
"#
    )
}

fn load_with(name: &str, body: &str, level: u32, assets: &dyn AssetLoader) -> Scene {
    let path = write_stage(name, &stage(body));
    let options = UsdImportOptions {
        subdivision_level: Some(level),
        ..UsdImportOptions::default()
    };
    Scene::from_usd_with_options(&path, assets, &options).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn load(name: &str, body: &str, level: u32) -> Scene {
    load_with(name, body, level, &NoAssets)
}

/// Where a ray straight down at `(x, z)` meets the surface.
fn height_at(scene: &Scene, x: f32, z: f32) -> Option<f32> {
    let ray = Ray::new(Vec3A::new(x, 10.0, z), Vec3A::new(0.0, -1.0, 0.0));
    scene
        .world
        .intersect(&ray, 1e-4, 1e4)
        .map(|h| 10.0 - h.rec.t)
}

/// A preview material displacing by `value`.
fn preview_material(name: &str, displacement: &str) -> String {
    format!(
        r#"    def Material "{name}"
    {{
        token outputs:surface.connect = </World/{name}/Surface.outputs:surface>
        def Shader "Surface"
        {{
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (0.5, 0.5, 0.5)
            {displacement}
            token outputs:surface
        }}
    }}
"#
    )
}

/// A unit-square ground at y = 0 over `[0, 1]^2` in XZ, one quad, its
/// `st` equal to `(x, z)`.
fn ground(name: &str, scheme: &str, material: &str, extra: &str) -> String {
    format!(
        r#"    def Mesh "{name}" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token subdivisionScheme = "{scheme}"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (0, 0, 1), (1, 0, 1), (1, 0, 0)]
        texCoord2f[] primvars:st = [(0, 0), (0, 1), (1, 1), (1, 0)] (interpolation = "vertex")
        rel material:binding = </World/{material}>
        {extra}
    }}
"#
    )
}

/// "A constant preview displacement", on a `none` quad: diced bilinearly to
/// level 2 (its faces stay flat), every vertex 0.1 along the normal.
#[test]
fn a_constant_preview_displacement_moves_a_none_mesh() {
    let body = format!(
        "{}{}",
        preview_material("Up", "float inputs:displacement = 0.1"),
        ground("Ground", "none", "Up", "")
    );
    let scene = load("constant_none", &body, 2);
    // Scheme none with displacement: refined bilinearly, 16 quads.
    assert_eq!(scene.world.primitive_breakdown().triangles, 32);
    for (x, z) in [(0.5, 0.5), (0.1, 0.9), (0.73, 0.21)] {
        let y = height_at(&scene, x, z).expect("hit");
        assert!((y - 0.1).abs() < 1e-5, "({x}, {z}) at {y}");
    }
    let d = &scene.stats.displacement;
    assert_eq!(d.meshes, 1);
    assert_eq!(d.vertices, 25);
    assert_eq!(d.max_offset, 0.1);
    assert_eq!(d.at_cage, 0);
    assert!(scene.stats.report().contains("displacement"));
}

/// The same `none` mesh without a displacement is its faceted cage, as
/// before — what `CRUST_DISPLACE=0` reduces every mesh to.
#[test]
fn a_none_mesh_without_displacement_stays_its_cage() {
    let body = format!(
        "{}{}",
        preview_material("Flat", ""),
        ground("Ground", "none", "Flat", "")
    );
    let scene = load("constant_none_off", &body, 2);
    assert_eq!(scene.world.primitive_breakdown().triangles, 2);
    assert_eq!(height_at(&scene, 0.5, 0.5), Some(0.0));
    assert_eq!(scene.stats.displacement.meshes, 0);
    let report = scene.stats.report();
    assert!(
        !report.contains("displacement"),
        "no displacement lines without displacement:\n{report}"
    );
}

/// At level 0 a displaced mesh moves only its cage vertices, and is counted
/// as such.
#[test]
fn a_cage_resolution_mesh_is_counted() {
    let body = format!(
        "{}{}",
        preview_material("Up", "float inputs:displacement = 0.25"),
        ground("Ground", "none", "Up", "")
    );
    let scene = load("constant_cage", &body, 0);
    assert_eq!(scene.world.primitive_breakdown().triangles, 2);
    let y = height_at(&scene, 0.5, 0.5).expect("hit");
    assert!((y - 0.25).abs() < 1e-5, "{y}");
    assert_eq!(scene.stats.displacement.at_cage, 1);
    assert_eq!(scene.stats.displacement.vertices, 4);
}

/// "A scaled placement": the offset is local, so a scale of 2 doubles it.
#[test]
fn the_offset_scales_with_the_placement() {
    let body = format!(
        r#"{}    def Xform "Big"
    {{
        double3 xformOp:scale = (2, 2, 2)
        uniform token[] xformOpOrder = ["xformOp:scale"]
{}    }}
"#,
        preview_material("Up", "float inputs:displacement = 0.1"),
        ground("Ground", "none", "Up", "")
    );
    let scene = load("scaled", &body, 1);
    let y = height_at(&scene, 1.0, 1.0).expect("hit");
    assert!((y - 0.2).abs() < 1e-5, "{y}");
}

/// "One cage placed twice": one displaced mesh, both placements displaced.
#[test]
fn a_shared_cage_is_displaced_once() {
    let body = format!(
        r#"{}{}    def Xform "Moved"
    {{
        double3 xformOp:translate = (3, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
{}    }}
"#,
        preview_material("Up", "float inputs:displacement = 0.1"),
        ground("A", "none", "Up", ""),
        ground("B", "none", "Up", "")
    );
    let scene = load("shared", &body, 1);
    assert_eq!(scene.stats.displacement.meshes, 1, "displaced once");
    for x in [0.5, 3.5] {
        let y = height_at(&scene, x, 0.5).expect("hit");
        assert!((y - 0.1).abs() < 1e-5, "x = {x}: {y}");
    }
}

/// A texture that is `u` in red and `v` in green, and records the colour
/// space it was requested in.
struct Ramp;
impl Texture2D for Ramp {
    fn eval(&self, u: f32, v: f32, _width: f32) -> [f32; 4] {
        [u, v, 0.0, 1.0]
    }
}

#[derive(Default)]
struct RampAssets {
    requested: Mutex<Vec<(PathBuf, ColorSpace)>>,
    ptex_requested: Mutex<Vec<(PathBuf, ColorSpace)>>,
}
impl AssetLoader for RampAssets {
    fn load_environment(
        &self,
        _path: &Path,
        _space: crust_core::ColorSpace,
    ) -> Option<EnvironmentMap> {
        None
    }
    fn load_texture(&self, path: &Path, space: ColorSpace) -> Option<Arc<dyn Texture2D>> {
        self.requested
            .lock()
            .unwrap()
            .push((path.to_path_buf(), space));
        Some(Arc::new(Ramp))
    }
    fn load_ptex(&self, path: &Path, space: ColorSpace) -> Option<Arc<dyn PtexTexture>> {
        self.ptex_requested
            .lock()
            .unwrap()
            .push((path.to_path_buf(), space));
        None
    }
}

const TEXTURED: &str = r#"inputs:displacement.connect = </World/Height/Map.outputs:r>
            token outputs:surface
        }
        def Shader "Map"
        {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @height.png@
            float4 inputs:scale = (0.2, 0.2, 0.2, 1)
            float4 inputs:bias = (-0.1, -0.1, -0.1, 0)
            float2 inputs:st.connect = </World/Height/Reader.outputs:result>
            float outputs:r
        }
        def Shader "Reader"
        {
            uniform token info:id = "UsdPrimvarReader_float2"
            string inputs:varname = "st"
            float2 outputs:result"#;

/// "A textured preview displacement": `0.2 · r − 0.1` at each vertex's
/// texture coordinate, the file requested raw.
#[test]
fn a_textured_preview_displacement_reads_scale_and_bias() {
    // The texture and reader nodes are spliced in after the surface's input;
    // the template's trailing `outputs:surface` lands on the reader, harmlessly.
    let material = preview_material("Height", &format!("float {TEXTURED}"));
    let body = format!("{material}{}", ground("Ground", "none", "Height", ""));
    let assets = RampAssets::default();
    let scene = load_with("textured", &body, 2, &assets);
    // r = u = x; bilinear dicing of a linear function is exact.
    for (x, z) in [(0.0, 0.5), (0.5, 0.5), (1.0, 0.25), (0.8, 0.9)] {
        let x: f32 = x;
        let y = height_at(&scene, x.clamp(1e-4, 1.0 - 1e-4), z).expect("hit");
        let want = 0.2 * x.clamp(1e-4, 1.0 - 1e-4) - 0.1;
        assert!((y - want).abs() < 1e-4, "({x}, {z}): {y} vs {want}");
    }
    let requested = assets.requested.lock().unwrap();
    assert!(
        requested.iter().all(|(_, s)| *s == ColorSpace::RAW),
        "a displacement map is read raw: {requested:?}"
    );
    assert!((scene.stats.displacement.max_offset - 0.1).abs() < 1e-6);
    // No bound authored: unknown.
    assert_eq!(scene.stats.displacement.frustum_skipped, 0, "uniform mode");
}

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

/// `samples/displacement.usda` loads, and — with no host to decode its
/// height map — each ground falls back the way its schema says: the preview
/// ground to `inputs:displacement`'s default of 0, which displaces nothing,
/// and the MaterialX ground to its `image` node's default of 0.5, times the
/// node's scale of 0.3. The cube's constant 0.1 needs no texture.
#[test]
fn the_displacement_sample_loads() {
    let scene = Scene::from_usd(&sample("displacement.usda")).expect("loads");
    let d = &scene.stats.displacement;
    assert_eq!(d.meshes, 2, "the cube and the MaterialX ground");
    assert!((d.max_offset - 0.15).abs() < 1e-6, "{}", d.max_offset);
    assert_eq!(d.at_cage, 0, "diced at the stage's level 6");
    assert!(scene.stats.report().contains("displacement"));
}

/// "No bound known", in adaptive mode: a texture-displaced mesh with no
/// `crust:displacementBound` has its frustum test turned off, and `--stats`
/// counts it; authoring a bound turns the test back on.
#[test]
fn an_unbounded_displacement_skips_the_frustum_test_in_adaptive_mode() {
    let run = |name: &str, extra: &str| {
        let material = preview_material("Height", &format!("float {TEXTURED}"));
        let ground = ground("Ground", "none", "Height", extra).replace(
            "rel material:binding",
            "double3 xformOp:translate = (0, -1, -5)\n        uniform token[] xformOpOrder = [\"xformOp:translate\"]\n        rel material:binding",
        );
        let text = format!(
            r#"{}
def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/Cam>
        uniform int2 resolution = (320, 180)
        float crust:subdivisionEdgeLength = 4
    }}
}}
"#,
            stage(&format!("    def Camera \"Cam\" {{}}\n{material}{ground}"))
        );
        let path = write_stage(name, &text);
        let assets = RampAssets::default();
        Scene::from_usd_with_options(&path, &assets, &UsdImportOptions::default())
            .unwrap_or_else(|e| panic!("{name}: {e}"))
    };
    let unbounded = run("adaptive_unbounded", "");
    assert_eq!(unbounded.stats.displacement.meshes, 1);
    assert_eq!(unbounded.stats.displacement.frustum_skipped, 1);
    assert!(unbounded.stats.report().contains("frustum test skipped"));
    let bounded = run("adaptive_bounded", "float crust:displacementBound = 0.1");
    assert_eq!(bounded.stats.displacement.meshes, 1);
    assert_eq!(bounded.stats.displacement.frustum_skipped, 0);
}

/// A displacement is resolved only for a mesh: a `PxrDisplace` material bound
/// to a sphere opens no displacement map, the same material on a mesh opens
/// it raw.
#[test]
fn only_meshes_open_displacement_maps() {
    let material = r#"    def Material "Rock"
    {
        token outputs:ri:surface.connect = </World/Rock/Bsdf.outputs:bxdf_out>
        token outputs:ri:displacement.connect = </World/Rock/PxrDisplace.outputs:displace>
        def Shader "Bsdf"
        {
            uniform token info:id = "PxrDisneyBsdf"
            token outputs:bxdf_out
        }
        def Shader "PxrDisplace"
        {
            uniform token info:id = "PxrDisplace"
            float inputs:dispAmount = 0.1
            float inputs:dispScalar.connect = </World/Rock/Tex.outputs:resultR>
            token outputs:displace
        }
        def Shader "Tex"
        {
            uniform token info:id = "PxrPtexture"
            asset inputs:filename = @height.ptx@
            float outputs:resultR
        }
    }
"#;
    let sphere = r#"    def Sphere "Ball" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        rel material:binding = </World/Rock>
    }
"#;
    let assets = RampAssets::default();
    load_with("sphere_only", &format!("{material}{sphere}"), 0, &assets);
    assert!(
        assets.ptex_requested.lock().unwrap().is_empty(),
        "a sphere opens no displacement map"
    );
    let assets = RampAssets::default();
    let mesh = ground("Ground", "none", "Rock", "");
    load_with(
        "sphere_and_mesh",
        &format!("{material}{sphere}{mesh}"),
        0,
        &assets,
    );
    let requested = assets.ptex_requested.lock().unwrap();
    assert_eq!(requested.len(), 1, "{requested:?}");
    assert_eq!(requested[0].1, ColorSpace::RAW);
}

/// A displacement connected to a `UsdUVTexture` that names no file keeps the
/// input's own authored constant, as a shading input does.
#[test]
fn an_unusable_texture_keeps_the_authored_constant() {
    let material = preview_material(
        "Fallback",
        r#"float inputs:displacement = 0.05
            float inputs:displacement.connect = </World/Fallback/Map.outputs:r>
            token outputs:surface
        }
        def Shader "Map"
        {
            uniform token info:id = "UsdUVTexture"
            float outputs:r"#,
    );
    let body = format!("{material}{}", ground("Ground", "none", "Fallback", ""));
    let scene = load_with("unusable_texture", &body, 1, &RampAssets::default());
    let y = height_at(&scene, 0.5, 0.5).expect("hit");
    assert!((y - 0.05).abs() < 1e-5, "{y}");
}

/// The displacement texture's primvar reader names the chart a mesh reads
/// when its surface reads none: here `uv2`, which the fallback names (`st`,
/// `uv`, `st0`, `UVMap`) would never find.
#[test]
fn a_displacement_texture_names_its_chart() {
    let material = preview_material("Height", &format!("float {TEXTURED}")).replace(
        r#"string inputs:varname = "st""#,
        r#"string inputs:varname = "uv2""#,
    );
    let mesh = ground("Ground", "none", "Height", "").replace("primvars:st", "primvars:uv2");
    let scene = load_with(
        "named_chart",
        &format!("{material}{mesh}"),
        2,
        &RampAssets::default(),
    );
    for x in [0.25f32, 0.75] {
        let y = height_at(&scene, x, 0.5).expect("hit");
        assert!((y - (0.2 * x - 0.1)).abs() < 1e-4, "x = {x}: {y}");
    }
}

/// A `PxrDisplace` whose `info:id` is authored as a string is found too.
#[test]
fn a_string_info_id_is_read() {
    let material = r#"    def Material "Rock"
    {
        token outputs:ri:surface.connect = </World/Rock/Bsdf.outputs:bxdf_out>
        token outputs:ri:displacement.connect = </World/Rock/PxrDisplace.outputs:displace>
        def Shader "Bsdf"
        {
            uniform token info:id = "PxrDisneyBsdf"
            token outputs:bxdf_out
        }
        def Shader "PxrDisplace"
        {
            uniform string info:id = "PxrDisplace"
            float inputs:dispAmount = 0.1
            float inputs:dispScalar = 1
            token outputs:displace
        }
    }
"#;
    let body = format!("{material}{}", ground("Ground", "none", "Rock", ""));
    let scene = load("string_id", &body, 1);
    let y = height_at(&scene, 0.5, 0.5).expect("hit");
    assert!((y - 0.1).abs() < 1e-5, "{y}");
}

/// Counts the WARN events logged while it is the default subscriber.
struct CountWarnings(Arc<Mutex<usize>>);

impl tracing::Subscriber for CountWarnings {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() == tracing::Level::WARN {
            *self.0.lock().unwrap() += 1;
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// Three meshes displaced at their cage: the warning is logged once, and its
/// record counts all three, naming each.
#[test]
fn displaced_at_cage_logs_once_and_counts_every_mesh() {
    let material = preview_material("Height", &format!("float {TEXTURED}"));
    // Distinct points, so no two share one displaced copy; grouped, so the
    // stage has too few top-level prims to stream.
    let grounds: String = ["A", "B", "C"]
        .iter()
        .enumerate()
        .map(|(i, name)| {
            ground(name, "none", "Height", "").replace("(0, 0, 0)", &format!("({i}, 0, 0)"))
        })
        .collect();
    let body = format!("{material}    def Xform \"Geo\"\n    {{\n{grounds}    }}\n");
    let assets = RampAssets::default();
    let logged = Arc::new(Mutex::new(0));
    let scene = tracing::subscriber::with_default(CountWarnings(logged.clone()), || {
        load_with("cage_three", &body, 0, &assets)
    });
    assert_eq!(scene.stats.displacement.at_cage, 3);
    assert_eq!(*logged.lock().unwrap(), 1, "one log line");
    let record = scene
        .warnings
        .iter()
        .find(|w| w.code == crust_core::WarningCode::MeshDisplacedAtCage)
        .expect("a mesh.displaced_at_cage record");
    assert_eq!(record.kind, crust_core::WarningKind::Approximated);
    assert_eq!(record.count, 3);
    // In the traversal's order, which visits siblings last to first.
    assert_eq!(
        record.prims,
        ["/World/Geo/C", "/World/Geo/B", "/World/Geo/A"]
    );
}
