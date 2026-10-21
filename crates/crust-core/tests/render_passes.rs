//! The active `UsdRenderPass` on import: naming it, the settings its
//! `renderSource` chooses, the collections it does not read, and how its
//! `matte` and `cameraVisibility` collections are resolved against the
//! imported geometry and lights. What a holdout does to a camera ray is
//! `holdouts.rs`.

use crust_core::{
    Error, MASK_CAMERA, MASK_SHADOW, NoAssets, PathSampler, Ray, SamplingStrategy, Scene,
    UsdImportOptions, Vec3A, Volumes, WarningCode, ray_color,
};
use std::path::{Path, PathBuf};

fn dir() -> PathBuf {
    let dir = std::env::temp_dir().join("crust_render_pass_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A stage with `body` under `/World`, `/Render/settings` (8×8, 3 spp)
/// and `passes` under `/Render/Passes`, plus `extra` at the root.
fn stage(name: &str, body: &str, passes: &str, extra: &str) -> PathBuf {
    let path = dir().join(format!("{name}.usda"));
    let text = format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
{body}
}}

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        int2 resolution = (8, 8)
        int crust:samplesPerPixel = 3
    }}
    def Scope "Passes"
    {{
{passes}
    }}
}}
{extra}
"#
    );
    std::fs::write(&path, text).expect("write stage");
    path
}

fn open(path: &Path, pass: Option<&str>) -> Result<Scene, Error> {
    let options = UsdImportOptions {
        pass: pass.map(str::to_owned),
        ..UsdImportOptions::default()
    };
    Scene::from_usd_with_options(path, &NoAssets, &options)
}

fn load(path: &Path, pass: Option<&str>) -> Scene {
    open(path, pass).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn count(scene: &Scene, code: WarningCode) -> u64 {
    scene
        .warnings
        .iter()
        .filter(|w| w.code == code)
        .map(|w| w.count)
        .sum()
}

/// A sphere of radius 0.5 at `(x, 0, 0)`.
fn ball(name: &str, x: f32) -> String {
    format!(
        r#"    def Sphere "{name}"
    {{
        double radius = 0.5
        double3 xformOp:translate = ({x}, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
"#
    )
}

/// What a camera ray down −Z at `x` meets first: whether it is a holdout,
/// or `None` when it meets nothing.
fn camera_hit(scene: &Scene, x: f32) -> Option<bool> {
    let ray = Ray::new(Vec3A::new(x, 0.0, 10.0), -Vec3A::Z).with_mask(MASK_CAMERA);
    scene
        .world
        .intersect(&ray, 1e-3, 1e4)
        .map(|h| scene.world.is_holdout(h.geom_id))
}

/// Whether a shadow ray down −Z at `x` is blocked.
fn shadow_hit(scene: &Scene, x: f32) -> bool {
    let ray = Ray::new(Vec3A::new(x, 0.0, 10.0), -Vec3A::Z).with_mask(MASK_SHADOW);
    scene.world.intersect(&ray, 1e-3, 1e4).is_some()
}

fn pass(name: &str, body: &str) -> String {
    format!(
        r#"        def RenderPass "{name}"
        {{
{body}
        }}
"#
    )
}

// ---------------------------------------------------------------------------
// Naming the pass
// ---------------------------------------------------------------------------

/// render-passes spec, "A named pass is active" and "Passes on the stage but
/// none named".
#[test]
fn only_the_named_pass_is_active() {
    let path = stage(
        "named",
        &[ball("Char", -2.0), ball("Prop", 2.0)].concat(),
        &[
            pass(
                "char",
                "            prepend rel collection:matte:includes = </World/Prop>",
            ),
            pass(
                "bg",
                "            prepend rel collection:matte:includes = </World/Char>",
            ),
        ]
        .concat(),
        "",
    );
    let char_pass = load(&path, Some("/Render/Passes/char"));
    assert_eq!(camera_hit(&char_pass, 2.0), Some(true));
    assert_eq!(camera_hit(&char_pass, -2.0), Some(false));
    let summary = char_pass.pass.as_ref().expect("a pass");
    assert_eq!(summary.path, "/Render/Passes/char");
    assert_eq!((summary.holdouts, summary.camera_hidden), (1, 0));

    let bg = load(&path, Some("/Render/Passes/bg"));
    assert_eq!(camera_hit(&bg, 2.0), Some(false));
    assert_eq!(camera_hit(&bg, -2.0), Some(true));

    let none = load(&path, None);
    assert!(!none.world.has_holdouts());
    assert!(none.pass.is_none());
    assert_eq!(camera_hit(&none, 2.0), Some(false));
}

/// render-passes spec, "A path that is not a pass": an `Xform`, a missing
/// prim and a malformed path all stop the import, naming the path.
#[test]
fn a_path_that_is_not_a_pass_is_an_error() {
    let path = stage("not_a_pass", &ball("Char", 0.0), &pass("char", ""), "");
    for (bad, why) in [
        ("/World", "Xform"),
        ("/World/Nowhere", "no prim"),
        ("Render/Passes/char", "absolute"),
    ] {
        match open(&path, Some(bad)) {
            Err(e @ Error::PassNotFound { .. }) => {
                let msg = e.to_string();
                assert!(msg.contains(bad) && msg.contains(why), "{msg}");
            }
            other => panic!("{bad}: expected PassNotFound, got {:?}", other.map(|_| ())),
        }
    }
}

// ---------------------------------------------------------------------------
// renderSource
// ---------------------------------------------------------------------------

const CAMERAS: &str = r#"    def Camera "Cam"
    {
        double3 xformOp:translate = (0, 0, 10)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Camera "CharCam"
    {
        double3 xformOp:translate = (1, 0, 10)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
"#;

const CHAR_SETTINGS: &str = r#"
over "Render"
{
    def RenderSettings "charSettings"
    {
        int2 resolution = (16, 4)
        int crust:samplesPerPixel = 7
        rel camera = </World/CharCam>
        rel products = [</Render/charProduct>]
    }
    def RenderProduct "charProduct"
    {
        token productName = "char.exr"
        rel orderedVars = [</Render/color>]
    }
    def RenderVar "color"
    {
        string sourceName = "color"
    }
}
"#;

/// render-passes spec, "Each pass renders its own products" and "A pass with
/// no render source": resolution, camera, products and every `crust:`
/// parameter come from the pass's settings, and the provenance names them.
#[test]
fn the_render_source_chooses_the_settings() {
    let path = stage(
        "render_source",
        CAMERAS,
        &[
            pass(
                "char",
                "            rel renderSource = </Render/charSettings>",
            ),
            pass("plain", ""),
        ]
        .concat(),
        CHAR_SETTINGS,
    );
    let char_pass = load(&path, Some("/Render/Passes/char"));
    assert_eq!(char_pass.settings.get_dimensions(), (16, 4));
    assert_eq!(char_pass.settings.samples_per_pixel(), 7);
    assert_eq!(char_pass.camera_path.as_deref(), Some("/World/CharCam"));
    assert_eq!(char_pass.aovs.products.len(), 1);
    assert_eq!(char_pass.aovs.products[0].name, "char.exr");
    let prov = char_pass.provenance.as_ref().expect("imported");
    assert_eq!(prov.render_settings(), Some("/Render/charSettings"));

    for scene in [load(&path, Some("/Render/Passes/plain")), load(&path, None)] {
        assert_eq!(scene.settings.get_dimensions(), (8, 8));
        assert_eq!(scene.settings.samples_per_pixel(), 3);
        assert!(scene.aovs.products.is_empty());
        assert_eq!(
            scene.provenance.as_ref().unwrap().render_settings(),
            Some("/Render/settings")
        );
        assert_eq!(count(&scene, WarningCode::RenderPassRenderSourceInvalid), 0);
    }
}

/// render-passes spec, "A render source that is not settings": a warning
/// names the pass and the target, and the stage's settings are read.
#[test]
fn a_render_source_that_is_not_settings_warns() {
    let path = stage(
        "render_source_camera",
        CAMERAS,
        &pass("char", "            rel renderSource = </World/CharCam>"),
        "",
    );
    let scene = load(&path, Some("/Render/Passes/char"));
    assert_eq!(scene.settings.get_dimensions(), (8, 8));
    let w = scene
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::RenderPassRenderSourceInvalid)
        .expect("warned");
    assert_eq!(w.count, 1);
    assert!(
        w.message.contains("/Render/Passes/char") && w.message.contains("/World/CharCam"),
        "{}",
        w.message
    );
}

// ---------------------------------------------------------------------------
// Collections not read
// ---------------------------------------------------------------------------

/// render-passes spec, "A pass that prunes": one warning per collection, and
/// what it names still renders.
#[test]
fn prune_and_render_visibility_warn_and_are_not_read() {
    let path = stage(
        "prune",
        &ball("Set", 0.0),
        &[
            pass(
                "pruning",
                "            prepend rel collection:prune:includes = </World/Set>",
            ),
            pass(
                "invisible",
                "            prepend rel collection:renderVisibility:excludes = </World/Set>",
            ),
            pass(
                "quiet",
                // Excluding from prune, including in renderVisibility:
                // neither restricts anything.
                "            prepend rel collection:prune:excludes = </World/Set>\n\
                             prepend rel collection:renderVisibility:includes = </World/Set>",
            ),
        ]
        .concat(),
        "",
    );
    for (name, warned) in [("pruning", 1), ("invisible", 1), ("quiet", 0)] {
        let scene = load(&path, Some(&format!("/Render/Passes/{name}")));
        assert_eq!(
            count(&scene, WarningCode::RenderPassCollectionNotRead),
            warned,
            "{name}"
        );
        assert_eq!(
            camera_hit(&scene, 0.0),
            Some(false),
            "{name}: the set renders"
        );
    }
}

// ---------------------------------------------------------------------------
// Membership
// ---------------------------------------------------------------------------

/// render-passes spec, "The nearest path decides", and `explicitOnly`.
#[test]
fn the_nearest_opinion_decides_membership() {
    let body = r#"    def Xform "Props"
    {
        def Xform "Hero"
        {
            def Sphere "Body"
            {
                double radius = 0.5
                double3 xformOp:translate = (-2, 0, 0)
                uniform token[] xformOpOrder = ["xformOp:translate"]
            }
        }
        def Sphere "Crate"
        {
            double radius = 0.5
            double3 xformOp:translate = (2, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
    }
"#;
    let path = stage(
        "nearest",
        body,
        &[
            pass(
                "nearest",
                "            prepend rel collection:matte:includes = </World/Props>\n\
                             prepend rel collection:matte:excludes = </World/Props/Hero>",
            ),
            pass(
                "explicit",
                "            uniform token collection:matte:expansionRule = \"explicitOnly\"\n\
                             prepend rel collection:matte:includes = [</World/Props>, </World/Props/Crate>]",
            ),
        ]
        .concat(),
        "",
    );
    let scene = load(&path, Some("/Render/Passes/nearest"));
    assert_eq!(camera_hit(&scene, -2.0), Some(false), "Hero/Body");
    assert_eq!(camera_hit(&scene, 2.0), Some(true), "Crate");
    // explicitOnly: /World/Props itself, which emits nothing, and the crate
    // named outright; not the hero below the included Xform.
    let scene = load(&path, Some("/Render/Passes/explicit"));
    assert_eq!(camera_hit(&scene, -2.0), Some(false), "Hero/Body");
    assert_eq!(camera_hit(&scene, 2.0), Some(true), "Crate");
}

/// render-passes spec, "A membership expression" — with a predicate the index
/// stage cannot answer, on a mesh behind a payload, in a streamed import
/// (five subtrees): the queries are built on each chunk's stage.
#[test]
fn a_membership_expression_reads_the_chunk_stage() {
    let payload = dir().join("expression_payload.usda");
    std::fs::write(
        &payload,
        r#"#usda 1.0
(
    defaultPrim = "Asset"
)
def Xform "Asset"
{
    def Mesh "Skin"
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-0.5, -0.5, 0), (0.5, -0.5, 0), (0.5, 0.5, 0), (-0.5, 0.5, 0)]
    }
}
"#,
    )
    .unwrap();
    let mut body = String::from(
        r#"    def Xform "Chars"
    {
        def Xform "Hero" (prepend payload = @./expression_payload.usda@)
        {
            double3 xformOp:translate = (-2, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
        def Sphere "Ball"
        {
            double radius = 0.5
        }
    }
"#,
    );
    for (i, x) in [2.0, 4.0, 6.0, 8.0].into_iter().enumerate() {
        body.push_str(&ball(&format!("Prop{i}"), x));
    }
    let path = stage(
        "expression",
        &body,
        &[
            pass(
                "chars",
                "            uniform bool collection:matte:includeRoot = 0\n\
                             uniform pathExpression collection:matte:membershipExpression = \"/World/Chars//\"",
            ),
            pass(
                "meshes",
                "            uniform pathExpression collection:matte:membershipExpression = \"/World//{isa:Mesh}\"",
            ),
        ]
        .concat(),
        "",
    );
    let scene = load(&path, Some("/Render/Passes/chars"));
    assert_eq!(camera_hit(&scene, -2.0), Some(true), "the payload's mesh");
    assert_eq!(camera_hit(&scene, 0.0), Some(true), "Chars/Ball");
    for x in [2.0, 4.0, 6.0, 8.0] {
        assert_eq!(camera_hit(&scene, x), Some(false), "prop at {x}");
    }
    let scene = load(&path, Some("/Render/Passes/meshes"));
    assert_eq!(camera_hit(&scene, -2.0), Some(true), "the mesh");
    assert_eq!(
        camera_hit(&scene, 0.0),
        Some(false),
        "a sphere is not a mesh"
    );
}

/// A pass that takes its `matte` opinion from a payload: only a chunk's stage
/// loads it, so which collections restrict anything is decided there, not on
/// the payload-free index stage.
#[test]
fn a_pass_whose_matte_comes_from_a_payload_holds_out() {
    let payload = dir().join("matte_payload.usda");
    std::fs::write(
        &payload,
        r#"#usda 1.0
(
    defaultPrim = "Pass"
)
def RenderPass "Pass"
{
    uniform bool collection:matte:includeRoot = 1
}
"#,
    )
    .unwrap();
    let mut body = String::new();
    for (i, x) in [0.0, 2.0, 4.0].into_iter().enumerate() {
        body.push_str(&ball(&format!("Prop{i}"), x));
    }
    let path = stage(
        "pass_payload",
        &body,
        "        def RenderPass \"fromPayload\" (prepend payload = @./matte_payload.usda@)\n        {\n        }\n",
        "",
    );
    let scene = load(&path, Some("/Render/Passes/fromPayload"));
    for x in [0.0, 2.0, 4.0] {
        assert_eq!(camera_hit(&scene, x), Some(true), "ball at {x}");
    }
}

/// render-passes spec, "A target inside an instance", and a `PointInstancer`
/// judged on the instancer.
#[test]
fn instances_are_judged_on_the_instance_prim() {
    let body = r#"    class Xform "_Rock"
    {
        def Sphere "Body"
        {
            double radius = 0.5
        }
    }
    def Xform "Rock" (instanceable = true; references = </World/_Rock>)
    {
        double3 xformOp:translate = (-2, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def PointInstancer "Scatter"
    {
        rel prototypes = [</World/Scatter/Protos/Pebble>]
        int[] protoIndices = [0, 0]
        point3f[] positions = [(2, 0, 0), (4, 0, 0)]
        def Scope "Protos"
        {
            def Sphere "Pebble"
            {
                double radius = 0.5
            }
        }
    }
"#;
    let path = stage(
        "instances",
        body,
        &[
            pass(
                "inside",
                "            prepend rel collection:matte:includes = [</World/Rock/Body>, </World/Rock>]",
            ),
            pass(
                "scatter",
                "            prepend rel collection:matte:includes = </World/Scatter>",
            ),
        ]
        .concat(),
        "",
    );
    let scene = load(&path, Some("/Render/Passes/inside"));
    assert_eq!(count(&scene, WarningCode::RenderPassTargetInInstance), 1);
    assert_eq!(camera_hit(&scene, -2.0), Some(true), "the instance");
    assert_eq!(camera_hit(&scene, 2.0), Some(false));

    let scene = load(&path, Some("/Render/Passes/scatter"));
    assert_eq!(count(&scene, WarningCode::RenderPassTargetInInstance), 0);
    assert_eq!(camera_hit(&scene, -2.0), Some(false));
    assert_eq!(camera_hit(&scene, 2.0), Some(true), "first placement");
    assert_eq!(camera_hit(&scene, 4.0), Some(true), "second placement");
    assert_eq!(scene.pass.as_ref().unwrap().holdouts, 1, "one prim");
}

/// render-passes spec, "A held-out volume": one warning naming it, and the
/// region as without the pass.
#[test]
fn a_held_out_volume_warns_and_is_unchanged() {
    let body = r#"    def Cube "Fog"
    {
        token crust:volume:type = "homogeneous"
    }
"#;
    let path = stage(
        "volume",
        body,
        &[
            pass(
                "matte",
                "            prepend rel collection:matte:includes = </World/Fog>",
            ),
            pass(
                "hidden",
                "            prepend rel collection:cameraVisibility:excludes = </World/Fog>",
            ),
        ]
        .concat(),
        "",
    );
    let plain = load(&path, None);
    for name in ["matte", "hidden"] {
        let scene = load(&path, Some(&format!("/Render/Passes/{name}")));
        let w = scene
            .warnings
            .iter()
            .find(|w| w.code == WarningCode::RenderPassVolumeIgnored)
            .unwrap_or_else(|| panic!("{name}: warned"));
        assert_eq!(
            (w.count, w.prims.as_slice()),
            (1, &["/World/Fog".to_owned()][..])
        );
        assert_eq!(scene.volumes.len(), 1);
        assert_eq!(scene.volumes[0].mask, plain.volumes[0].mask, "{name}");
    }
}

/// render-passes spec, "A blocker hidden in one pass only", and a pass never
/// showing what `crust:rayMask` hides.
#[test]
fn camera_visibility_only_removes_camera_rays() {
    let body = [
        ball("Blocker", -2.0),
        r#"    def Sphere "Masked"
    {
        double radius = 0.5
        int crust:rayMask = 6
        double3 xformOp:translate = (2, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
"#
        .to_owned(),
    ]
    .concat();
    let path = stage(
        "camera_visibility",
        &body,
        &pass(
            "pass",
            "            prepend rel collection:cameraVisibility:excludes = </World/Blocker>\n\
                         prepend rel collection:cameraVisibility:includes = </World/Masked>",
        ),
        "",
    );
    let plain = load(&path, None);
    assert_eq!(camera_hit(&plain, -2.0), Some(false));
    let scene = load(&path, Some("/Render/Passes/pass"));
    assert_eq!(
        camera_hit(&scene, -2.0),
        None,
        "the camera passes the blocker"
    );
    assert!(shadow_hit(&scene, -2.0), "it still shadows");
    assert_eq!(camera_hit(&scene, 2.0), None, "rayMask 6 stays hidden");
    assert!(shadow_hit(&scene, 2.0));
    assert_eq!(scene.pass.as_ref().unwrap().camera_hidden, 1);
}

// ---------------------------------------------------------------------------
// Lights
// ---------------------------------------------------------------------------

/// render-passes spec, "A pass cannot reveal a hidden light" and "An area
/// light hidden by the pass stays solid".
#[test]
fn a_pass_hides_area_lights_from_the_camera_only() {
    let body = r#"    def SphereLight "Bulb"
    {
        float inputs:radius = 0.5
        double3 xformOp:translate = (-2, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def RectLight "Panel"
    {
        bool crust:light:cameraVisible = true
        float inputs:width = 1
        float inputs:height = 1
        double3 xformOp:translate = (2, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
"#;
    let path = stage(
        "area_lights",
        body,
        &pass(
            "pass",
            "            prepend rel collection:cameraVisibility:includes = </World/Bulb>\n\
                         prepend rel collection:cameraVisibility:excludes = </World/Panel>",
        ),
        "",
    );
    let plain = load(&path, None);
    assert_eq!(camera_hit(&plain, -2.0), None, "hidden by default");
    assert_eq!(camera_hit(&plain, 2.0), Some(false), "camera-visible");
    let panel_occludes = shadow_hit(&plain, 2.0);

    let scene = load(&path, Some("/Render/Passes/pass"));
    assert_eq!(camera_hit(&scene, -2.0), None, "still hidden");
    assert_eq!(camera_hit(&scene, 2.0), None, "hidden by the pass");
    assert_eq!(
        shadow_hit(&scene, 2.0),
        panel_occludes,
        "occludes as before"
    );
    assert_eq!(scene.lights.count(), plain.lights.count());
}

/// The mean of `n` camera paths along `ray` through `scene`.
fn radiance(scene: &Scene, ray: &Ray, n: i32) -> Vec<[u32; 3]> {
    (0..n)
        .map(|i| {
            ray_color(
                ray,
                &scene.world,
                &scene.lights,
                &Volumes::default(),
                4,
                SamplingStrategy::PowerMis,
                PathSampler::new(2, 2, 0, i),
            )
            .to_array()
            .map(f32::to_bits)
        })
        .collect()
}

const FLOOR: &str = r#"    def Mesh "Floor"
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-5, -1, -5), (5, -1, -5), (5, -1, 5), (-5, -1, 5)]
    }
"#;

/// render-passes spec, "The dome is held out of a character pass": escaping
/// camera rays see nothing of it, and surfaces are lit exactly as before.
#[test]
fn a_matte_dome_is_hidden_from_the_camera_and_lights_as_before() {
    let body = format!(
        r#"{FLOOR}    def DomeLight "Sky"
    {{
        float inputs:intensity = 2
    }}
"#
    );
    let path = stage(
        "matte_dome",
        &body,
        &pass(
            "pass",
            "            prepend rel collection:matte:includes = </World/Sky>",
        ),
        "",
    );
    let plain = load(&path, None);
    let scene = load(&path, Some("/Render/Passes/pass"));
    let up = Ray::new(Vec3A::ZERO, Vec3A::Y).with_mask(MASK_CAMERA);
    assert!(radiance(&plain, &up, 4).iter().any(|c| *c != [0; 3]));
    assert!(radiance(&scene, &up, 4).iter().all(|c| *c == [0; 3]));
    let down = Ray::new(Vec3A::ZERO, -Vec3A::Y).with_mask(MASK_CAMERA);
    let lit = radiance(&scene, &down, 32);
    assert!(
        lit == radiance(&plain, &down, 32),
        "lit as without the pass"
    );
    assert!(lit.iter().any(|c| *c != [0; 3]));
}

/// render-passes spec, "A backdrop dome hidden by the pass": camera
/// visibility is decided before light linking demotes a dome linked to
/// nothing, so it is dropped rather than kept as a backdrop.
#[test]
fn camera_visibility_is_decided_before_light_linking() {
    let body = format!(
        r#"{FLOOR}    def DomeLight "Backdrop"
    {{
        float inputs:intensity = 2
        uniform bool collection:lightLink:includeRoot = 0
    }}
"#
    );
    let path = stage(
        "backdrop",
        &body,
        &pass(
            "pass",
            "            prepend rel collection:cameraVisibility:excludes = </World/Backdrop>",
        ),
        "",
    );
    let up = Ray::new(Vec3A::ZERO, Vec3A::Y).with_mask(MASK_CAMERA);
    let plain = load(&path, None);
    assert_eq!(plain.lights.backdrops().len(), 1, "a camera-only backdrop");
    assert!(radiance(&plain, &up, 4).iter().any(|c| *c != [0; 3]));
    let scene = load(&path, Some("/Render/Passes/pass"));
    assert!(scene.lights.backdrops().is_empty(), "dropped");
    assert!(radiance(&scene, &up, 4).iter().all(|c| *c == [0; 3]));
}

// ---------------------------------------------------------------------------
// Compositing
// ---------------------------------------------------------------------------

/// `samples/render_passes.usda` rendered with `pass` (or none) at `spp`,
/// small, unclamped and without adaptive sampling: the beauty and alpha per
/// pixel.
fn render_sample(pass: Option<&str>, spp: u32) -> Vec<[f32; 4]> {
    use crust_core::{AovProduct, AovRequest, AovSource, AovVar, Precision, Renderer};
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/render_passes.usda");
    let scene = load(&path, pass);
    let (w, h) = (48, 27);
    let settings = scene
        .settings
        .with_resolution(w, h)
        .with_samples_per_pixel(spp)
        .with_adaptive_sampling(spp, 0.0)
        .with_indirect_clamp(0.0);
    let alpha = AovVar {
        prim_path: "/Render/Vars/a".into(),
        name: "a".into(),
        channel_prefix: None,
        source: AovSource::Alpha,
        components: 1,
        precision: Precision::Float,
        accumulation: AovSource::Alpha.default_accumulation(),
        clear: 0.0,
        expression: None,
        raw: false,
        variance: false,
    };
    let request = AovRequest {
        products: vec![AovProduct {
            prim_path: "/Render/p".into(),
            name: "p.exr".into(),
            vars: vec![alpha.clone()],
            attributes: Vec::new(),
        }],
    };
    let r = Renderer::new(scene.camera, scene.world, scene.lights, settings)
        .with_volumes(scene.volumes);
    let (beauty, film, _) = r.render_with_aovs(true, &|_, _| {}, &request);
    let a = film.var_channels(&beauty, &alpha).remove(0);
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let c = beauty.get_pixel(x, y);
            [c.x, c.y, c.z, a[y * w + x]]
        })
        .collect()
}

/// RMS difference of the composite of `fg` and `bg` from `whole`, over the
/// colour channels: `plus` adds the passes, `over` is `fg + (1 − αfg)·bg`.
fn composite_error(fg: &[[f32; 4]], bg: &[[f32; 4]], whole: &[[f32; 4]], over: bool) -> f64 {
    let sum: f64 = fg
        .iter()
        .zip(bg)
        .zip(whole)
        .map(|((f, b), w)| {
            let k = if over { 1.0 - f[3] } else { 1.0 };
            (0..3)
                .map(|c| (f[c] + k * b[c] - w[c]) as f64)
                .map(|d| d * d)
                .sum::<f64>()
        })
        .sum();
    (sum / (fg.len() * 3) as f64).sqrt()
}

/// Each sample of the frame lands in exactly one of two passes that hold each
/// other out, so their beauties add up to the render with no pass active,
/// edge pixels included. `over` does not: at an edge it takes the
/// foreground's share away from the background a second time, a bias that
/// does not fall with the sample count.
#[test]
fn passes_that_hold_each_other_out_add_up_to_the_whole() {
    let mut over = Vec::new();
    for spp in [16, 64] {
        let ch = render_sample(Some("/Render/Passes/char"), spp);
        let props = render_sample(Some("/Render/Passes/props"), spp);
        let whole = render_sample(None, spp);
        let plus = composite_error(&ch, &props, &whole, false);
        assert!(plus < 1e-6, "{spp} spp: plus {plus}");
        over.push(composite_error(&ch, &props, &whole, true));
    }
    // Bias, not noise: four times the samples leaves it where it was.
    assert!(over[1] > 0.5 * over[0] && over[0] > 1e-3, "over: {over:?}");
}
