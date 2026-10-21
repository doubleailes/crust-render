//! Time-sampled transforms over the camera's shutter: which geometry moves,
//! from where to where, what the beauty and the motion record make of it, and
//! the warnings for motion that is not produced.

use crust_core::{Buffer, NoAssets, Ray, Renderer, Scene, UsdImportOptions, Vec3A, WarningCode};
use std::path::PathBuf;

const W: usize = 64;
const H: usize = 36;

/// A track along X at one unit per frame: x = -0.5 at frame 5 and 0.5 at
/// frame 6.
const TRACK: &str = r#"double3 xformOp:translate.timeSamples = {
            1: (-4.5, 0, 0),
            10: (4.5, 0, 0),
        }
        uniform token[] xformOpOrder = ["xformOp:translate"]"#;

/// An emissive square of half-width 0.3 in the XY plane, with `ops`.
fn card(name: &str, ops: &str) -> String {
    format!(
        r#"    def Mesh "{name}" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-0.3, -0.3, 0), (0.3, -0.3, 0), (0.3, 0.3, 0), (-0.3, 0.3, 0)]
        rel material:binding = </World/Glow>
        {ops}
    }}
"#
    )
}

/// An emissive sphere of radius 0.3, with `ops`.
fn ball(name: &str, ops: &str) -> String {
    format!(
        r#"    def Sphere "{name}" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        double radius = 0.3
        rel material:binding = </World/Glow>
        {ops}
    }}
"#
    )
}

/// A stage seen by a default camera at z = 8 looking down −Z, the camera
/// authoring `shutter`, with `body` under `/World`, black everywhere but the
/// emitters, and `settings` on the render settings. At this distance one
/// world unit is about 19 pixels.
fn stage(shutter: &str, body: &str, settings: &str) -> String {
    // A prototype is authored as a top-level `class`, pulled out of `body`.
    let (body, root) = match body.split_once("<<class>>") {
        Some((body, root)) => (body, root),
        None => (body, ""),
    };
    format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
    startTimeCode = 1
    endTimeCode = 10
)

def Xform "World"
{{
    def Camera "cam"
    {{
        double3 xformOp:translate = (0, 0, 8)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        {shutter}
    }}
    def Material "Glow"
    {{
        token outputs:surface.connect = </World/Glow/S.outputs:surface>
        def Shader "S"
        {{
            uniform token info:id = "crust:openpbr"
            float inputs:emissionLuminance = 3
            color3f inputs:emissionColor = (1, 1, 1)
            token outputs:surface
        }}
    }}
{body}
}}
{root}
def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/cam>
        int2 resolution = ({W}, {H})
        int crust:samplesPerPixel = 16
        int crust:minSamplesPerPixel = 16
        int crust:maxDepth = 2
{settings}
    }}
}}
"#
    )
}

const SHUTTER: &str = "double shutter:open = 0\n        double shutter:close = 1";

fn write(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_time_motion_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, text).expect("write stage");
    path
}

fn load_at(name: &str, text: &str, frame: Option<f64>) -> Scene {
    let path = write(name, text);
    let options = UsdImportOptions {
        frame,
        ..Default::default()
    };
    Scene::from_usd_with_options(&path, &NoAssets, &options)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn load(name: &str, text: &str) -> Scene {
    load_at(name, text, Some(5.0))
}

/// The geometry a ray down −Z through `(x, y)` meets at shutter time `t`, and
/// where.
fn probe(scene: &Scene, x: f32, y: f32, t: f32) -> Option<(u32, Vec3A)> {
    let ray = Ray::new(Vec3A::new(x, y, 5.0), -Vec3A::Z).with_time(t);
    scene
        .world
        .intersect(&ray, 1e-3, 100.0)
        .map(|h| (h.geom_id, h.rec.p))
}

/// The world-space displacement over the shutter the motion record gives the
/// point hit through `(x, y)` at shutter time `t`.
fn motion(scene: &Scene, x: f32, y: f32, t: f32) -> Vec3A {
    let (id, p) = probe(scene, x, y, t).unwrap_or_else(|| panic!("nothing at ({x}, {y}) at {t}"));
    scene.world.motion(id, p, t)
}

fn near(a: Vec3A, b: Vec3A) -> bool {
    (a - b).abs().max_element() < 1e-4
}

fn render(scene: Scene) -> Buffer {
    Renderer::new(scene.camera, scene.world, scene.lights, scene.settings).render_with_tiles()
}

fn lit(b: &Buffer, x: usize, y: usize) -> bool {
    b.get_pixel(x, y).max_element() > 0.0
}

fn bits(b: &Buffer) -> Vec<[u32; 3]> {
    (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .map(|(x, y)| {
            let c = b.get_pixel(x, y);
            [c.x.to_bits(), c.y.to_bits(), c.z.to_bits()]
        })
        .collect()
}

fn warned(scene: &Scene, code: WarningCode) -> u64 {
    scene
        .warnings
        .iter()
        .find(|w| w.code == code)
        .map_or(0, |w| w.count)
}

/// The codes this change raises, none of which a stage without time-sampled
/// motion should see.
const MOTION_CODES: [WarningCode; 4] = [
    WarningCode::XformMotionIgnored,
    WarningCode::TimeMotionNeedsFrame,
    WarningCode::CameraShutterInstantaneous,
    WarningCode::CameraShutterMismatch,
];

// ---------------------------------------------------------------------------
// Placements move from shutter open to shutter close
// ---------------------------------------------------------------------------

/// A mesh's time-sampled translate at `-f 5` over a shutter of one frame:
/// at shutter open it is where frame 5 puts it, at shutter close where frame
/// 6 does, and the record says it moves one unit along X.
#[test]
fn a_time_sampled_translate_moves_a_mesh_over_the_shutter() {
    let scene = load("mesh_track", &stage(SHUTTER, &card("Card", TRACK), ""));
    assert!(scene.world.has_motion());
    assert!(probe(&scene, -0.5, 0.0, 0.0).is_some());
    assert!(probe(&scene, -0.5, 0.0, 1.0).is_none());
    assert!(probe(&scene, 0.5, 0.0, 1.0).is_some());
    assert!(near(motion(&scene, -0.5, 0.0, 0.0), Vec3A::X));
    for code in MOTION_CODES {
        assert_eq!(warned(&scene, code), 0, "{code:?}");
    }
}

/// The beauty is streaked along the path; with blur off it is sharp at the
/// shutter-open position (centred about column 22), the close one (about
/// column 41) dark.
#[test]
fn an_animated_mesh_is_blurred_and_sharp_at_shutter_open_with_blur_off() {
    let blurred = render(load(
        "mesh_blurred",
        &stage(SHUTTER, &card("Card", TRACK), ""),
    ));
    // Between the two silhouettes, where only the streak reaches.
    assert!(lit(&blurred, 32, H / 2), "the streak's middle is lit");
    let sharp = render(load(
        "mesh_sharp",
        &stage(
            SHUTTER,
            &card("Card", TRACK),
            "        uniform bool disableMotionBlur = 1",
        ),
    ));
    assert!(lit(&sharp, 22, H / 2), "the shutter-open silhouette is lit");
    for y in 0..H {
        for x in 30..50 {
            assert!(!lit(&sharp, x, y), "pixel ({x}, {y}) lit with blur off");
        }
    }
}

/// `crust:motion:translate` on a static mesh still moves it by `v`, with no
/// shutter authored at all.
#[test]
fn an_authored_translation_still_moves_a_static_mesh_without_a_shutter() {
    let ops = "float3 crust:motion:translate = (1, 0, 0)\n        \
               double3 xformOp:translate = (-0.5, 0, 0)\n        \
               uniform token[] xformOpOrder = [\"xformOp:translate\"]";
    let scene = load("authored_only", &stage("", &card("Card", ops), ""));
    assert!(probe(&scene, 0.5, 0.0, 1.0).is_some());
    assert_eq!(motion(&scene, -0.5, 0.0, 0.0), Vec3A::X);
}

/// A sphere on the same track, and one that also authors a translation:
/// its streak ends at the sum of the two motions.
#[test]
fn a_sphere_moves_with_its_transform_and_its_authored_translation() {
    let body = ball("Ball", TRACK);
    let scene = load("sphere_track", &stage(SHUTTER, &body, ""));
    assert!(probe(&scene, -0.5, 0.0, 0.0).is_some());
    assert!(probe(&scene, -0.5, 0.0, 1.0).is_none());
    assert!(probe(&scene, 0.5, 0.0, 1.0).is_some());
    assert!(near(motion(&scene, -0.5, 0.0, 0.0), Vec3A::X));

    let both = format!("float3 crust:motion:translate = (0, 1, 0)\n        {TRACK}");
    let scene = load("sphere_both", &stage(SHUTTER, &ball("Ball", &both), ""));
    assert!(
        probe(&scene, 0.5, 1.0, 1.0).is_some(),
        "the streak ends at (0.5, 1)"
    );
    assert!(probe(&scene, 0.5, 0.0, 1.0).is_none());
    assert!(near(
        motion(&scene, -0.5, 0.0, 0.0),
        Vec3A::new(1.0, 1.0, 0.0)
    ));
}

/// A mesh that also authors a translation moves each point by the sum.
#[test]
fn an_authored_translation_adds_to_an_animated_mesh() {
    let ops = format!("float3 crust:motion:translate = (0, 1, 0)\n        {TRACK}");
    let scene = load("mesh_both", &stage(SHUTTER, &card("Card", &ops), ""));
    assert!(near(
        motion(&scene, -0.5, 0.0, 0.0),
        Vec3A::new(1.0, 1.0, 0.0)
    ));
    assert!(probe(&scene, 0.5, 1.0, 1.0).is_some());
}

/// A static mesh under an `Xform` whose rotation is time-sampled moves with
/// it: about Y by 10° a frame, a card 2 units out along X swings back.
#[test]
fn a_static_mesh_moves_with_an_animated_ancestor() {
    let body = format!(
        r#"    def Xform "Turn"
    {{
        float xformOp:rotateY.timeSamples = {{
            1: 0,
            10: 90,
        }}
        uniform token[] xformOpOrder = ["xformOp:rotateY"]
{}    }}
"#,
        card(
            "Card",
            "double3 xformOp:translate = (2, 0, 0)\n        \
             uniform token[] xformOpOrder = [\"xformOp:translate\"]"
        )
    );
    let scene = load("ancestor", &stage(SHUTTER, &body, ""));
    assert!(scene.world.has_motion());
    // At frame 5 the card is at 40°: its centre at x = 2 cos 40°.
    let x5 = 2.0 * 40f32.to_radians().cos();
    let x6 = 2.0 * 50f32.to_radians().cos();
    let v = motion(&scene, x5, 0.0, 0.0);
    assert!(
        near(
            v,
            Vec3A::new(
                x6 - x5,
                0.0,
                2.0 * 40f32.to_radians().sin() - 2.0 * 50f32.to_radians().sin()
            )
        ),
        "{v}"
    );
}

/// Two instances of a two-mesh prototype, one with a time-sampled
/// transform: both of its meshes move, neither of the other's does.
#[test]
fn only_the_animated_instance_of_a_prototype_moves() {
    let body = format!(
        r#"    def Xform "Still" (instanceable = true prepend references = </Proto>)
    {{
        double3 xformOp:translate = (0, 1.5, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def Xform "Moving" (instanceable = true prepend references = </Proto>)
    {{
        {TRACK}
    }}
<<class>>class Xform "Proto"
{{
{}{}}}
"#,
        card(
            "Low",
            "double3 xformOp:translate = (0, -0.4, 0)\n        \
             uniform token[] xformOpOrder = [\"xformOp:translate\"]"
        ),
        card(
            "High",
            "double3 xformOp:translate = (0, 0.4, 0)\n        \
             uniform token[] xformOpOrder = [\"xformOp:translate\"]"
        ),
    );
    let scene = load("native", &stage(SHUTTER, &body, ""));
    for y in [-0.4, 0.4] {
        assert!(
            near(motion(&scene, -0.5, y, 0.0), Vec3A::X),
            "moving part at y = {y}"
        );
        assert!(probe(&scene, 0.5, y, 1.0).is_some());
        assert_eq!(
            motion(&scene, 0.0, 1.5 + y, 0.0),
            Vec3A::ZERO,
            "still part at y = {y}"
        );
        assert!(probe(&scene, 0.0, 1.5 + y, 1.0).is_some());
    }
}

/// A `PointInstancer` whose own translate is time-sampled: every instance
/// moves by the same world displacement.
#[test]
fn every_placement_of_an_animated_point_instancer_moves() {
    let body = format!(
        r#"    def PointInstancer "Scatter"
    {{
        rel prototypes = [</World/Scatter/Protos/Ball>]
        int[] protoIndices = [0, 0, 0]
        point3f[] positions = [(0, -1, 0), (0, 0, 0), (0, 1, 0)]
        {TRACK}
        def Scope "Protos"
        {{
{}        }}
    }}
"#,
        ball("Ball", "")
    );
    let scene = load("instancer", &stage(SHUTTER, &body, ""));
    for y in [-1.0, 0.0, 1.0] {
        assert!(
            near(motion(&scene, -0.5, y, 0.0), Vec3A::X),
            "instance at y = {y}"
        );
        assert!(probe(&scene, 0.5, y, 1.0).is_some());
        assert!(probe(&scene, -0.5, y, 1.0).is_none());
    }
}

/// A shutter opening before the frame with blur off. `disableMotionBlur` is
/// UsdRender's `[0, 0]` shutter at the current time code: the sharp image is
/// at frame 5 itself, and the vector is the shutter's half-frame
/// displacement measured from there. `instantaneousShutter` alone keeps the
/// sharp image at shutter open, time code 4.75; with both, `disableMotionBlur`
/// wins.
#[test]
fn blur_off_is_sharp_at_the_frame_and_instantaneous_at_shutter_open() {
    let centred = "double shutter:open = -0.25\n        double shutter:close = 0.25";
    let sharp = |name: &str, settings: &str| {
        let scene = load(name, &stage(centred, &ball("Ball", TRACK), settings));
        assert!(!scene.settings.motion_blur(), "{name}");
        scene
    };
    // Centred at `x`: the ray through it meets the ball's front at z = 0.3,
    // its radius; one a quarter unit off would meet it at z ≈ 0.17.
    let at_x = |scene: &Scene, x: f32| {
        let (_, p) = probe(scene, x, 0.0, 0.0).unwrap_or_else(|| panic!("hit at x = {x}"));
        assert!((p.z - 0.3).abs() < 1e-3, "centred at {x}: {p}");
        assert!(near(motion(scene, x, 0.0, 0.0), Vec3A::new(0.5, 0.0, 0.0)));
    };

    let frame = sharp(
        "centred_frame",
        "        uniform bool disableMotionBlur = 1",
    );
    at_x(&frame, -0.5);

    let open = sharp(
        "centred_open",
        "        uniform bool instantaneousShutter = 1",
    );
    at_x(&open, -0.75);

    let both = sharp(
        "centred_both",
        "        uniform bool disableMotionBlur = 1\n        uniform bool instantaneousShutter = 1",
    );
    at_x(&both, -0.5);
}

/// A static stage renders exactly as it would with an instantaneous
/// shutter, and so does an animated one with no shutter authored.
#[test]
fn static_prims_and_unauthored_shutters_are_unchanged() {
    let still = card(
        "Card",
        "double3 xformOp:translate = (-0.5, 0, 0)\n        \
         uniform token[] xformOpOrder = [\"xformOp:translate\"]",
    );
    let with = load("static_with", &stage(SHUTTER, &still, ""));
    let without = load("static_without", &stage("", &still, ""));
    assert!(!with.world.has_motion());
    assert_eq!(bits(&render(with)), bits(&render(without)));

    let animated = load("animated_no_shutter", &stage("", &card("Card", TRACK), ""));
    assert!(!animated.world.has_motion());
    assert!(
        probe(&animated, -0.5, 0.0, 1.0).is_some(),
        "sharp at frame 5"
    );
    for code in MOTION_CODES {
        assert_eq!(warned(&animated, code), 0, "{code:?}");
    }
}

// ---------------------------------------------------------------------------
// Motion that is not produced is reported, once per cause
// ---------------------------------------------------------------------------

/// The shutter opens but no frame is given: one warning, and the mesh
/// renders sharp.
#[test]
fn a_shutter_without_a_frame_warns_and_moves_nothing() {
    let shutter = "double shutter:close = 0.5";
    let body = format!("{}{}", card("A", TRACK), card("B", TRACK));
    let scene = load_at("no_frame", &stage(shutter, &body, ""), None);
    assert!(!scene.world.has_motion());
    assert_eq!(warned(&scene, WarningCode::TimeMotionNeedsFrame), 1);
    let w = scene
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::TimeMotionNeedsFrame)
        .unwrap();
    assert!(w.message.contains("2 drawn prims'"), "{}", w.message);
}

/// Each kind that gets no motion, alone on a stage: one `xform.motion_ignored`
/// each, and nothing when the shutter is instantaneous.
#[test]
fn time_varying_transforms_that_get_no_motion_are_counted() {
    let curves = format!(
        r#"    def BasisCurves "Hair"
    {{
        uniform token type = "linear"
        int[] curveVertexCounts = [2]
        point3f[] points = [(0, -0.5, 0), (0, 0.5, 0)]
        float[] widths = [0.1] (interpolation = "constant")
        {TRACK}
    }}
"#
    );
    let light = format!(
        r#"    def SphereLight "Lamp"
    {{
        float inputs:radius = 0.1
        {TRACK}
    }}
"#
    );
    let volume = format!(
        r#"    def Cube "Fog"
    {{
        token crust:volume:type = "homogeneous"
        color3f crust:volume:sigmaS = (0.1, 0.1, 0.1)
        {TRACK}
    }}
"#
    );
    let prototype = format!(
        r#"    def Xform "Copy" (instanceable = true prepend references = </Proto>)
    {{
    }}
<<class>>class Xform "Proto"
{{
{}}}
"#,
        card("Spin", TRACK)
    );
    for (name, body) in [
        ("curves", curves),
        ("light", light),
        ("volume", volume),
        ("prototype", prototype),
    ] {
        let scene = load(&format!("ignored_{name}"), &stage(SHUTTER, &body, ""));
        assert_eq!(
            scene
                .warnings
                .iter()
                .filter(|w| w.code == WarningCode::XformMotionIgnored)
                .map(|w| w.count)
                .collect::<Vec<_>>(),
            [1],
            "{name}"
        );
        let quiet = load(&format!("quiet_{name}"), &stage("", &body, ""));
        for code in MOTION_CODES {
            assert_eq!(warned(&quiet, code), 0, "{name}: {code:?}");
        }
    }
}

/// No camera named, and the traversal settles on a camera in a payload the
/// pre-read could not see: one warning naming both, when their shutters
/// differ.
#[test]
fn a_render_camera_other_than_the_one_whose_shutter_was_read_is_reported() {
    write(
        "mismatch_shot",
        "#usda 1.0\ndef Xform \"Shot\"\n{\n    def Camera \"Cam\"\n    {\n        \
         double shutter:close = 0.5\n        double3 xformOp:translate = (0, 0, 8)\n        \
         uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    }\n}\n",
    );
    let text = |close: &str| {
        format!(
            "#usda 1.0\ndef Xform \"World\"\n{{\n    def Camera \"Cam\"\n    {{\n        \
             double shutter:close = {close}\n    }}\n    def Xform \"Shot\" (\n        \
             prepend payload = @mismatch_shot.usda@</Shot>\n    )\n    {{\n    }}\n}}\n"
        )
    };
    let scene = load("mismatch", &text("1"));
    assert_eq!(scene.camera_path.as_deref(), Some("/World/Shot/Cam"));
    assert_eq!(warned(&scene, WarningCode::CameraShutterMismatch), 1);
    let w = scene
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::CameraShutterMismatch)
        .unwrap();
    assert!(
        w.message.contains("/World/Shot/Cam") && w.message.contains("/World/Cam"),
        "{}",
        w.message
    );
    // The same shutter on both: nothing to report.
    let scene = load("mismatch_same", &text("0.5"));
    assert_eq!(warned(&scene, WarningCode::CameraShutterMismatch), 0);
}

// ---------------------------------------------------------------------------
// The instantaneous-shutter warning
// ---------------------------------------------------------------------------

/// A product with a `forward` motion vector var, over `stage`'s own layer.
fn with_forward_var(name: &str, sublayer: &str, camera: &str) -> String {
    format!(
        r#"#usda 1.0
(
    subLayers = [@{sublayer}@]
)

over "Render"
{{
    over "settings"
    {{
        rel camera = <{camera}>
        rel products = [</Render/comp>]
    }}
    def RenderProduct "comp"
    {{
        token productName = "{name}.exr"
        rel orderedVars = [</Render/comp/C>, </Render/comp/forward>]
        def RenderVar "C"
        {{
            uniform token dataType = "color3f"
            uniform string sourceName = "color"
        }}
        def RenderVar "forward"
        {{
            uniform token dataType = "float2"
            uniform string sourceName = "motionvector"
        }}
    }}
}}
"#
    )
}

fn sample(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
        .to_str()
        .unwrap()
        .to_owned()
}

/// `samples/animation.usda` at frame 5 with a `forward` var and no shutter:
/// one warning, and the moving sphere's vector is zero.
#[test]
fn motion_vectors_over_an_instantaneous_shutter_warn_once() {
    let text = with_forward_var("instant", &sample("animation.usda"), "/World/Cam");
    let scene = load("instantaneous", &text);
    assert_eq!(warned(&scene, WarningCode::CameraShutterInstantaneous), 1);
    assert!(!scene.world.has_motion());
}

/// A static stage asking for the same var: no warning. A sphere authoring
/// `crust:motion:translate` still has its vector.
#[test]
fn a_static_stage_asking_for_motion_vectors_is_not_warned_about() {
    let base = write(
        "static_vectors_base",
        &stage(
            "",
            &ball(
                "Ball",
                "float3 crust:motion:translate = (1, 0, 0)\n        \
                 double3 xformOp:translate = (-0.5, 0, 0)\n        \
                 uniform token[] xformOpOrder = [\"xformOp:translate\"]",
            ),
            "",
        ),
    );
    let text = with_forward_var("static", base.to_str().unwrap(), "/World/cam");
    let scene = load("static_vectors", &text);
    assert!(scene.aovs.asks_for(crust_core::AovSource::MotionVector));
    assert_eq!(warned(&scene, WarningCode::CameraShutterInstantaneous), 0);
    assert_eq!(motion(&scene, -0.5, 0.0, 0.0), Vec3A::X);
}
