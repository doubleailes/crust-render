//! USD import driven by small stages written on the fly, one feature per
//! test: prim types, transforms, render settings, materials, lights,
//! volumes, masks, subdivision and error handling. Complements
//! `usd_scene.rs`, which loads the checked-in sample files.

use crust_core::{MASK_CAMERA, Ray, SamplingStrategy, Scene, Vec3A};
use std::path::PathBuf;

/// Writes `body` (the prims under `/World`) into a fresh `.usda` and loads it.
fn load(name: &str, body: &str) -> Scene {
    load_raw(name, &format!(
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
    ))
}

/// Like `load`, with a root-level `/Render/settings` prim: the importer
/// reads `UsdRenderSettings` from the stage metadata or that conventional
/// path, never from an arbitrary location under `/World`.
fn load_with_settings(name: &str, body: &str, settings: &str) -> Scene {
    load_raw(name, &format!(
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
    ))
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
    scene.world.intersect(&Ray::new(origin, dir), 1e-3, 1e4).is_some()
}

fn hit_t(scene: &Scene, origin: Vec3A, dir: Vec3A) -> Option<f32> {
    scene.world.intersect(&Ray::new(origin, dir), 1e-3, 1e4).map(|h| h.rec.t)
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
    assert_eq!(scene.settings.sampling_strategy(), SamplingStrategy::PowerMis);
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
    assert!(hits(&scene, Vec3A::new(0.5, 0.7, 5.0), -Vec3A::Z), "inside the pentagon");
    assert!(hits(&scene, Vec3A::new(3.5, 0.3, 5.0), -Vec3A::Z), "inside the triangle");
    assert!(hits(&scene, Vec3A::new(6.5, 0.5, 5.0), -Vec3A::Z), "inside the quad");
    assert!(!hits(&scene, Vec3A::new(2.2, 0.5, 5.0), -Vec3A::Z), "the gap between them");
}

#[test]
fn a_mesh_missing_its_arrays_is_skipped_not_fatal() {
    let scene = load(
        "broken_mesh",
        r#"
    def Mesh "NoPoints"
    {
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
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)]
        double3 xformOp:translate = ({x}, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}"#
        )
    };
    let once = load("place_once", &mesh("A", 0.0));
    assert_eq!(once.world.primitive_breakdown().triangles, 2, "a single placement is baked flat");
    assert_eq!(once.world.primitive_breakdown().instances, 0);

    let twice = load("place_twice", &format!("{}{}", mesh("A", -3.0), mesh("B", 3.0)));
    assert_eq!(twice.world.count(), 2);
    let br = twice.world.primitive_breakdown();
    assert_eq!(br.instances, 2, "two placements of one mesh share a prototype");
    assert_eq!(br.triangles, 0);
    assert_eq!(twice.world.unique_primitive_breakdown().triangles, 2, "one resident copy");
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
    assert!(hits(&scene, Vec3A::new(-2.0, 0.0, 10.0), -Vec3A::Z), "child should land at x = -2");
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
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)]
        }
    }"#,
    );
    let bb = scene.world.bounds().unwrap();
    // Translation stays (0, 4, 0); the quad is scaled to ±2 around it.
    assert!((bb.minimum.y - 2.0).abs() < 1e-3 && (bb.maximum.y - 6.0).abs() < 1e-3, "{bb:?}");
    assert!((bb.minimum.x + 2.0).abs() < 1e-3 && (bb.maximum.x - 2.0).abs() < 1e-3, "{bb:?}");
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
    assert_eq!(scene.settings.sampling_strategy(), SamplingStrategy::BalanceMis);
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
    assert_eq!(scene.settings.sampling_strategy(), SamplingStrategy::PowerMis);
}

#[test]
fn image_counters_mirror_the_settings() {
    let scene = load_with_settings("counters", "", SETTINGS);
    assert_eq!((scene.stats.image.width, scene.stats.image.height), (32, 16));
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
    let hit = scene.world.intersect(&ray, 1e-3, 100.0).expect("the centre ray hits the ball");
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
    let d = scene.camera.get_ray(0.5, 0.5, [0.5, 0.5], 0.0).direction().normalize();
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
        let d = s.camera.get_ray(1.0, 0.5, [0.5, 0.5], 0.0).direction().normalize();
        d.dot(-Vec3A::Z).acos()
    };
    assert!(edge(&wide) > 2.0 * edge(&narrow), "{} vs {}", edge(&wide), edge(&narrow));
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
    assert!(!a.origin().abs_diff_eq(b.origin(), 1e-6), "lens samples must move the origin");
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
    assert!(scene.world.material(0).emitted().abs_diff_eq(Vec3A::new(3.0, 1.5, 0.75), 1e-5));
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
    assert!(scene.world.material(0).emitted().abs_diff_eq(Vec3A::new(2.0, 1.0, 0.5), 1e-5));
    assert_eq!(scene.lights.lights[0].geom_id(), Some(0));
    let ray = Ray::new(Vec3A::new(0.0, 5.0, 10.0), -Vec3A::Z);
    assert!(scene.world.intersect(&ray.clone().with_mask(MASK_CAMERA), 1e-3, 100.0).is_none(), "camera-invisible by default");
    assert!(scene.world.intersect(&ray, 1e-3, 100.0).is_some(), "other rays see it");
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
    assert!(scene.world.material(0).emitted().abs_diff_eq(Vec3A::splat(8.0), 1e-5));
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
    assert!(scene.world.intersect(&at_light.clone().with_mask(MASK_CAMERA), 1e-3, 100.0).is_some());
    assert!(scene.world.intersect(&at_light.with_mask(crust_core::MASK_SHADOW), 1e-3, 100.0).is_none());
    // The sphere with mask 6 hides from the camera only.
    let at_sphere = Ray::new(Vec3A::new(5.0, 0.0, 10.0), -Vec3A::Z);
    assert!(scene.world.intersect(&at_sphere.clone().with_mask(MASK_CAMERA), 1e-3, 100.0).is_none());
    assert!(scene.world.intersect(&at_sphere.with_mask(crust_core::MASK_SHADOW), 1e-3, 100.0).is_some());
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
    assert!((bb.maximum.x - 1.0).abs() < 1e-3 && (bb.maximum.y - 0.5).abs() < 1e-3, "{bb:?}");
    // It emits along local -Z: a point below it on -Z is lit.
    let s = scene.lights.lights[0].sample_li(Vec3A::new(0.0, 0.0, 0.0), 0.5, 0.5).expect("reachable");
    assert!(s.direction.z > 0.99);
    assert!(s.pdf.is_finite() && s.pdf < 1e3, "the emitting side faces the point");
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
    for l in &scene.lights.lights {
        assert!(l.geom_id().is_none());
    }
    // The dome answers every escaping ray with its colour.
    let dome = scene.lights.lights.iter().find(|l| l.escaped(Vec3A::ZERO, Vec3A::X).is_some()).expect("dome");
    let (r, _) = dome.escaped(Vec3A::ZERO, Vec3A::new(0.3, -0.2, 0.9).normalize()).unwrap();
    assert!(r.abs_diff_eq(Vec3A::new(0.2, 0.3, 0.4), 1e-5));
    // rotateX(-90) turns local -Z into -Y: the sun shines straight down, so
    // a ray escaping straight up finds it.
    let sun = scene.lights.lights.iter().find(|l| l.escaped(Vec3A::ZERO, Vec3A::Y).is_some_and(|(r, _)| r.x > 1.0)).expect("sun overhead");
    assert!(sun.escaped(Vec3A::ZERO, -Vec3A::Y).is_none());
}

#[test]
fn unsupported_light_types_are_skipped() {
    let scene = load(
        "disk_light",
        r#"
    def DiskLight "D" { float inputs:radius = 1 }
    def CylinderLight "C" {}
    def Sphere "Ball" {}"#,
    );
    assert_eq!(scene.lights.count(), 0);
    assert_eq!(scene.world.count(), 1);
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
    assert!(v.sigma_s.abs_diff_eq(Vec3A::splat(1.0), 1e-6), "densityScale folds in: {}", v.sigma_s);
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

#[test]
fn subdivision_level_multiplies_faces_by_four() {
    let at_level = |level: u32| {
        load(
            &format!("subdiv_{level}"),
            &format!(
                r#"
    def Mesh "Cube"
    {{
{CUBE}
        int crust:subdivisionLevel = {level}
    }}"#
            ),
        )
        .world
        .primitive_breakdown()
        .triangles
    };
    let l0 = at_level(0);
    assert_eq!(l0, 12);
    assert_eq!(at_level(1), 4 * l0);
    assert_eq!(at_level(2), 16 * l0);
}

#[test]
fn a_subdivided_cube_shrinks_toward_a_sphere() {
    let cage = load("subdiv_cage", &format!(r#"
    def Mesh "Cube" {{ {CUBE} }}"#));
    let smooth = load(
        "subdiv_smooth",
        &format!(
            r#"
    def Mesh "Cube"
    {{
{CUBE}
        int crust:subdivisionLevel = 3
    }}"#
        ),
    );
    let corner = Vec3A::splat(0.95);
    assert!(hits(&cage, corner + Vec3A::Z * 5.0, -Vec3A::Z), "the cage fills its corner");
    // Catmull-Clark pulls the corners well inside the cube.
    let cage_t = hit_t(&cage, Vec3A::new(0.0, 0.0, 5.0), -Vec3A::Z).unwrap();
    let smooth_t = hit_t(&smooth, Vec3A::new(0.0, 0.0, 5.0), -Vec3A::Z).unwrap();
    assert!(smooth_t > cage_t, "the limit surface sits inside the cage: {smooth_t} vs {cage_t}");
    let bb = smooth.world.bounds().unwrap();
    assert!(bb.maximum.x < 1.0 && bb.maximum.x > 0.5, "{bb:?}");
    // Smooth shading normals are unit length and not axis-aligned at an
    // oblique hit.
    let hit = smooth
        .world
        .intersect(&Ray::new(Vec3A::new(0.4, 0.4, 5.0), -Vec3A::Z), 1e-3, 100.0)
        .unwrap();
    assert!((hit.rec.normal.length() - 1.0).abs() < 1e-3);
    assert!(hit.rec.normal.x > 0.05 && hit.rec.normal.y > 0.05, "{}", hit.rec.normal);
}

#[test]
fn subdivision_scheme_none_and_clamping() {
    let none = load(
        "subdiv_none",
        &format!(
            r#"
    def Mesh "Cube"
    {{
{CUBE}
        uniform token subdivisionScheme = "none"
        int crust:subdivisionLevel = 2
    }}"#
        ),
    );
    assert_eq!(none.world.primitive_breakdown().triangles, 12, "scheme none renders the cage");
    let huge = load(
        "subdiv_clamped",
        &format!(
            r#"
    def Mesh "Cube"
    {{
{CUBE}
        int crust:subdivisionLevel = 40
    }}"#
        ),
    );
    // Clamped to level 6: 12 · 4^6.
    assert_eq!(huge.world.primitive_breakdown().triangles, 12 * 4096);
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
    assert!(!scene.stats.phases.is_empty(), "the importer records its phases");
    assert!(scene.stats.total() > std::time::Duration::ZERO);
    let report = scene.stats.report();
    assert!(report.contains("geometries"));
    assert!(report.contains("Profile by execution tree"));
}

#[test]
fn a_missing_file_is_a_usd_open_error() {
    let err = Scene::from_usd(std::path::Path::new("/definitely/not/here.usda")).err().expect("error");
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
    let e = crust_core::Error::UsdOpen { path: PathBuf::from("/tmp/y.usda"), message: "boom".into() };
    assert!(e.to_string().contains("boom") && e.to_string().contains("y.usda"));
    let _: &dyn std::error::Error = &e;
}

#[test]
fn loading_the_same_stage_twice_is_identical() {
    let body = r#"
    def Sphere "A" { double radius = 0.7 }
    def Mesh "Q"
    {
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
