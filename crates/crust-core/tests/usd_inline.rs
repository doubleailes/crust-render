//! USD import driven by small stages written on the fly, one feature per
//! test: prim types, transforms, render settings, materials, lights,
//! volumes, masks, subdivision and error handling. Complements
//! `usd_scene.rs`, which loads the checked-in sample files.

use crust_core::{
    Light, MASK_CAMERA, NoAssets, Ray, SamplingStrategy, Scene, UsdImportOptions, Vec3A,
};
use std::path::PathBuf;

/// Writes `body` (the prims under `/World`) into a fresh `.usda` and loads it.
fn load(name: &str, body: &str) -> Scene {
    load_raw(
        name,
        &format!(
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
        ),
    )
}

/// Like `load`, with a root-level `/Render/settings` prim: the importer
/// reads `UsdRenderSettings` from the stage metadata or that conventional
/// path, never from an arbitrary location under `/World`.
fn load_with_settings(name: &str, body: &str, settings: &str) -> Scene {
    load_raw(
        name,
        &format!(
            r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
{body}
}}
{settings}
"#
        ),
    )
}

/// Like `load`, refining subdivision surfaces at `level` through the host
/// option (the CLI's `--subdiv-level`) — no settings prim, so the stage's
/// shape (and whether it streams) is exactly `load`'s.
fn load_at_level(name: &str, body: &str, level: u32) -> Scene {
    let path = write_stage(
        name,
        &format!(
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
        ),
    );
    let options = UsdImportOptions {
        subdivision_level: Some(level),
        ..UsdImportOptions::default()
    };
    Scene::from_usd_with_options(&path, &NoAssets, &options)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn load_raw(name: &str, text: &str) -> Scene {
    let path = write_stage(name, text);
    Scene::from_usd(&path).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn write_stage(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_usd_inline_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, text).expect("write stage");
    path
}

fn hits(scene: &Scene, origin: Vec3A, dir: Vec3A) -> bool {
    scene
        .world
        .intersect(&Ray::new(origin, dir), 1e-3, 1e4)
        .is_some()
}

fn hit_t(scene: &Scene, origin: Vec3A, dir: Vec3A) -> Option<f32> {
    scene
        .world
        .intersect(&Ray::new(origin, dir), 1e-3, 1e4)
        .map(|h| h.rec.t)
}

const SETTINGS: &str = r#"
def Scope "Render"
{
    def RenderSettings "settings"
    {
        int2 resolution = (32, 16)
        int crust:samplesPerPixel = 7
        int crust:maxDepth = 3
        int crust:minSamplesPerPixel = 2
        float crust:varianceThreshold = 0.1
        int crust:frame = 4
        token crust:samplingStrategy = "balance"
        token crust:pixelFilter = "gaussian"
        float crust:pixelFilterRadius = 2.5
    }
}
"#;

// ---------------------------------------------------------------------------
// Prims and geometry
// ---------------------------------------------------------------------------

#[test]
fn an_empty_stage_loads_with_defaults() {
    let scene = load("empty", "");
    assert_eq!(scene.world.count(), 0);
    assert_eq!(scene.lights.count(), 0);
    assert!(scene.volumes.is_empty());
    assert_eq!(scene.settings.get_dimensions(), (640, 360));
    assert_eq!(scene.settings.samples_per_pixel(), 128);
    assert_eq!(scene.settings.max_depth(), 32);
    assert_eq!(
        scene.settings.sampling_strategy(),
        SamplingStrategy::PowerMis
    );
    assert!(scene.world.bounds().is_none());
}

#[test]
fn a_sphere_prim_becomes_analytic_geometry() {
    let scene = load(
        "sphere",
        r#"
    def Sphere "Ball"
    {
        double radius = 2
        double3 xformOp:translate = (1, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#,
    );
    assert_eq!(scene.world.count(), 1);
    assert_eq!(scene.world.primitive_breakdown().spheres, 1);
    let t = hit_t(&scene, Vec3A::new(1.0, 0.0, 10.0), -Vec3A::Z).expect("hit");
    assert!((t - 8.0).abs() < 1e-4, "radius 2 at x = 1: t = {t}");
    assert!(!hits(&scene, Vec3A::new(3.5, 0.0, 10.0), -Vec3A::Z));
    let bb = scene.world.bounds().unwrap();
    assert!((bb.minimum.x + 1.0).abs() < 1e-4 && (bb.maximum.x - 3.0).abs() < 1e-4);
}

#[test]
fn sphere_radius_defaults_to_one_and_accepts_float() {
    let scene = load(
        "sphere_defaults",
        r#"
    def Sphere "Unit" {}
    def Sphere "Small"
    {
        float radius = 0.25
        double3 xformOp:translate = (5, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#,
    );
    assert_eq!(scene.world.count(), 2);
    assert!(hits(&scene, Vec3A::new(0.9, 0.0, 10.0), -Vec3A::Z));
    assert!(!hits(&scene, Vec3A::new(1.1, 0.0, 10.0), -Vec3A::Z));
    assert!(hits(&scene, Vec3A::new(5.2, 0.0, 10.0), -Vec3A::Z));
    assert!(!hits(&scene, Vec3A::new(5.3, 0.0, 10.0), -Vec3A::Z));
}

#[test]
fn a_quad_mesh_is_fan_triangulated() {
    let scene = load(
        "quad",
        r#"
    def Mesh "Quad"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)]
    }"#,
    );
    assert_eq!(scene.world.count(), 1);
    assert_eq!(scene.world.primitive_breakdown().triangles, 2);
    assert!(hits(&scene, Vec3A::new(0.5, 0.5, 5.0), -Vec3A::Z));
    assert!(hits(&scene, Vec3A::new(-0.5, -0.5, 5.0), -Vec3A::Z));
    assert!(!hits(&scene, Vec3A::new(1.5, 0.0, 5.0), -Vec3A::Z));
}

#[test]
fn ngons_and_mixed_face_counts_triangulate_to_n_minus_two() {
    let scene = load(
        "ngon",
        r#"
    def Mesh "Mixed"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [5, 3, 4]
        int[] faceVertexIndices = [0, 1, 2, 3, 4,  5, 6, 7,  8, 9, 10, 11]
        point3f[] points = [
            (0, 0, 0), (1, 0, 0), (1.5, 1, 0), (0.5, 1.6, 0), (-0.5, 1, 0),
            (3, 0, 0), (4, 0, 0), (3.5, 1, 0),
            (6, 0, 0), (7, 0, 0), (7, 1, 0), (6, 1, 0)
        ]
    }"#,
    );
    // 3 + 1 + 2
    assert_eq!(scene.world.primitive_breakdown().triangles, 6);
    assert!(
        hits(&scene, Vec3A::new(0.5, 0.7, 5.0), -Vec3A::Z),
        "inside the pentagon"
    );
    assert!(
        hits(&scene, Vec3A::new(3.5, 0.3, 5.0), -Vec3A::Z),
        "inside the triangle"
    );
    assert!(
        hits(&scene, Vec3A::new(6.5, 0.5, 5.0), -Vec3A::Z),
        "inside the quad"
    );
    assert!(
        !hits(&scene, Vec3A::new(2.2, 0.5, 5.0), -Vec3A::Z),
        "the gap between them"
    );
}

#[test]
fn a_mesh_missing_its_arrays_is_skipped_not_fatal() {
    let scene = load(
        "broken_mesh",
        r#"
    def Mesh "NoPoints"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
    }
    def Sphere "Ok" {}"#,
    );
    assert_eq!(scene.world.primitive_breakdown().triangles, 0);
    assert_eq!(scene.world.primitive_breakdown().spheres, 1);
}

#[test]
fn identical_meshes_placed_twice_are_instanced_once_baked() {
    let mesh = |name: &str, x: f32| {
        format!(
            r#"
    def Mesh "{name}"
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)]
        double3 xformOp:translate = ({x}, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}"#
        )
    };
    let once = load("place_once", &mesh("A", 0.0));
    assert_eq!(
        once.world.primitive_breakdown().triangles,
        2,
        "a single placement is baked flat"
    );
    assert_eq!(once.world.primitive_breakdown().instances, 0);

    let twice = load(
        "place_twice",
        &format!("{}{}", mesh("A", -3.0), mesh("B", 3.0)),
    );
    assert_eq!(twice.world.count(), 2);
    let br = twice.world.primitive_breakdown();
    assert_eq!(
        br.instances, 2,
        "two placements of one mesh share a prototype"
    );
    assert_eq!(br.triangles, 0);
    assert_eq!(
        twice.world.unique_primitive_breakdown().triangles,
        2,
        "one resident copy"
    );
    assert!(hits(&twice, Vec3A::new(-3.0, 0.0, 5.0), -Vec3A::Z));
    assert!(hits(&twice, Vec3A::new(3.0, 0.0, 5.0), -Vec3A::Z));
    assert!(!hits(&twice, Vec3A::new(0.0, 0.0, 5.0), -Vec3A::Z));
}

// ---------------------------------------------------------------------------
// Transforms
// ---------------------------------------------------------------------------

#[test]
fn xform_hierarchy_composes_translations() {
    let scene = load(
        "nested_translate",
        r#"
    def Xform "A"
    {
        double3 xformOp:translate = (1, 2, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        def Xform "B"
        {
            double3 xformOp:translate = (0, 0, -3)
            uniform token[] xformOpOrder = ["xformOp:translate"]
            def Sphere "S" { double radius = 0.5 }
        }
    }"#,
    );
    let t = hit_t(&scene, Vec3A::new(1.0, 2.0, 10.0), -Vec3A::Z).expect("composed position");
    assert!((t - 12.5).abs() < 1e-3, "sphere at z = -3: t = {t}");
    assert!(!hits(&scene, Vec3A::new(0.0, 0.0, 10.0), -Vec3A::Z));
}

#[test]
fn a_parent_rotation_moves_a_translated_child() {
    let scene = load(
        "rotate_parent",
        r#"
    def Xform "A"
    {
        float xformOp:rotateY = 90
        uniform token[] xformOpOrder = ["xformOp:rotateY"]
        def Xform "B"
        {
            double3 xformOp:translate = (0, 0, -2)
            uniform token[] xformOpOrder = ["xformOp:translate"]
            def Sphere "S" { double radius = 0.5 }
        }
    }"#,
    );
    // rotateY(90) maps (0, 0, -2) to (-2, 0, 0).
    assert!(
        hits(&scene, Vec3A::new(-2.0, 0.0, 10.0), -Vec3A::Z),
        "child should land at x = -2"
    );
    assert!(!hits(&scene, Vec3A::new(0.0, 0.0, 10.0), -Vec3A::Z));
    assert!(!hits(&scene, Vec3A::new(2.0, 0.0, 10.0), -Vec3A::Z));
}

#[test]
fn translate_then_scale_stack_keeps_the_authored_translation() {
    let scene = load(
        "translate_scale",
        r#"
    def Xform "A"
    {
        double3 xformOp:translate = (0, 4, 0)
        float3 xformOp:scale = (2, 2, 2)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:scale"]
        def Mesh "Quad"
        {
            uniform token subdivisionScheme = "none"
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)]
        }
    }"#,
    );
    let bb = scene.world.bounds().unwrap();
    // Translation stays (0, 4, 0); the quad is scaled to ±2 around it.
    assert!(
        (bb.minimum.y - 2.0).abs() < 1e-3 && (bb.maximum.y - 6.0).abs() < 1e-3,
        "{bb:?}"
    );
    assert!(
        (bb.minimum.x + 2.0).abs() < 1e-3 && (bb.maximum.x - 2.0).abs() < 1e-3,
        "{bb:?}"
    );
}

#[test]
fn a_mirrored_placement_keeps_the_surface_facing_outward() {
    let scene = load(
        "mirror",
        r#"
    def Xform "M"
    {
        float3 xformOp:scale = (-1, 1, 1)
        uniform token[] xformOpOrder = ["xformOp:scale"]
        def Mesh "Quad"
        {
            uniform token subdivisionScheme = "none"
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)]
        }
    }"#,
    );
    // The unmirrored quad winds counter-clockwise seen from +Z, so a ray
    // from +Z sees its front face. Mirroring must not flip that.
    let hit = scene
        .world
        .intersect(&Ray::new(Vec3A::new(0.2, 0.3, 5.0), -Vec3A::Z), 1e-3, 100.0)
        .expect("hit");
    assert!(hit.rec.front_face, "mirroring inverted the winding");
    assert!(hit.rec.normal.abs_diff_eq(Vec3A::Z, 1e-4));
}

#[test]
fn a_matrix_xform_op_is_honoured() {
    let scene = load(
        "matrix",
        r#"
    def Xform "A"
    {
        matrix4d xformOp:transform = ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (7, 0, 0, 1) )
        uniform token[] xformOpOrder = ["xformOp:transform"]
        def Sphere "S" {}
    }"#,
    );
    assert!(hits(&scene, Vec3A::new(7.0, 0.0, 10.0), -Vec3A::Z));
    assert!(!hits(&scene, Vec3A::new(0.0, 0.0, 10.0), -Vec3A::Z));
}

// ---------------------------------------------------------------------------
// Render settings and camera
// ---------------------------------------------------------------------------

#[test]
fn render_settings_are_read_from_the_stage() {
    let scene = load_with_settings("settings", "", SETTINGS);
    assert_eq!(scene.settings.get_dimensions(), (32, 16));
    assert_eq!(scene.settings.samples_per_pixel(), 7);
    assert_eq!(scene.settings.max_depth(), 3);
    assert_eq!(
        scene.settings.sampling_strategy(),
        SamplingStrategy::BalanceMis
    );
    assert_eq!(scene.settings.pixel_filter().name(), "gaussian");
    assert_eq!(scene.settings.pixel_filter().radius(), 2.5);
}

#[test]
fn every_sampling_strategy_token_is_recognised() {
    for (token, want) in [
        ("power", SamplingStrategy::PowerMis),
        ("balance", SamplingStrategy::BalanceMis),
        ("light", SamplingStrategy::LightOnly),
        ("bsdf", SamplingStrategy::BsdfOnly),
    ] {
        let scene = load_with_settings(
            &format!("strategy_{token}"),
            "",
            &format!(
                r#"
def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        token crust:samplingStrategy = "{token}"
    }}
}}"#
            ),
        );
        assert_eq!(scene.settings.sampling_strategy(), want, "{token}");
    }
    // An unknown token falls back to the default rather than failing.
    let scene = load_with_settings(
        "strategy_bogus",
        "",
        r#"
def Scope "Render"
{
    def RenderSettings "settings"
    {
        token crust:samplingStrategy = "banana"
    }
}"#,
    );
    assert_eq!(
        scene.settings.sampling_strategy(),
        SamplingStrategy::PowerMis
    );
}

#[test]
fn image_counters_mirror_the_settings() {
    let scene = load_with_settings("counters", "", SETTINGS);
    assert_eq!(
        (scene.stats.image.width, scene.stats.image.height),
        (32, 16)
    );
    assert_eq!(scene.stats.image.samples_per_pixel, 7);
    assert_eq!(scene.stats.image.max_depth, 3);
}

#[test]
fn the_camera_looks_down_its_local_minus_z() {
    let scene = load(
        "camera",
        r#"
    def Camera "Cam"
    {
        float focalLength = 50
        float horizontalAperture = 20.955
        double3 xformOp:translate = (0, 0, 5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Sphere "Ball" { double radius = 0.5 }"#,
    );
    let ray = scene.camera.get_ray(0.5, 0.5, [0.5, 0.5], 0.0);
    assert!(ray.origin().abs_diff_eq(Vec3A::new(0.0, 0.0, 5.0), 1e-4));
    assert!(ray.direction().normalize().abs_diff_eq(-Vec3A::Z, 1e-4));
    let hit = scene
        .world
        .intersect(&ray, 1e-3, 100.0)
        .expect("the centre ray hits the ball");
    assert!((hit.rec.p.z - 0.5).abs() < 1e-3);
    assert_eq!(ray.mask(), MASK_CAMERA);
}

#[test]
fn a_rotated_camera_turns_its_view() {
    let scene = load(
        "camera_rot",
        r#"
    def Camera "Cam"
    {
        float xformOp:rotateY = -90
        uniform token[] xformOpOrder = ["xformOp:rotateY"]
    }"#,
    );
    let d = scene
        .camera
        .get_ray(0.5, 0.5, [0.5, 0.5], 0.0)
        .direction()
        .normalize();
    // rotateY(-90) takes -Z to +X.
    assert!(d.abs_diff_eq(Vec3A::X, 1e-4), "{d}");
}

#[test]
fn a_wider_aperture_widens_the_field_of_view() {
    let stage = |aperture: f32| {
        format!(
            r#"
    def Camera "Cam"
    {{
        float focalLength = 50
        float horizontalAperture = {aperture}
    }}"#
        )
    };
    let narrow = load("cam_narrow", &stage(10.0));
    let wide = load("cam_wide", &stage(40.0));
    let edge = |s: &Scene| {
        let d = s
            .camera
            .get_ray(1.0, 0.5, [0.5, 0.5], 0.0)
            .direction()
            .normalize();
        d.dot(-Vec3A::Z).acos()
    };
    assert!(
        edge(&wide) > 2.0 * edge(&narrow),
        "{} vs {}",
        edge(&wide),
        edge(&narrow)
    );
}

#[test]
fn f_stop_gives_the_camera_a_lens() {
    let scene = load(
        "camera_dof",
        r#"
    def Camera "Cam"
    {
        float focalLength = 50
        float fStop = 2
        float focusDistance = 5
        double3 xformOp:translate = (0, 0, 5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#,
    );
    let a = scene.camera.get_ray(0.5, 0.5, [0.0, 0.5], 0.0);
    let b = scene.camera.get_ray(0.5, 0.5, [1.0, 0.5], 0.0);
    assert!(
        !a.origin().abs_diff_eq(b.origin(), 1e-6),
        "lens samples must move the origin"
    );
    // Both converge on the focus plane 5 units down the axis.
    assert!(a.at(1.0).abs_diff_eq(Vec3A::ZERO, 1e-3), "{}", a.at(1.0));
    assert!(b.at(1.0).abs_diff_eq(Vec3A::ZERO, 1e-3), "{}", b.at(1.0));
}

// ---------------------------------------------------------------------------
// Materials
// ---------------------------------------------------------------------------

#[test]
fn crust_openpbr_shader_inputs_decode_to_the_material() {
    let scene = load(
        "openpbr",
        r#"
    def Material "Glow"
    {
        token outputs:surface.connect = </World/Glow/S.outputs:surface>
        def Shader "S"
        {
            uniform token info:id = "crust:openpbr"
            float inputs:emissionLuminance = 3
            color3f inputs:emissionColor = (1, 0.5, 0.25)
            token outputs:surface
        }
    }
    def Sphere "Ball" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        rel material:binding = </World/Glow>
    }"#,
    );
    assert_eq!(scene.world.count(), 1);
    assert!(
        scene
            .world
            .material(0)
            .emitted()
            .abs_diff_eq(Vec3A::new(3.0, 1.5, 0.75), 1e-5)
    );
}

#[test]
fn preview_surface_emissive_colour_reaches_the_material() {
    let scene = load(
        "preview",
        r#"
    def Material "M"
    {
        token outputs:surface.connect = </World/M/S.outputs:surface>
        def Shader "S"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (0.1, 0.2, 0.3)
            color3f inputs:emissiveColor = (0.5, 0.25, 0.125)
            token outputs:surface
        }
    }
    def Sphere "Ball" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        rel material:binding = </World/M>
    }"#,
    );
    let e = scene.world.material(0).emitted();
    assert!(e.abs_diff_eq(Vec3A::new(0.5, 0.25, 0.125), 1e-5), "{e}");
}

#[test]
fn unbound_and_unknown_shaders_get_a_non_emissive_default() {
    let scene = load(
        "unbound",
        r#"
    def Material "Weird"
    {
        token outputs:surface.connect = </World/Weird/S.outputs:surface>
        def Shader "S"
        {
            uniform token info:id = "SomeUnknownShader"
            token outputs:surface
        }
    }
    def Sphere "Plain" {}
    def Sphere "Bound" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        rel material:binding = </World/Weird>
        double3 xformOp:translate = (3, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#,
    );
    assert_eq!(scene.world.count(), 2);
    for id in 0..2 {
        assert_eq!(scene.world.material(id).emitted(), Vec3A::ZERO);
        assert!(scene.world.material(id).face_texture().is_none());
    }
}

#[test]
fn a_binding_to_a_missing_material_falls_back() {
    let scene = load(
        "dangling",
        r#"
    def Sphere "Ball" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        rel material:binding = </World/Nope>
    }"#,
    );
    assert_eq!(scene.world.count(), 1);
    assert_eq!(scene.world.material(0).emitted(), Vec3A::ZERO);
}

// ---------------------------------------------------------------------------
// Lights
// ---------------------------------------------------------------------------

#[test]
fn a_sphere_light_is_geometry_plus_a_light_hidden_from_the_camera() {
    let scene = load(
        "sphere_light",
        r#"
    def SphereLight "L"
    {
        float inputs:radius = 0.5
        float inputs:intensity = 2
        color3f inputs:color = (1, 0.5, 0.25)
        double3 xformOp:translate = (0, 5, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#,
    );
    assert_eq!(scene.world.count(), 1);
    assert_eq!(scene.lights.count(), 1);
    assert!(
        scene
            .world
            .material(0)
            .emitted()
            .abs_diff_eq(Vec3A::new(2.0, 1.0, 0.5), 1e-5)
    );
    assert_eq!(scene.lights.lights()[0].geom_id(), Some(0));
    let ray = Ray::new(Vec3A::new(0.0, 5.0, 10.0), -Vec3A::Z);
    assert!(
        scene
            .world
            .intersect(&ray.clone().with_mask(MASK_CAMERA), 1e-3, 100.0)
            .is_none(),
        "camera-invisible by default"
    );
    assert!(
        scene.world.intersect(&ray, 1e-3, 100.0).is_some(),
        "other rays see it"
    );
}

#[test]
fn exposure_scales_light_by_powers_of_two() {
    let scene = load(
        "exposure",
        r#"
    def SphereLight "L"
    {
        float inputs:intensity = 1
        float inputs:exposure = 3
    }"#,
    );
    assert!(
        scene
            .world
            .material(0)
            .emitted()
            .abs_diff_eq(Vec3A::splat(8.0), 1e-5)
    );
}

#[test]
fn camera_visible_light_opts_back_in() {
    let scene = load(
        "visible_light",
        r#"
    def SphereLight "L"
    {
        bool crust:light:cameraVisible = true
    }"#,
    );
    let ray = Ray::new(Vec3A::new(0.0, 0.0, 10.0), -Vec3A::Z).with_mask(MASK_CAMERA);
    assert!(scene.world.intersect(&ray, 1e-3, 100.0).is_some());
}

#[test]
fn an_authored_ray_mask_wins_on_lights_and_hides_geometry() {
    let scene = load(
        "masks",
        r#"
    def SphereLight "L"
    {
        int crust:rayMask = 1
    }
    def Sphere "Hidden"
    {
        int crust:rayMask = 6
        double3 xformOp:translate = (5, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#,
    );
    // The light with mask 1 is camera-only now.
    let at_light = Ray::new(Vec3A::new(0.0, 0.0, 10.0), -Vec3A::Z);
    assert!(
        scene
            .world
            .intersect(&at_light.clone().with_mask(MASK_CAMERA), 1e-3, 100.0)
            .is_some()
    );
    assert!(
        scene
            .world
            .intersect(&at_light.with_mask(crust_core::MASK_SHADOW), 1e-3, 100.0)
            .is_none()
    );
    // The sphere with mask 6 hides from the camera only.
    let at_sphere = Ray::new(Vec3A::new(5.0, 0.0, 10.0), -Vec3A::Z);
    assert!(
        scene
            .world
            .intersect(&at_sphere.clone().with_mask(MASK_CAMERA), 1e-3, 100.0)
            .is_none()
    );
    assert!(
        scene
            .world
            .intersect(&at_sphere.with_mask(crust_core::MASK_SHADOW), 1e-3, 100.0)
            .is_some()
    );
}

#[test]
fn a_rect_light_is_two_triangles_and_one_light() {
    let scene = load(
        "rect_light",
        r#"
    def RectLight "R"
    {
        float inputs:width = 2
        float inputs:height = 1
        float inputs:intensity = 5
        double3 xformOp:translate = (0, 0, 3)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#,
    );
    assert_eq!(scene.world.count(), 1);
    assert_eq!(scene.world.primitive_breakdown().triangles, 2);
    assert_eq!(scene.lights.count(), 1);
    let bb = scene.world.bounds().unwrap();
    assert!(
        (bb.maximum.x - 1.0).abs() < 1e-3 && (bb.maximum.y - 0.5).abs() < 1e-3,
        "{bb:?}"
    );
    // It emits along local -Z: a point below it on -Z is lit.
    let s = scene.lights.lights()[0]
        .sample_li(Vec3A::new(0.0, 0.0, 0.0), 0.5, 0.5)
        .expect("reachable");
    assert!(s.direction.z > 0.99);
    assert!(
        s.pdf.get().is_finite() && s.pdf.get() < 1e3,
        "the emitting side faces the point"
    );
}

#[test]
fn infinite_lights_add_no_geometry() {
    let scene = load(
        "infinite",
        r#"
    def DistantLight "Sun"
    {
        float inputs:intensity = 2
        float inputs:angle = 1
        float xformOp:rotateX = -90
        uniform token[] xformOpOrder = ["xformOp:rotateX"]
    }
    def DomeLight "Sky"
    {
        color3f inputs:color = (0.2, 0.3, 0.4)
    }"#,
    );
    assert_eq!(scene.world.count(), 0);
    assert_eq!(scene.lights.count(), 2);
    for l in scene.lights.lights() {
        assert!(l.geom_id().is_none());
    }
    // The dome answers every escaping ray with its colour.
    let dome = scene
        .lights
        .lights()
        .iter()
        .find(|l| l.escaped(Vec3A::ZERO, Vec3A::X).is_some())
        .expect("dome");
    let (r, _) = dome
        .escaped(Vec3A::ZERO, Vec3A::new(0.3, -0.2, 0.9).normalize())
        .unwrap();
    assert!(r.abs_diff_eq(Vec3A::new(0.2, 0.3, 0.4), 1e-5));
    // rotateX(-90) turns local -Z into -Y: the sun shines straight down, so
    // a ray escaping straight up finds it.
    let sun = scene
        .lights
        .lights()
        .iter()
        .find(|l| {
            l.escaped(Vec3A::ZERO, Vec3A::Y)
                .is_some_and(|(r, _)| r.x > 1.0)
        })
        .expect("sun overhead");
    assert!(sun.escaped(Vec3A::ZERO, -Vec3A::Y).is_none());
}

// ---------------------------------------------------------------------------
// UsdLux: every light type, normalize, colour temperature, shaping
// ---------------------------------------------------------------------------

/// Both halves of MIS must see the same light: for each NEE sample, a ray
/// aimed along it must hit the light's geometry, the emission that hit
/// reports must equal the sampled radiance, and the bounce-side pdf must
/// equal the sampled one. Returns how many samples carried radiance.
fn assert_mis_sides_agree(scene: &Scene, from: Vec3A) -> usize {
    assert_eq!(scene.lights.count(), 1);
    let light = &scene.lights.lights()[0];
    let mut lit = 0;
    let mut rng = openqmc::pcg::Rng::new(11);
    for _ in 0..64 {
        let Some(s) = light.sample_li(from, rng.next_f32(), rng.next_f32()) else {
            continue;
        };
        let ray = Ray::new(from, s.direction);
        let hit = scene
            .world
            .intersect(&ray, 1e-4, s.distance * 1.001 + 1e-3)
            .expect("an NEE sample must land on the light's geometry");
        assert_eq!(Some(hit.geom_id), light.geom_id());
        if hit.rec.t < s.distance * 0.999 {
            // A far-side point of a closed shape: its shadow ray stops at
            // the near side, so NEE never delivers it. Nothing to compare.
            continue;
        }
        let cos = ray.direction().normalize().dot(hit.rec.normal).abs();
        let emitted = hit.mat.emitted_at(&ray, &hit.rec, cos);
        let tol = 1e-3 * s.radiance.max_element().max(1e-6);
        assert!(
            emitted.abs_diff_eq(s.radiance, tol),
            "bounce sees {emitted}, NEE sampled {}",
            s.radiance
        );
        if s.radiance.max_element() > 0.0 {
            lit += 1;
            let pdf = light.pdf_at_point(from, hit.rec.p).map_or(0.0, |p| p.get());
            assert!(
                (pdf - s.pdf.get()).abs() <= 2e-3 * s.pdf.get(),
                "bounce pdf {pdf} vs NEE pdf {}",
                s.pdf.get()
            );
        }
    }
    lit
}

/// The radiance a light sends toward `from`, averaged over NEE samples that
/// carry any (a uniform, unshaped light's radiance is a constant).
fn radiance_toward(scene: &Scene, from: Vec3A) -> Vec3A {
    let light = &scene.lights.lights()[0];
    (0..64)
        .filter_map(|i| {
            let (u, v) = ((i % 8) as f32 + 0.5, (i / 8) as f32 + 0.5);
            light.sample_li(from, u / 8.0, v / 8.0)
        })
        .map(|s| s.radiance)
        .find(|r| r.max_element() > 0.0)
        .unwrap_or(Vec3A::ZERO)
}

#[test]
fn disk_light_is_a_one_sided_analytic_disk() {
    let scene = load(
        "disk_light",
        r#"
    def DiskLight "D"
    {
        float inputs:radius = 1
        float inputs:intensity = 3
        double3 xformOp:translate = (0, 0, 2)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#,
    );
    assert_eq!(scene.lights.count(), 1);
    assert_eq!(scene.world.primitive_breakdown().disks, 1);
    // It faces local −Z: the origin, below it, is lit at the authored nits…
    assert_eq!(radiance_toward(&scene, Vec3A::ZERO), Vec3A::splat(3.0));
    assert!(assert_mis_sides_agree(&scene, Vec3A::ZERO) > 0);
    // …and a point above it, behind the emitting side, sees nothing — from
    // either strategy.
    let behind = Vec3A::new(0.2, 0.1, 5.0);
    assert_eq!(radiance_toward(&scene, behind), Vec3A::ZERO);
    assert_eq!(assert_mis_sides_agree(&scene, behind), 0);
}

#[test]
fn cylinder_light_is_an_open_tube_emitting_outward() {
    let scene = load(
        "cylinder_light",
        r#"
    def CylinderLight "C"
    {
        float inputs:radius = 0.25
        float inputs:length = 4
        float inputs:intensity = 2
    }"#,
    );
    assert_eq!(scene.world.primitive_breakdown().cylinders, 1);
    let bb = scene.world.bounds().unwrap();
    assert!((bb.maximum.x - 2.0).abs() < 1e-4 && (bb.maximum.y - 0.25).abs() < 1e-4);
    // Lit from the side, either side of the axis.
    for from in [Vec3A::new(0.3, 3.0, 0.0), Vec3A::new(-0.5, 0.0, -2.0)] {
        assert_eq!(radiance_toward(&scene, from), Vec3A::splat(2.0));
        assert!(assert_mis_sides_agree(&scene, from) > 0);
    }
}

/// `inputs:normalize` divides by the *world-space* area, transform scale
/// included, so the same power spreads over a bigger surface.
#[test]
fn normalize_divides_area_lights_by_their_world_area() {
    let pi = std::f32::consts::PI;
    for (name, prim, area) in [
        (
            "rect",
            "def RectLight \"L\" { float inputs:width = 2\n float inputs:height = 1\n \
             float3 xformOp:scale = (3, 3, 3)\n uniform token[] xformOpOrder = [\"xformOp:scale\"] }",
            18.0,
        ),
        (
            "sphere",
            "def SphereLight \"L\" { float inputs:radius = 1 }",
            4.0 * pi,
        ),
        (
            "disk",
            "def DiskLight \"L\" { float inputs:radius = 0.5\n \
             float3 xformOp:scale = (2, 2, 2)\n uniform token[] xformOpOrder = [\"xformOp:scale\"] }",
            pi,
        ),
        (
            "cylinder",
            "def CylinderLight \"L\" { float inputs:radius = 0.5\n float inputs:length = 2 }",
            2.0 * pi,
        ),
    ] {
        let prim_n = prim.replacen(
            '{',
            "{ bool inputs:normalize = 1\n float inputs:intensity = 100\n",
            1,
        );
        let plain = load(&format!("norm_off_{name}"), prim);
        let norm = load(&format!("norm_on_{name}"), &prim_n);
        // A point every shape emits toward: below, off to the side.
        let from = Vec3A::new(0.1, -0.4, -5.0);
        assert_eq!(
            radiance_toward(&plain, from),
            Vec3A::ONE,
            "{name} un-normalized"
        );
        let r = radiance_toward(&norm, from);
        assert!(
            (r.x - 100.0 / area).abs() < 1e-3 * (100.0 / area),
            "{name}: {} vs 100/{area}",
            r.x
        );
    }
}

/// Squashed round lights — an ellipsoid, an ellipse, an elliptical tube —
/// are placed through instances and sampled non-uniformly in world area;
/// the pdf both MIS sides compute must still be the same density, and
/// `normalize` must still divide by the true world area.
#[test]
fn squashed_round_lights_stay_consistent() {
    let pi = std::f32::consts::PI;
    // An ellipse with semi-axes 1.5 and 0.5 has area π·0.75 exactly.
    let disk = load(
        "squashed_disk",
        r#"
    def DiskLight "D"
    {
        float inputs:radius = 0.5
        bool inputs:normalize = 1
        float3 xformOp:scale = (3, 1, 1)
        double3 xformOp:translate = (0, 0, 2)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:scale"]
    }"#,
    );
    assert_eq!(
        disk.world.primitive_breakdown().instances,
        1,
        "placed by an instance"
    );
    let r = radiance_toward(&disk, Vec3A::ZERO);
    assert!((r.x - 1.0 / (0.75 * pi)).abs() < 1e-4, "{}", r.x);
    assert!(assert_mis_sides_agree(&disk, Vec3A::new(0.3, 0.2, 0.0)) > 0);

    let sphere = load(
        "squashed_sphere",
        r#"
    def SphereLight "S"
    {
        float inputs:radius = 1
        float3 xformOp:scale = (1, 2, 0.5)
        double3 xformOp:rotateXYZ = (20, 35, 0)
        uniform token[] xformOpOrder = ["xformOp:rotateXYZ", "xformOp:scale"]
    }"#,
    );
    assert!(assert_mis_sides_agree(&sphere, Vec3A::new(0.5, -4.0, 1.0)) > 0);

    let tube = load(
        "squashed_cylinder",
        r#"
    def CylinderLight "C"
    {
        float inputs:radius = 0.5
        float inputs:length = 2
        float3 xformOp:scale = (1, 1, 3)
        uniform token[] xformOpOrder = ["xformOp:scale"]
    }"#,
    );
    assert!(assert_mis_sides_agree(&tube, Vec3A::new(0.2, -3.0, 0.4)) > 0);
}

#[test]
fn a_uniformly_scaled_sphere_light_scales_its_radius() {
    let scene = load(
        "scaled_sphere_light",
        r#"
    def SphereLight "S"
    {
        float inputs:radius = 0.5
        float3 xformOp:scale = (4, 4, 4)
        uniform token[] xformOpOrder = ["xformOp:scale"]
    }"#,
    );
    let bb = scene.world.bounds().unwrap();
    assert!((bb.maximum.x - 2.0).abs() < 1e-5, "{bb:?}");
    assert_eq!(
        scene.world.primitive_breakdown().spheres,
        1,
        "still analytic"
    );
}

#[test]
fn color_temperature_tints_the_emission() {
    let warm = load(
        "color_temperature",
        r#"
    def SphereLight "S"
    {
        float inputs:intensity = 2
        color3f inputs:color = (1, 0.5, 1)
        bool inputs:enableColorTemperature = 1
        float inputs:colorTemperature = 3000
    }"#,
    );
    let off = load(
        "color_temperature_off",
        r#"
    def SphereLight "S"
    {
        float inputs:intensity = 2
        color3f inputs:color = (1, 0.5, 1)
        float inputs:colorTemperature = 3000
    }"#,
    );
    let from = Vec3A::new(0.0, -3.0, 0.0);
    let bb = crust_core::blackbody_rgb(3000.0);
    let expect = Vec3A::new(2.0, 1.0, 2.0) * bb;
    assert!(radiance_toward(&warm, from).abs_diff_eq(expect, 1e-5));
    assert_eq!(
        radiance_toward(&off, from),
        Vec3A::new(2.0, 1.0, 2.0),
        "the temperature is ignored unless enabled"
    );
}

/// UsdLux distant-light units: `intensity` is the source's luminance in
/// nits; `normalize` makes it the illuminance on a facing surface; a zero
/// angle is a delta whose `intensity` is that illuminance.
#[test]
fn distant_light_units_follow_the_spec() {
    let sun = |name: &str, extra: &str, angle: f32| {
        let s = load(
            name,
            &format!(
                "def DistantLight \"Sun\" {{ float inputs:intensity = 2\n \
                 float inputs:angle = {angle}\n {extra} }}"
            ),
        );
        let l = &s.lights.lights()[0];
        let smp = l.sample_li(Vec3A::ZERO, 0.5, 0.5).unwrap();
        let half = 0.5 * crust_core::DistantLight::clamp_diameter(angle).to_radians();
        (
            smp.radiance.x,
            smp.radiance.x * crust_core::projected_cone_solid_angle(half),
        )
    };
    // Un-normalised, the luminance is the authored nits (to the f32 rounding
    // of a 0.5° cone), and what reaches a facing surface is L·π·sin²θ exactly.
    let exact = |nits: f64, diameter: f64| {
        nits * std::f64::consts::PI * (0.5 * diameter.to_radians()).sin().powi(2)
    };
    let (radiance, irradiance) = sun("sun_nits", "", 1.0);
    assert!(
        (radiance - 2.0).abs() < 2e-2,
        "luminance in nits: {radiance}"
    );
    let want = exact(2.0, 1.0) as f32;
    assert!(
        (irradiance / want - 1.0).abs() < 1e-5,
        "{irradiance} vs {want}"
    );
    let (_, irradiance) = sun("sun_lux", "bool inputs:normalize = 1", 1.0);
    assert!(
        (irradiance - 2.0).abs() < 1e-5,
        "illuminance in lux: {irradiance}"
    );
    let (_, delta) = sun("sun_delta", "", 0.0);
    assert!(
        (delta - 2.0).abs() < 1e-5,
        "a delta light's intensity is lux: {delta}"
    );
    // Narrower than the widening floor — so narrow the f32 cosine is 1 — the
    // widened cone still delivers what the authored one did.
    let (_, narrow) = sun("sun_narrow", "", 0.01);
    let want = exact(2.0, 0.01) as f32;
    assert!(
        want > 0.0 && (narrow / want - 1.0).abs() < 1e-5,
        "{narrow} vs {want}"
    );
}

/// A spotlight: `ShapingAPI`'s cone cuts emission off outside its angle, on
/// the NEE side and on the bounce side alike.
#[test]
fn shaping_cone_reaches_both_mis_sides() {
    let scene = load(
        "shaped_rect",
        r#"
    def RectLight "Spot" (
        prepend apiSchemas = ["ShapingAPI"]
    )
    {
        float inputs:intensity = 5
        float inputs:shaping:cone:angle = 30
        double3 xformOp:translate = (0, 0, 4)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#,
    );
    // Straight below: inside the cone.
    assert_eq!(radiance_toward(&scene, Vec3A::ZERO), Vec3A::splat(5.0));
    assert!(assert_mis_sides_agree(&scene, Vec3A::ZERO) > 0);
    // 60° off the axis: outside it, from either strategy.
    let oblique = Vec3A::new(4.0 * 3f32.sqrt(), 0.0, 0.0);
    assert_eq!(radiance_toward(&scene, oblique), Vec3A::ZERO);
    assert_eq!(assert_mis_sides_agree(&scene, oblique), 0);
}

/// With `ShapingAPI` applied and no cone authored, the schema's 90° fallback
/// is in force; without the API there is no cone at all.
#[test]
fn shaping_api_fallback_cone_only_applies_when_the_api_is() {
    let body =
        |api: &str| format!("def SphereLight \"S\" ( {api} ) {{ float inputs:radius = 0.5 }}");
    let with = load(
        "shaping_applied",
        &body("prepend apiSchemas = [\"ShapingAPI\"]"),
    );
    let without = load("shaping_absent", &body(""));
    // Behind the light's −Z axis, i.e. on its +Z side.
    let behind = Vec3A::new(0.0, 0.0, 5.0);
    assert_eq!(radiance_toward(&with, behind), Vec3A::ZERO);
    assert_eq!(radiance_toward(&without, behind), Vec3A::ONE);
    assert_eq!(radiance_toward(&with, -behind), Vec3A::ONE);
}

/// An IES profile arrives through the host's loader and scales the light by
/// direction, relative to its −Z axis.
#[test]
fn ies_profile_shapes_the_light() {
    struct Ies;
    impl crust_core::AssetLoader for Ies {
        fn load_environment(&self, _: &std::path::Path) -> Option<crust_core::EnvironmentMap> {
            None
        }
        fn load_ies(
            &self,
            path: &std::path::Path,
        ) -> Option<std::sync::Arc<crust_core::IesProfile>> {
            assert!(path.ends_with("spot.ies"), "{}", path.display());
            // 4 cd within 45° of the axis, nothing past 50°.
            let v = [0.0f32, 45.0, 50.0, 180.0].map(f32::to_radians).to_vec();
            let h = [0.0f32, 360.0].map(f32::to_radians).to_vec();
            let row = vec![4.0, 4.0, 0.0, 0.0];
            crust_core::IesProfile::new(v, h, vec![row.clone(), row]).map(std::sync::Arc::new)
        }
    }
    let path = write_stage(
        "ies_light",
        r#"#usda 1.0
def DiskLight "Spot" (
    prepend apiSchemas = ["ShapingAPI"]
)
{
    float inputs:shaping:cone:angle = 180
    asset inputs:shaping:ies:file = @spot.ies@
    double3 xformOp:translate = (0, 0, 3)
    uniform token[] xformOpOrder = ["xformOp:translate"]
}
"#,
    );
    let scene = Scene::from_usd_with_assets(&path, &Ies).unwrap();
    assert_eq!(radiance_toward(&scene, Vec3A::ZERO), Vec3A::splat(4.0));
    assert!(assert_mis_sides_agree(&scene, Vec3A::ZERO) > 0);
    // 70° off the axis: past the profile's beam.
    let off = Vec3A::new(3.0 * 70f32.to_radians().tan(), 0.0, 0.0);
    assert_eq!(radiance_toward(&scene, off), Vec3A::ZERO);
}

// ---------------------------------------------------------------------------
// Volumes
// ---------------------------------------------------------------------------

#[test]
fn an_unsized_volume_prim_is_a_unit_cube() {
    // Without an authored `size` the importer uses the unit cube, not
    // USD's default Cube size of 2.
    let scene = load(
        "unit_volume",
        r#"
    def Cube "Fog"
    {
        token crust:volume:type = "homogeneous"
    }"#,
    );
    let v = &scene.volumes[0];
    assert_eq!(v.density(Vec3A::splat(0.45)), 1.0);
    assert_eq!(v.density(Vec3A::new(0.55, 0.0, 0.0)), 0.0);
}

#[test]
fn a_volume_prim_is_a_region_not_geometry() {
    let scene = load(
        "fog",
        r#"
    def Cube "Fog"
    {
        double size = 4
        token crust:volume:type = "homogeneous"
        color3f crust:volume:sigmaS = (0.5, 0.5, 0.5)
        color3f crust:volume:sigmaA = (0.1, 0.2, 0.3)
        float crust:volume:densityScale = 2
        float crust:volume:anisotropy = 0.3
        double3 xformOp:translate = (0, 2, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#,
    );
    assert_eq!(scene.world.count(), 0, "a volume's bounds must not occlude");
    assert_eq!(scene.volumes.len(), 1);
    let v = &scene.volumes[0];
    assert!(v.is_homogeneous());
    assert!(
        v.sigma_s.abs_diff_eq(Vec3A::splat(1.0), 1e-6),
        "densityScale folds in: {}",
        v.sigma_s
    );
    assert!(v.sigma_a.abs_diff_eq(Vec3A::new(0.2, 0.4, 0.6), 1e-6));
    assert!((v.g - 0.3).abs() < 1e-6);
    // size 4 → half 2, centred at y = 2: spans y ∈ [0, 4].
    assert_eq!(v.density(Vec3A::new(0.0, 3.9, 0.0)), 1.0);
    assert_eq!(v.density(Vec3A::new(0.0, 4.1, 0.0)), 0.0);
    assert_eq!(v.density(Vec3A::new(1.9, 0.1, -1.9)), 1.0);
    assert_eq!(v.density(Vec3A::new(2.1, 2.0, 0.0)), 0.0);
    assert_eq!(scene.stats.scene.volumes, 1);
}

#[test]
fn smoke_and_grid_volumes_import_their_fields() {
    let scene = load(
        "smoke_grid",
        r#"
    def Cube "Smoke"
    {
        token crust:volume:type = "smoke"
        float crust:volume:noiseScale = 3
        int crust:volume:noiseOctaves = 2
    }
    def Cube "Grid"
    {
        double size = 2
        token crust:volume:type = "grid"
        int[] crust:volume:gridDims = [2, 1, 1]
        float[] crust:volume:gridData = [0, 1]
        double3 xformOp:translate = (10, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Cube "BadGrid"
    {
        token crust:volume:type = "grid"
        int[] crust:volume:gridDims = [2, 2, 2]
        float[] crust:volume:gridData = [1, 2, 3]
    }"#,
    );
    assert_eq!(scene.volumes.len(), 2, "the mis-sized grid is skipped");
    assert!(scene.volumes.iter().all(|v| !v.is_homogeneous()));
    // The grid: size 2 → half 1, centred at x = 10; the low-x voxel is
    // empty, the high-x voxel full.
    let grid = scene
        .volumes
        .iter()
        .find(|v| v.density(Vec3A::new(10.0, 0.0, 0.0)) > 0.0)
        .expect("the grid region sits at x = 10");
    assert!(grid.density(Vec3A::new(10.9, 0.0, 0.0)) > 0.9);
    assert!(grid.density(Vec3A::new(9.1, 0.0, 0.0)) < 0.1);
}

#[test]
fn an_unknown_volume_type_is_skipped() {
    let scene = load(
        "bad_volume",
        r#"
    def Cube "Mystery"
    {
        token crust:volume:type = "plasma"
    }"#,
    );
    assert!(scene.volumes.is_empty());
    assert_eq!(scene.world.count(), 0, "still not turned into geometry");
}

// ---------------------------------------------------------------------------
// Subdivision
// ---------------------------------------------------------------------------

const CUBE: &str = r#"
        int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]
        int[] faceVertexIndices = [0, 1, 3, 2, 2, 3, 5, 4, 4, 5, 7, 6, 6, 7, 1, 0, 1, 7, 5, 3, 6, 0, 2, 4]
        point3f[] points = [(-1, -1, 1), (1, -1, 1), (-1, 1, 1), (1, 1, 1), (-1, 1, -1), (1, 1, -1), (-1, -1, -1), (1, -1, -1)]
"#;

/// A `RenderSettings` prim authoring only `crust:subdivisionLevel`.
fn subdiv_settings(level: i32) -> String {
    format!(
        r#"def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        int crust:subdivisionLevel = {level}
    }}
}}"#
    )
}

/// The cube with an authored Catmull-Clark scheme.
fn catmull_cube() -> String {
    format!(
        r#"
    def Mesh "Cube"
    {{
{CUBE}
        uniform token subdivisionScheme = "catmullClark"
    }}"#
    )
}

fn triangles(scene: &Scene) -> usize {
    scene.world.primitive_breakdown().triangles
}

#[test]
fn subdivision_level_multiplies_faces_by_four() {
    let at_level = |level: i32| {
        triangles(&load_with_settings(
            &format!("subdiv_{level}"),
            &catmull_cube(),
            &subdiv_settings(level),
        ))
    };
    let l0 = at_level(0);
    assert_eq!(l0, 12, "level 0 renders the cage");
    assert_eq!(at_level(1), 4 * l0);
    assert_eq!(at_level(2), 16 * l0);
}

#[test]
fn by_default_nothing_is_refined() {
    let scene = load("subdiv_default", &catmull_cube());
    assert_eq!(triangles(&scene), 12, "default level 0: the cage");
}

/// An unauthored scheme is USD's fallback, `catmullClark` — how ALab and
/// Kitchen_set mark their subdivision meshes.
#[test]
fn an_unauthored_scheme_is_catmull_clark() {
    let scene = load_with_settings(
        "subdiv_unauthored",
        &format!(
            r#"
    def Mesh "Cube" {{ {CUBE} }}"#
        ),
        &subdiv_settings(3),
    );
    assert_eq!(triangles(&scene), 12 * 64);
    // The rounded limit surface, not the cage: the corner is gone.
    assert!(!hits(
        &scene,
        Vec3A::splat(0.95) + Vec3A::Z * 5.0,
        -Vec3A::Z
    ));
}

/// At level 0 a subdivision surface renders its cage — same triangles — but
/// shades it with smooth normals, where a `none` cage stays faceted.
#[test]
fn level_zero_shades_the_cage_smooth() {
    let normal_at = |scene: &Scene| {
        scene
            .world
            .intersect(&Ray::new(Vec3A::new(0.5, 0.5, 5.0), -Vec3A::Z), 1e-3, 100.0)
            .unwrap()
            .rec
            .normal
    };
    let smooth = load_with_settings("subdiv_level0", &catmull_cube(), &subdiv_settings(0));
    assert_eq!(triangles(&smooth), 12, "level 0 keeps the cage");
    let n = normal_at(&smooth);
    assert!(
        n.z < 0.999 && n.x > 1e-3 && n.y > 1e-3,
        "the cage top shades with a smooth normal, got {n:?}"
    );
    let faceted = load(
        "subdiv_none_faceted",
        &format!(
            r#"
    def Mesh "Cube"
    {{
{CUBE}
        uniform token subdivisionScheme = "none"
    }}"#
        ),
    );
    assert!(
        (normal_at(&faceted).z - 1.0).abs() < 1e-5,
        "scheme none is faceted"
    );
}

/// A smooth level-0 subdivision cage and a faceted `none` cage share every
/// array and the material, but not their shading: they must not intern to
/// one shared mesh, whichever comes first.
#[test]
fn a_smooth_cage_and_a_faceted_cage_do_not_share_a_mesh() {
    let cube = |name: &str, x: f32, scheme: &str| {
        format!(
            r#"
    def Mesh "{name}" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
{CUBE}
        {scheme}
        rel material:binding = </World/Looks/Grey>
        double3 xformOp:translate = ({x}, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}"#
        )
    };
    let looks = r#"
    def Scope "Looks" {
        def Material "Grey" {
            token outputs:surface.connect = </World/Looks/Grey/S.outputs:surface>
            def Shader "S" {
                uniform token info:id = "crust:openpbr"
                token outputs:surface
            }
        }
    }"#;
    let none = r#"uniform token subdivisionScheme = "none""#;
    for (name, first, second) in [
        (
            "subdiv_key_faceted_first",
            ("Faceted", -3.0, none),
            ("Smooth", 3.0, ""),
        ),
        (
            "subdiv_key_smooth_first",
            ("Smooth", 3.0, ""),
            ("Faceted", -3.0, none),
        ),
    ] {
        let scene = load(
            name,
            &format!(
                "{looks}{}{}",
                cube(first.0, first.1, first.2),
                cube(second.0, second.1, second.2)
            ),
        );
        let normal_at = |x: f32| {
            scene
                .world
                .intersect(&Ray::new(Vec3A::new(x, 0.5, 5.0), -Vec3A::Z), 1e-3, 100.0)
                .unwrap()
                .rec
                .normal
        };
        let faceted = normal_at(-3.0 + 0.5);
        let smooth = normal_at(3.0 + 0.5);
        assert!(
            (faceted.z - 1.0).abs() < 1e-5,
            "{name}: the none cage shades faceted, got {faceted:?}"
        );
        assert!(
            smooth.z < 0.999,
            "{name}: the subdivision cage shades smooth, got {smooth:?}"
        );
    }
}

/// The per-prim attribute is retired: the load's level wins over it.
#[test]
fn a_per_prim_level_is_no_longer_read() {
    let scene = load(
        "subdiv_legacy",
        &format!(
            r#"
    def Mesh "Cube"
    {{
{CUBE}
        int crust:subdivisionLevel = 3
    }}"#
        ),
    );
    assert_eq!(
        triangles(&scene),
        12,
        "the default level 0, not the prim's 3"
    );
}

#[test]
fn a_subdivided_cube_shrinks_toward_a_sphere() {
    let cage = load(
        "subdiv_cage",
        &format!(
            r#"
    def Mesh "Cube" {{ {CUBE} uniform token subdivisionScheme = "none" }}"#
        ),
    );
    let smooth = load_with_settings("subdiv_smooth", &catmull_cube(), &subdiv_settings(3));
    let corner = Vec3A::splat(0.95);
    assert!(
        hits(&cage, corner + Vec3A::Z * 5.0, -Vec3A::Z),
        "the cage fills its corner"
    );
    // Catmull-Clark pulls the corners well inside the cube.
    let cage_t = hit_t(&cage, Vec3A::new(0.0, 0.0, 5.0), -Vec3A::Z).unwrap();
    let smooth_t = hit_t(&smooth, Vec3A::new(0.0, 0.0, 5.0), -Vec3A::Z).unwrap();
    assert!(
        smooth_t > cage_t,
        "the limit surface sits inside the cage: {smooth_t} vs {cage_t}"
    );
    let bb = smooth.world.bounds().unwrap();
    assert!(bb.maximum.x < 1.0 && bb.maximum.x > 0.5, "{bb:?}");
    // Smooth shading normals are unit length and not axis-aligned at an
    // oblique hit.
    let hit = smooth
        .world
        .intersect(&Ray::new(Vec3A::new(0.4, 0.4, 5.0), -Vec3A::Z), 1e-3, 100.0)
        .unwrap();
    assert!((hit.rec.normal.length() - 1.0).abs() < 1e-3);
    assert!(
        hit.rec.normal.x > 0.05 && hit.rec.normal.y > 0.05,
        "{}",
        hit.rec.normal
    );
}

#[test]
fn subdivision_scheme_none_and_clamping() {
    let none = load_with_settings(
        "subdiv_none",
        &format!(
            r#"
    def Mesh "Cube"
    {{
{CUBE}
        uniform token subdivisionScheme = "none"
    }}"#
        ),
        &subdiv_settings(2),
    );
    assert_eq!(triangles(&none), 12, "scheme none renders the cage");
    let huge = load_with_settings("subdiv_clamped", &catmull_cube(), &subdiv_settings(40));
    // Clamped to level 6: 12 · 4^6.
    assert_eq!(triangles(&huge), 12 * 4096);
}

#[test]
fn bilinear_and_loop_schemes() {
    let cube = |scheme: &str| {
        format!(
            r#"
    def Mesh "Cube"
    {{
{CUBE}
        uniform token subdivisionScheme = "{scheme}"
    }}"#
        )
    };
    let bilinear = load_at_level("subdiv_bilinear", &cube("bilinear"), 1);
    assert_eq!(triangles(&bilinear), 12 * 4, "bilinear refines");
    // Bilinear keeps the cube's shape: its corner stays filled.
    assert!(hits(
        &bilinear,
        Vec3A::splat(0.95) + Vec3A::Z * 5.0,
        -Vec3A::Z
    ));
    // Loop needs an all-triangle cage; the quad cube warns and stays a cage.
    let looped = load_at_level("subdiv_loop_quads", &cube("loop"), 1);
    assert_eq!(triangles(&looped), 12);
}

/// Streaming walks each top-level subtree under its own masked stage; the
/// load-wide level must reach every one of them.
#[test]
fn a_streamed_import_subdivides_every_chunk() {
    let body: String = (0..4)
        .map(|i| {
            format!(
                r#"
    def Mesh "Cube{i}"
    {{
{CUBE}
        uniform token subdivisionScheme = "catmullClark"
        double3 xformOp:translate = ({x}, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}"#,
                x = 3 * i
            )
        })
        .collect();
    let scene = load_at_level("subdiv_streamed", &body, 1);
    assert_eq!(scene.world.count(), 4);
    // Four identical cages: one shared refined mesh, instanced four times,
    // or four baked copies — either way every placement is refined.
    for i in 0..4 {
        let t = hit_t(&scene, Vec3A::new(3.0 * i as f32, 0.0, 5.0), -Vec3A::Z)
            .unwrap_or_else(|| panic!("cube {i} missing"));
        assert!(t > 4.0 + 1e-3, "cube {i} was not refined (t = {t})");
    }
}

/// The host's level (the CLI's `--subdiv-level`) wins over the stage's.
#[test]
fn the_host_subdivision_level_overrides_the_stage() {
    let path = write_stage(
        "subdiv_override",
        &format!(
            r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
{}
}}
{}
"#,
            catmull_cube(),
            subdiv_settings(3)
        ),
    );
    let at = |level: Option<u32>| {
        let options = UsdImportOptions {
            subdivision_level: level,
            ..UsdImportOptions::default()
        };
        triangles(&Scene::from_usd_with_options(&path, &NoAssets, &options).unwrap())
    };
    assert_eq!(at(None), 12 * 64, "the stage's level 3");
    assert_eq!(at(Some(0)), 12, "--subdiv-level 0 renders the cage");
    assert_eq!(at(Some(1)), 12 * 4);
}

/// A flat 2×2 quad charted 0..1, subdivided: every hit must read the chart
/// the cage authored at that point — and must read one at all, where a
/// subdivided mesh used to drop its UVs. A sharp-cornered flat quad
/// reproduces affine data, so the refined chart is exactly `(x, y) / 2`.
#[test]
fn a_subdivided_mesh_keeps_its_uv_chart() {
    let dir = std::env::temp_dir().join("crust_usd_inline_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    // A `.mtlx` material, because `uses_uv()` is what makes the importer
    // read a chart at all.
    std::fs::write(
        dir.join("subdiv_flat.mtlx"),
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
    .expect("write probe mtlx");
    let quad = |name: &str, st: &str| {
        load_at_level(
            name,
            &format!(
                r#"
    def Scope "Looks" {{
        def Material "Mtl" (
            prepend references = @subdiv_flat.mtlx@</MaterialX/Materials/mtlx_flat>
        ) {{
        }}
    }}
    def Mesh "Quad" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (2, 0, 0), (2, 2, 0), (0, 2, 0)]
        {st}
        uniform token subdivisionScheme = "catmullClark"
        rel material:binding = </World/Looks/Mtl>
    }}"#
            ),
            1,
        )
    };
    let face_varying = quad(
        "subdiv_uv_fv",
        r#"texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (interpolation = "faceVarying")
        uniform token faceVaryingLinearInterpolation = "boundaries""#,
    );
    let vertex = quad(
        "subdiv_uv_vertex",
        r#"texCoord2f[] primvars:st = [(0, 1), (1, 1), (1, 0), (0, 0)] (interpolation = "vertex")
        int[] primvars:st:indices = [3, 2, 1, 0]"#,
    );
    for (what, scene) in [("faceVarying", &face_varying), ("vertex", &vertex)] {
        assert_eq!(triangles(scene), 2 * 4, "{what}: refined to level 1");
        for (x, y) in [(1.0, 1.0), (0.5, 1.5), (1.7, 0.3), (0.1, 0.1)] {
            let r = Ray::new(Vec3A::new(x, y, 5.0), -Vec3A::Z).with_mask(MASK_CAMERA);
            let hit = scene.world.intersect(&r, 1e-3, f32::INFINITY).unwrap();
            assert!(hit.rec.has_uv, "{what}: no chart at ({x}, {y})");
            let (u, v) = hit.rec.uv;
            assert!(
                (u - x / 2.0).abs() < 1e-4 && (v - y / 2.0).abs() < 1e-4,
                "{what}: ({x}, {y}) reads ({u}, {v})"
            );
        }
    }
}

/// Each `faceVaryingLinearInterpolation` token reaches the refiner. A 2×2
/// grid charted `(x, y) / 2`, except the interior vertex, pulled to
/// `(0.9, 0.5)`: `all` interpolates the chart linearly, so the limit keeps
/// the authored value there, while `none` smooths it back toward its
/// neighbours. (A lone quad cannot tell the rules apart — Catmull-Clark
/// reproduces bilinear data on it exactly.)
#[test]
fn every_face_varying_rule_is_read() {
    let dir = std::env::temp_dir().join("crust_usd_inline_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(
        dir.join("subdiv_rule.mtlx"),
        r#"<?xml version="1.0"?>
<materialx version="1.38">
  <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
  <surface name="s" type="surfaceshader">
    <input name="bsdf" type="BSDF" nodename="d" />
  </surface>
  <surfacematerial name="m" type="material">
    <input name="surfaceshader" type="surfaceshader" nodename="s" />
  </surfacematerial>
</materialx>
"#,
    )
    .expect("write probe mtlx");
    let probe = |rule: &str| {
        let scene = load_at_level(
            &format!("subdiv_rule_{rule}"),
            &format!(
                r#"
    def Scope "Looks" {{
        def Material "Mtl" (
            prepend references = @subdiv_rule.mtlx@</MaterialX/Materials/m>
        ) {{
        }}
    }}
    def Mesh "Quad" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        int[] faceVertexCounts = [4, 4, 4, 4]
        int[] faceVertexIndices = [0, 1, 4, 3, 1, 2, 5, 4, 3, 4, 7, 6, 4, 5, 8, 7]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (2, 0, 0), (0, 1, 0), (1, 1, 0), (2, 1, 0), (0, 2, 0), (1, 2, 0), (2, 2, 0)]
        texCoord2f[] primvars:st = [(0, 0), (0.5, 0), (1, 0), (0, 0.5), (0.9, 0.5), (1, 0.5), (0, 1), (0.5, 1), (1, 1)] (interpolation = "faceVarying")
        int[] primvars:st:indices = [0, 1, 4, 3, 1, 2, 5, 4, 3, 4, 7, 6, 4, 5, 8, 7]
        uniform token faceVaryingLinearInterpolation = "{rule}"
        uniform token subdivisionScheme = "catmullClark"
        rel material:binding = </World/Looks/Mtl>
    }}"#
            ),
            1,
        );
        let r = Ray::new(Vec3A::new(1.001, 1.001, 5.0), -Vec3A::Z).with_mask(MASK_CAMERA);
        let hit = scene.world.intersect(&r, 1e-3, f32::INFINITY).unwrap();
        assert!(hit.rec.has_uv, "{rule}: no chart");
        hit.rec.uv
    };
    let rules = [
        "none",
        "cornersOnly",
        "cornersPlus1",
        "cornersPlus2",
        "boundaries",
        "all",
    ];
    let uvs: Vec<(f32, f32)> = rules.iter().map(|r| probe(r)).collect();
    let (none, all) = (uvs[0], uvs[5]);
    assert!(
        (all.0 - 0.9).abs() < 0.01 && (all.1 - 0.5).abs() < 0.01,
        "all keeps the authored interior value: {all:?}"
    );
    assert!(
        none.0 < 0.85,
        "none smooths the interior value toward its neighbours: {none:?}"
    );
}

// ---------------------------------------------------------------------------
// Statistics and errors
// ---------------------------------------------------------------------------

#[test]
fn import_fills_the_scene_counters_and_phases() {
    let scene = load(
        "stats",
        r#"
    def Sphere "A" {}
    def Sphere "B" { double3 xformOp:translate = (3, 0, 0)
                     uniform token[] xformOpOrder = ["xformOp:translate"] }
    def SphereLight "L" { double3 xformOp:translate = (0, 5, 0)
                          uniform token[] xformOpOrder = ["xformOp:translate"] }"#,
    );
    let s = &scene.stats.scene;
    assert_eq!(s.geometries, scene.world.count());
    assert_eq!(s.geometries, 3);
    assert_eq!(s.top_level.spheres, 3);
    assert_eq!(s.lights, 1);
    assert_eq!(s.volumes, 0);
    assert!(s.footprint.total() > 0);
    assert!(
        !scene.stats.phases.is_empty(),
        "the importer records its phases"
    );
    assert!(scene.stats.total() > std::time::Duration::ZERO);
    let report = scene.stats.report();
    assert!(report.contains("geometries"));
    assert!(report.contains("Phases by execution tree"));
}

#[test]
fn a_missing_file_is_a_usd_open_error() {
    let err = Scene::from_usd(std::path::Path::new("/definitely/not/here.usda"))
        .err()
        .expect("error");
    let msg = err.to_string();
    assert!(msg.contains("failed to open USD stage"), "{msg}");
    assert!(msg.contains("here.usda"), "{msg}");
    assert!(matches!(err, crust_core::Error::UsdOpen { .. }));
}

#[test]
fn a_garbage_file_is_an_error_not_a_panic() {
    let path = write_stage("garbage", "this is not a usd layer {{{ ]]]");
    let err = Scene::from_usd(&path).err().expect("must fail");
    assert!(!err.to_string().is_empty());
}

#[test]
fn error_display_names_the_path() {
    let e = crust_core::Error::NonUtf8Path(PathBuf::from("/tmp/x.usda"));
    assert!(e.to_string().contains("/tmp/x.usda"));
    assert!(e.to_string().contains("UTF-8"));
    let e = crust_core::Error::UsdOpen {
        path: PathBuf::from("/tmp/y.usda"),
        source: "boom".into(),
    };
    assert!(e.to_string().contains("boom") && e.to_string().contains("y.usda"));
    let dyn_e: &dyn std::error::Error = &e;
    // The cause is kept as the source, not only flattened into the message.
    assert_eq!(
        dyn_e.source().map(|s| s.to_string()).as_deref(),
        Some("boom")
    );
}

#[test]
fn loading_the_same_stage_twice_is_identical() {
    let body = r#"
    def Sphere "A" { double radius = 0.7 }
    def Mesh "Q"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, -1, -2), (1, -1, -2), (1, 1, -2), (-1, 1, -2)]
    }"#;
    let a = load("twice_a", body);
    let b = load("twice_b", body);
    assert_eq!(a.world.count(), b.world.count());
    assert_eq!(a.world.primitive_breakdown(), b.world.primitive_breakdown());
    assert_eq!(a.world.memory_footprint(), b.world.memory_footprint());
    let ray = Ray::new(Vec3A::new(0.2, 0.1, 5.0), -Vec3A::Z);
    let ha = a.world.intersect(&ray, 1e-3, 100.0).unwrap();
    let hb = b.world.intersect(&ray, 1e-3, 100.0).unwrap();
    assert_eq!(ha.rec.t.to_bits(), hb.rec.t.to_bits());
    assert_eq!(ha.geom_id, hb.geom_id);
}

/// `RectLight`'s `inputs:texture:file` multiplies the emission per point:
/// the image's top row at the light's local +Y edge, its left column at −X,
/// the same texel for NEE and for a bounce ray that hits the light, and
/// `normalize` still dividing by the area.
#[test]
fn rect_light_texture_maps_onto_the_light() {
    struct Card;
    impl crust_core::AssetLoader for Card {
        fn load_environment(&self, _: &std::path::Path) -> Option<crust_core::EnvironmentMap> {
            None
        }
        fn load_light_texture(
            &self,
            path: &std::path::Path,
        ) -> Option<std::sync::Arc<crust_core::LightTexture>> {
            assert!(path.ends_with("card.exr"), "{}", path.display());
            // top-left, top-right / bottom-left, bottom-right — HDR on purpose.
            let px = vec![
                Vec3A::new(16.0, 0.0, 0.0),
                Vec3A::new(0.0, 1.0, 0.0),
                Vec3A::new(0.0, 0.0, 1.0),
                Vec3A::new(1.0, 1.0, 1.0),
            ];
            crust_core::LightTexture::new(2, 2, px).map(std::sync::Arc::new)
        }
    }
    let path = write_stage(
        "rect_texture",
        r#"#usda 1.0
def RectLight "Card"
{
    float inputs:width = 2
    float inputs:height = 2
    float inputs:intensity = 3
    bool inputs:normalize = 1
    asset inputs:texture:file = @card.exr@
    double3 xformOp:translate = (0, 0, 2)
    uniform token[] xformOpOrder = ["xformOp:translate"]
}
"#,
    );
    let scene = Scene::from_usd_with_assets(&path, &Card).unwrap();
    let light = &scene.lights.lights()[0];
    // Aim NEE at each quadrant's centre through (u, v): the rect's sample
    // parameters run along local +X and +Y from its (−X, −Y) corner.
    let from = Vec3A::ZERO;
    let at = |u: f32, v: f32| light.sample_li(from, u, v).unwrap().radiance;
    let scale = 3.0 / 4.0; // intensity over the 2×2 area
    assert_eq!(
        at(0.25, 0.75),
        Vec3A::new(16.0, 0.0, 0.0) * scale,
        "top-left"
    );
    assert_eq!(
        at(0.75, 0.75),
        Vec3A::new(0.0, 1.0, 0.0) * scale,
        "top-right"
    );
    assert_eq!(
        at(0.25, 0.25),
        Vec3A::new(0.0, 0.0, 1.0) * scale,
        "bottom-left"
    );
    assert_eq!(at(0.75, 0.25), Vec3A::ONE * scale, "bottom-right");
    assert!(assert_mis_sides_agree(&scene, from) > 0);
    assert!(assert_mis_sides_agree(&scene, Vec3A::new(0.7, -0.4, -1.0)) > 0);
}

/// A light whose size is not finite and positive is skipped whole: no light
/// without the surface a bounce ray would need to find it.
#[test]
fn invalid_light_dimensions_skip_the_light() {
    let scene = load(
        "invalid_light_sizes",
        r#"
    def DiskLight "NegativeDisk" { float inputs:radius = -1 }
    def CylinderLight "ZeroTube" { float inputs:length = 0 }
    def SphereLight "ZeroSphere" { float inputs:radius = 0 }
    def RectLight "FlatRect" { float inputs:width = 0 }
    def DiskLight "Fine" { float inputs:radius = 1 }"#,
    );
    assert_eq!(scene.lights.count(), 1, "only the valid light survives");
    assert_eq!(scene.world.count(), 1);
    assert!(assert_mis_sides_agree(&scene, Vec3A::new(0.0, 0.0, -3.0)) > 0);
}

/// A non-finite shaping input falls back rather than turning the light's
/// radiance into NaN on both MIS halves.
#[test]
fn non_finite_shaping_inputs_fall_back() {
    let scene = load(
        "nan_shaping",
        r#"
    def RectLight "Spot" (
        prepend apiSchemas = ["ShapingAPI"]
    )
    {
        float inputs:shaping:cone:angle = inf
        float inputs:shaping:cone:softness = nan
        float inputs:shaping:focus = nan
        double3 xformOp:translate = (0, 0, 3)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#,
    );
    // The cone falls back to the applied schema's 90°: straight below is lit
    // at exactly the authored radiance.
    let r = radiance_toward(&scene, Vec3A::ZERO);
    assert_eq!(r, Vec3A::ONE);
    assert!(assert_mis_sides_agree(&scene, Vec3A::new(0.3, 0.2, 0.0)) > 0);
}

// ---------------------------------------------------------------------------
// Camera visibility and lights linked to nothing
// ---------------------------------------------------------------------------

/// How many of the stage's lights at infinity a ray of category `mask`
/// sees on escaping.
fn infinite_seen(scene: &Scene, mask: crust_core::RayMask) -> usize {
    scene.lights.infinite_seen_by(Vec3A::ZERO, mask).count()
}

fn camera_hits(scene: &Scene) -> bool {
    let ray = Ray::new(Vec3A::new(0.0, 0.0, 10.0), -Vec3A::Z).with_mask(MASK_CAMERA);
    scene.world.intersect(&ray, 1e-3, 100.0).is_some()
}

fn indirect_hits(scene: &Scene) -> bool {
    let ray = Ray::new(Vec3A::new(0.0, 0.0, 10.0), -Vec3A::Z).with_mask(crust_core::MASK_INDIRECT);
    scene.world.intersect(&ray, 1e-3, 100.0).is_some()
}

#[test]
fn the_renderman_camera_primvar_hides_a_dome_from_the_camera_only() {
    let scene = load(
        "dome_ri_camera",
        r#"
    def DomeLight "Sky"
    {
        custom int primvars:ri:attributes:visibility:camera = 0
    }"#,
    );
    assert_eq!(scene.lights.count(), 1, "it still lights the scene");
    assert_eq!(infinite_seen(&scene, MASK_CAMERA), 0);
    assert_eq!(infinite_seen(&scene, crust_core::MASK_INDIRECT), 1);
}

#[test]
fn a_dome_is_camera_visible_by_default_and_crust_beats_renderman() {
    let default = load("dome_default", "    def DomeLight \"Sky\"\n    {\n    }");
    assert_eq!(infinite_seen(&default, MASK_CAMERA), 1);
    let both = load(
        "dome_crust_wins",
        r#"
    def DomeLight "Sky"
    {
        bool crust:light:cameraVisible = true
        custom int primvars:ri:attributes:visibility:camera = 0
    }"#,
    );
    assert_eq!(infinite_seen(&both, MASK_CAMERA), 1);
}

#[test]
fn the_renderman_camera_primvar_shows_an_area_light_and_crust_beats_it() {
    let shown = load(
        "sphere_ri_camera",
        r#"
    def SphereLight "L"
    {
        custom int primvars:ri:attributes:visibility:camera = 1
    }"#,
    );
    assert!(camera_hits(&shown));
    let hidden = load(
        "sphere_crust_wins",
        r#"
    def SphereLight "L"
    {
        bool crust:light:cameraVisible = false
        custom int primvars:ri:attributes:visibility:camera = 1
    }"#,
    );
    assert!(!camera_hits(&hidden));
    assert!(indirect_hits(&hidden), "still found by bounces");
}

/// The Moana rig: an HDRI whose link excludes only the other *light*, and a
/// backdrop whose link excludes every receiver.
#[test]
fn a_dome_linked_to_nothing_becomes_a_backdrop() {
    let scene = load(
        "backdrop",
        r#"
    def DomeLight "Hdri"
    {
        prepend rel collection:lightLink:excludes = </World/Backdrop>
    }
    def DomeLight "Backdrop"
    {
        prepend rel collection:lightLink:excludes = </World>
    }
    def Sphere "Ball"
    {
    }"#,
    );
    assert_eq!(scene.lights.count(), 1, "only the HDRI illuminates");
    assert_eq!(scene.lights.backdrops().len(), 1);
    assert!(scene.lights.escapes_to_backdrop(MASK_CAMERA));
    assert!(!scene.lights.escapes_to_backdrop(crust_core::MASK_INDIRECT));
}

/// The link is judged once every receiver is in, so a light traversed
/// before the geometry it excludes — and, streamed, in a different chunk
/// (four children of the root make four chunks) — is judged the same.
#[test]
fn a_link_is_judged_after_the_last_receiver() {
    let scene = load(
        "backdrop_streamed",
        r#"
    def DomeLight "Backdrop"
    {
        prepend rel collection:lightLink:excludes = </World>
    }
    def Sphere "A"
    {
    }
    def Sphere "B"
    {
    }
    def Xform "C"
    {
        def Sphere "Deep"
        {
        }
    }"#,
    );
    assert_eq!(scene.lights.count(), 0);
    assert_eq!(scene.lights.backdrops().len(), 1);
}

#[test]
fn a_partial_link_is_ignored_and_lights_everything() {
    let scene = load(
        "partial_link",
        r#"
    def DomeLight "Sky"
    {
        prepend rel collection:lightLink:excludes = </World/A>
    }
    def Sphere "A"
    {
    }
    def Sphere "B"
    {
    }"#,
    );
    assert_eq!(scene.lights.count(), 1);
    assert!(scene.lights.backdrops().is_empty());
}

#[test]
fn an_area_light_linked_to_nothing_keeps_only_its_camera_geometry() {
    let hidden = load(
        "area_nothing",
        r#"
    def SphereLight "L"
    {
        uniform bool collection:lightLink:includeRoot = 0
    }"#,
    );
    assert_eq!(hidden.lights.count(), 0);
    assert!(!camera_hits(&hidden) && !indirect_hits(&hidden));
    let shown = load(
        "area_nothing_visible",
        r#"
    def SphereLight "L"
    {
        uniform bool collection:lightLink:includeRoot = 0
        bool crust:light:cameraVisible = true
    }"#,
    );
    assert_eq!(shown.lights.count(), 0);
    assert!(camera_hits(&shown), "the camera still sees it");
    assert!(
        !indirect_hits(&shown),
        "no bounce can find it, so it lights nothing"
    );
}

#[test]
fn dome_light_camera_visibility_false_hides_every_dome_and_backdrop() {
    let scene = load_with_settings(
        "dome_camera_off",
        r#"
    def DomeLight "Hdri"
    {
    }
    def DomeLight "Backdrop"
    {
        prepend rel collection:lightLink:excludes = </World>
    }
    def Sphere "Ball"
    {
    }"#,
        r#"
def Scope "Render"
{
    def RenderSettings "settings"
    {
        bool domeLightCameraVisibility = false
    }
}"#,
    );
    assert_eq!(scene.lights.count(), 1);
    assert_eq!(infinite_seen(&scene, MASK_CAMERA), 0);
    assert_eq!(infinite_seen(&scene, crust_core::MASK_INDIRECT), 1);
    assert!(!scene.lights.escapes_to_backdrop(MASK_CAMERA));
}

#[test]
fn adaptive_neighbour_tolerance_defaults_to_one_and_refuses_nan() {
    let unauthored = load_with_settings("tolerance_default", "", SETTINGS);
    assert_eq!(unauthored.settings.adaptive_neighbour_tolerance(), 1.0);
    let nan = load_with_settings(
        "tolerance_nan",
        "",
        r#"
def Scope "Render"
{
    def RenderSettings "settings"
    {
        float crust:adaptiveNeighbourTolerance = nan
    }
}
"#,
    );
    assert_eq!(nan.settings.adaptive_neighbour_tolerance(), 1.0);
    let off = load_with_settings(
        "tolerance_off",
        "",
        r#"
def Scope "Render"
{
    def RenderSettings "settings"
    {
        float crust:adaptiveNeighbourTolerance = -1
    }
}
"#,
    );
    assert_eq!(off.settings.adaptive_neighbour_tolerance(), -1.0);
}
