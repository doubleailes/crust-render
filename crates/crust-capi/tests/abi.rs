//! Drives the extern "C" surface exactly as a C host would — real pointers,
//! real status codes — and pins the boundary's contracts: error paths,
//! progressive determinism through the ABI, AOV sentinels, stop/resume.
//!
//! Runs under the test profile, where Cargo forces `panic = "unwind"`, so
//! the workspace's release `panic = "abort"` does not affect these tests.

use crust_capi::*;
use std::ptr;

/// Column-major world-to-view: camera at (0,0,3) looking down -Z.
const VIEW: [f64; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, -3.0, 1.0,
];

/// Column-major GL perspective, 45° vertical fov, square aspect.
fn perspective() -> [f64; 16] {
    let f = 1.0 / (45.0f64.to_radians() / 2.0).tan();
    let (n, fa) = (0.1, 100.0);
    [
        f, 0.0, 0.0, 0.0, //
        0.0, f, 0.0, 0.0, //
        0.0, 0.0, -(fa + n) / (fa - n), -1.0, //
        0.0, 0.0, -2.0 * fa * n / (fa - n), 0.0,
    ]
}

/// A quad covering the view center, lit by a sphere light — enough for
/// nonzero pixels, hit/miss AOV coverage, and a stable geom_id.
struct TestScene {
    scene: *mut SceneHandle,
    quad_id: u32,
}

fn build_scene(spp: u32) -> TestScene {
    // SAFETY: this test drives the C ABI with locally-owned, valid,
    // exclusively-held pointers and correct array lengths - precisely
    // the crust.h contract the exports' `# Safety` sections require.
    unsafe {
    let scene = crust_scene_create();
    assert!(!scene.is_null());

    let mut material = std::mem::zeroed::<CrustMaterial>();
    crust_material_default(&mut material);
    material.base_color = [0.8, 0.4, 0.2];

    // Two triangles spanning [-0.6, 0.6]^2 at z = 0 (the 45° frustum at
    // distance 3 spans ±1.24 there, so corners of the image miss).
    let positions: [f32; 12] = [
        -0.6, -0.6, 0.0, //
        0.6, -0.6, 0.0, //
        0.6, 0.6, 0.0, //
        -0.6, 0.6, 0.0,
    ];
    let indices: [u32; 6] = [0, 1, 2, 0, 2, 3];
    let mut quad_id = u32::MAX;
    assert_eq!(
        crust_scene_add_mesh(
            scene,
            positions.as_ptr(),
            4,
            indices.as_ptr(),
            2,
            ptr::null(),
            &material,
            &mut quad_id,
        ),
        CrustStatus::Ok
    );
    assert_eq!(quad_id, 0, "first geometry gets the first id");

    let status = crust_scene_add_sphere_light(
        scene,
        [0.0f32, 0.0, 2.0].as_ptr(),
        0.5,
        [12.0f32, 12.0, 12.0].as_ptr(),
    );
    assert_eq!(status, CrustStatus::Ok);

    assert_eq!(
        crust_scene_set_camera(scene, VIEW.as_ptr(), perspective().as_ptr(), 0.0, 3.0),
        CrustStatus::Ok
    );

    let mut settings = std::mem::zeroed::<CrustRenderSettings>();
    crust_render_settings_default(&mut settings);
    settings.width = 32;
    settings.height = 32;
    settings.samples_per_pixel = spp;
    settings.max_depth = 4;
    assert_eq!(
        crust_scene_set_render_settings(scene, &settings),
        CrustStatus::Ok
    );

    TestScene { scene, quad_id }
    }
}

fn commit(scene: *mut SceneHandle, token: *const TokenHandle) -> *mut RendererHandle {
    // SAFETY: this test drives the C ABI with locally-owned, valid,
    // exclusively-held pointers and correct array lengths - precisely
    // the crust.h contract the exports' `# Safety` sections require.
    unsafe {
    let mut renderer: *mut RendererHandle = ptr::null_mut();
    assert_eq!(
        crust_scene_commit(scene, token, &mut renderer),
        CrustStatus::Ok
    );
    assert!(!renderer.is_null());
    renderer
    }
}

fn read_rgba(renderer: *mut RendererHandle) -> Vec<f32> {
    // SAFETY: this test drives the C ABI with locally-owned, valid,
    // exclusively-held pointers and correct array lengths - precisely
    // the crust.h contract the exports' `# Safety` sections require.
    unsafe {
    let mut out = vec![0.0f32; 32 * 32 * 4];
    assert_eq!(
        crust_renderer_read_color(renderer, out.as_mut_ptr(), 32 * 32),
        CrustStatus::Ok
    );
    out
    }
}

#[test]
fn renders_deterministically_through_the_abi() {
    // SAFETY: this test drives the C ABI with locally-owned, valid,
    // exclusively-held pointers and correct array lengths - precisely
    // the crust.h contract the exports' `# Safety` sections require.
    unsafe {
    let run = || -> Vec<f32> {
        let ts = build_scene(8);
        let renderer = commit(ts.scene, ptr::null());
        // Uneven chunks on purpose: chunking must not change the result.
        let mut status = CrustStepStatus::InProgress;
        let mut spp_done = 0u32;
        for chunk in [3u32, 1, 4, 8] {
            if status == CrustStepStatus::Complete {
                break;
            }
            assert_eq!(
                crust_renderer_step(renderer, chunk, &mut status, &mut spp_done),
                CrustStatus::Ok
            );
        }
        assert_eq!(status, CrustStepStatus::Complete);
        assert_eq!(spp_done, 8);
        assert!(crust_renderer_is_converged(renderer));
        assert_eq!(crust_renderer_spp_done(renderer), 8);

        let rgba = read_rgba(renderer);
        crust_renderer_destroy(renderer);
        crust_scene_destroy(ts.scene);
        rgba
    };

    let a = run();
    let b = run();
    assert!(
        a.iter().any(|v| *v > 0.0),
        "image is all black — the light or camera wiring is broken"
    );
    assert!(a.iter().all(|v| v.is_finite()));
    let bits = |v: &[f32]| v.iter().map(|f| f.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&a), bits(&b), "two identical runs must be bit-identical");
    }
}

#[test]
fn aov_planes_report_hits_and_misses() {
    // SAFETY: this test drives the C ABI with locally-owned, valid,
    // exclusively-held pointers and correct array lengths - precisely
    // the crust.h contract the exports' `# Safety` sections require.
    unsafe {
    let ts = build_scene(2);
    let renderer = commit(ts.scene, ptr::null());
    let mut status = CrustStepStatus::InProgress;
    let mut done = 0u32;
    assert_eq!(
        crust_renderer_step(renderer, 2, &mut status, &mut done),
        CrustStatus::Ok
    );

    let mut width = 0u32;
    let mut height = 0u32;
    crust_renderer_get_dimensions(renderer, &mut width, &mut height);
    assert_eq!((width, height), (32, 32));
    let px = (width * height) as usize;
    let center = (16 * 32 + 16) as usize;
    let corner = 0usize;

    let mut depth = vec![0.0f32; px];
    let mut normal = vec![0.0f32; px * 3];
    let mut ids = vec![0u32; px * 2];
    let mut alpha = vec![0.0f32; px];
    assert_eq!(
        crust_renderer_read_aov_depth(renderer, depth.as_mut_ptr(), px),
        CrustStatus::Ok
    );
    assert_eq!(
        crust_renderer_read_aov_normal(renderer, normal.as_mut_ptr(), px),
        CrustStatus::Ok
    );
    assert_eq!(
        crust_renderer_read_aov_id(renderer, ids.as_mut_ptr(), px),
        CrustStatus::Ok
    );
    assert_eq!(
        crust_renderer_read_aov_alpha(renderer, alpha.as_mut_ptr(), px),
        CrustStatus::Ok
    );

    assert_eq!(alpha[center], 1.0);
    assert_eq!(alpha[corner], 0.0);
    assert!(
        (depth[center] - 3.0).abs() < 0.05,
        "quad at z=0 seen from z=3: depth {} != 3",
        depth[center]
    );
    assert_eq!(depth[corner], f32::INFINITY);
    // The quad faces the camera: outward normal is +Z.
    assert!((normal[center * 3 + 2] - 1.0).abs() < 0.05);
    assert_eq!([ids[center * 2], ids[center * 2 + 1]], [ts.quad_id, 1]);
    assert_eq!(
        [ids[corner * 2], ids[corner * 2 + 1]],
        [u32::MAX, u32::MAX]
    );

    // Color alpha channel mirrors the alpha AOV.
    let rgba = read_rgba(renderer);
    assert_eq!(rgba[center * 4 + 3], 1.0);
    assert_eq!(rgba[corner * 4 + 3], 0.0);

    crust_renderer_destroy(renderer);
    crust_scene_destroy(ts.scene);
    }
}

#[test]
fn stop_token_interrupts_and_resumes() {
    // SAFETY: this test drives the C ABI with locally-owned, valid,
    // exclusively-held pointers and correct array lengths - precisely
    // the crust.h contract the exports' `# Safety` sections require.
    unsafe {
    let ts = build_scene(8);
    let token = crust_stop_token_create();
    assert!(!crust_stop_token_is_stopped(token));
    let renderer = commit(ts.scene, token);

    // Fire before the first row: the step is cut short immediately.
    crust_stop_token_stop(token);
    assert!(crust_stop_token_is_stopped(token));
    let mut status = CrustStepStatus::InProgress;
    let mut done = 99u32;
    assert_eq!(
        crust_renderer_step(renderer, 8, &mut status, &mut done),
        CrustStatus::Ok
    );
    assert_eq!(status, CrustStepStatus::Stopped);
    assert_eq!(done, 0);
    assert!(!crust_renderer_is_converged(renderer));
    // The partial image is readable and coherent (all black here).
    let rgba = read_rgba(renderer);
    assert!(rgba.iter().all(|v| v.is_finite()));

    // A stopped token is permanent — build the same scene with a fresh
    // token and confirm it completes; also proves the reference render.
    let ts2 = build_scene(8);
    let token2 = crust_stop_token_create();
    let renderer2 = commit(ts2.scene, token2);
    let mut status2 = CrustStepStatus::InProgress;
    let mut done2 = 0u32;
    while status2 != CrustStepStatus::Complete {
        assert_eq!(
            crust_renderer_step(renderer2, 4, &mut status2, &mut done2),
            CrustStatus::Ok
        );
    }
    assert!(crust_renderer_is_converged(renderer2));

    crust_renderer_destroy(renderer);
    crust_renderer_destroy(renderer2);
    crust_scene_destroy(ts.scene);
    crust_scene_destroy(ts2.scene);
    crust_stop_token_destroy(token);
    crust_stop_token_destroy(token2);
    }
}

/// Builds the standard test scene with the quad placed through the
/// geometry cache as an instance. `arrays` controls whether the vertex
/// data is supplied (a cache hit does not need it).
fn build_instanced_scene(
    cache: *mut GeoCacheHandle,
    version: u32,
    arrays: bool,
    xform: &[f64; 16],
) -> *mut SceneHandle {
    // SAFETY: this test drives the C ABI with locally-owned, valid,
    // exclusively-held pointers and correct array lengths - precisely
    // the crust.h contract the exports' `# Safety` sections require.
    unsafe {
    let scene = crust_scene_create();
    let mut material = std::mem::zeroed::<CrustMaterial>();
    crust_material_default(&mut material);
    material.base_color = [0.8, 0.4, 0.2];

    let positions: [f32; 12] = [
        -0.6, -0.6, 0.0, //
        0.6, -0.6, 0.0, //
        0.6, 0.6, 0.0, //
        -0.6, 0.6, 0.0,
    ];
    let indices: [u32; 6] = [0, 1, 2, 0, 2, 3];
    let mut id = u32::MAX;
    let status = crust_scene_add_instance(
        scene,
        cache,
        0xC0FFEE,
        version,
        if arrays { positions.as_ptr() } else { ptr::null() },
        4,
        if arrays { indices.as_ptr() } else { ptr::null() },
        2,
        ptr::null(),
        xform.as_ptr(),
        &material,
        &mut id,
    );
    assert_eq!(status, CrustStatus::Ok);

    assert_eq!(
        crust_scene_add_sphere_light(
            scene,
            [0.0f32, 0.0, 2.0].as_ptr(),
            0.5,
            [12.0f32, 12.0, 12.0].as_ptr(),
        ),
        CrustStatus::Ok
    );
    assert_eq!(
        crust_scene_set_camera(scene, VIEW.as_ptr(), perspective().as_ptr(), 0.0, 3.0),
        CrustStatus::Ok
    );
    let mut settings = std::mem::zeroed::<CrustRenderSettings>();
    crust_render_settings_default(&mut settings);
    settings.width = 32;
    settings.height = 32;
    settings.samples_per_pixel = 8;
    settings.max_depth = 4;
    assert_eq!(
        crust_scene_set_render_settings(scene, &settings),
        CrustStatus::Ok
    );
    scene
    }
}

fn render_to_completion(renderer: *mut RendererHandle) -> Vec<f32> {
    // SAFETY: this test drives the C ABI with locally-owned, valid,
    // exclusively-held pointers and correct array lengths - precisely
    // the crust.h contract the exports' `# Safety` sections require.
    unsafe {
    let mut status = CrustStepStatus::InProgress;
    let mut done = 0u32;
    while status != CrustStepStatus::Complete {
        assert_eq!(
            crust_renderer_step(renderer, 4, &mut status, &mut done),
            CrustStatus::Ok
        );
    }
    read_rgba(renderer)
    }
}

const IDENTITY: [f64; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

/// The geometry cache must be invisible in the image: a cache-hit rebuild
/// (NULL vertex arrays) renders bit-identically to the miss that populated
/// it, and version bumps invalidate.
#[test]
fn geo_cache_hits_render_bit_identical() {
    // SAFETY: this test drives the C ABI with locally-owned, valid,
    // exclusively-held pointers and correct array lengths - precisely
    // the crust.h contract the exports' `# Safety` sections require.
    unsafe {
    let cache = crust_geo_cache_create();
    assert!(!crust_geo_cache_contains(cache, 0xC0FFEE, 1));

    // Miss: arrays supplied, prototype committed and cached.
    let scene_a = build_instanced_scene(cache, 1, true, &IDENTITY);
    assert!(crust_geo_cache_contains(cache, 0xC0FFEE, 1));
    assert!(!crust_geo_cache_contains(cache, 0xC0FFEE, 2));
    let renderer_a = commit(scene_a, ptr::null());
    let a = render_to_completion(renderer_a);

    // Hit: NULL arrays, same key+version, same everything else.
    let scene_b = build_instanced_scene(cache, 1, false, &IDENTITY);
    let renderer_b = commit(scene_b, ptr::null());
    let b = render_to_completion(renderer_b);

    assert!(a.iter().any(|v| *v > 0.0));
    let bits = |v: &[f32]| v.iter().map(|f| f.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&a), bits(&b), "cache hit must not change the image");

    // A miss with NULL arrays is an error (version bumped, no data).
    let scene_c = crust_scene_create();
    let mut material = std::mem::zeroed::<CrustMaterial>();
    crust_material_default(&mut material);
    let mut id = 0u32;
    assert_eq!(
        crust_scene_add_instance(
            scene_c,
            cache,
            0xC0FFEE,
            2,
            ptr::null(),
            4,
            ptr::null(),
            2,
            ptr::null(),
            IDENTITY.as_ptr(),
            &material,
            &mut id,
        ),
        CrustStatus::NullArgument
    );
    // A singular placement is rejected.
    let zero_scale: [f64; 16] = [0.0; 16];
    let positions: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    let indices: [u32; 3] = [0, 1, 2];
    assert_eq!(
        crust_scene_add_instance(
            scene_c,
            cache,
            7,
            1,
            positions.as_ptr(),
            3,
            indices.as_ptr(),
            1,
            ptr::null(),
            zero_scale.as_ptr(),
            &material,
            &mut id,
        ),
        CrustStatus::InvalidArgument
    );
    crust_scene_destroy(scene_c);

    // Removal forgets the key; clear forgets everything.
    crust_geo_cache_remove(cache, 0xC0FFEE);
    assert!(!crust_geo_cache_contains(cache, 0xC0FFEE, 1));
    crust_geo_cache_clear(cache);

    // Renderers keep their prototypes alive through the Arc regardless.
    let a2 = read_rgba(renderer_a);
    assert_eq!(bits(&a), bits(&a2));

    crust_renderer_destroy(renderer_a);
    crust_renderer_destroy(renderer_b);
    crust_scene_destroy(scene_a);
    crust_scene_destroy(scene_b);
    crust_geo_cache_destroy(cache);
    crust_geo_cache_destroy(ptr::null_mut()); // NULL is a no-op
    }
}

/// In-place camera and settings edits restart sampling without a rebuild
/// and land on exactly the image a fresh build would produce.
#[test]
fn in_place_edits_match_fresh_builds() {
    // SAFETY: this test drives the C ABI with locally-owned, valid,
    // exclusively-held pointers and correct array lengths - precisely
    // the crust.h contract the exports' `# Safety` sections require.
    unsafe {
    // Reference: fresh build with the SHIFTED camera.
    let mut shifted_view = VIEW;
    shifted_view[12] = -0.4; // translate x
    let ts_ref = build_scene(8);
    assert_eq!(
        crust_scene_set_camera(
            ts_ref.scene,
            shifted_view.as_ptr(),
            perspective().as_ptr(),
            0.0,
            3.0
        ),
        CrustStatus::Ok
    );
    let renderer_ref = commit(ts_ref.scene, ptr::null());
    let reference = render_to_completion(renderer_ref);

    // Edited: build with the ORIGINAL camera, render some, then update.
    let ts = build_scene(8);
    let renderer = commit(ts.scene, ptr::null());
    let mut status = CrustStepStatus::InProgress;
    let mut done = 0u32;
    assert_eq!(
        crust_renderer_step(renderer, 3, &mut status, &mut done),
        CrustStatus::Ok
    );
    let token = crust_stop_token_create();
    assert_eq!(
        crust_renderer_update_camera(
            renderer,
            shifted_view.as_ptr(),
            perspective().as_ptr(),
            0.0,
            3.0,
            token,
        ),
        CrustStatus::Ok
    );
    // Progress restarted from zero.
    assert_eq!(crust_renderer_spp_done(renderer), 0);
    assert!(!crust_renderer_is_converged(renderer));
    let edited = render_to_completion(renderer);

    let bits = |v: &[f32]| v.iter().map(|f| f.to_bits()).collect::<Vec<_>>();
    assert!(reference.iter().any(|v| *v > 0.0));
    assert_eq!(
        bits(&reference),
        bits(&edited),
        "update_camera must land on the fresh-build image"
    );

    // Settings edit: resolution change resizes the outputs.
    let mut settings = std::mem::zeroed::<CrustRenderSettings>();
    crust_render_settings_default(&mut settings);
    settings.width = 16;
    settings.height = 16;
    settings.samples_per_pixel = 4;
    settings.max_depth = 4;
    assert_eq!(
        crust_renderer_update_settings(renderer, &settings, ptr::null()),
        CrustStatus::Ok
    );
    let mut w = 0u32;
    let mut h = 0u32;
    crust_renderer_get_dimensions(renderer, &mut w, &mut h);
    assert_eq!((w, h), (16, 16));
    let mut small = vec![0.0f32; 16 * 16 * 4];
    let mut st = CrustStepStatus::InProgress;
    while st != CrustStepStatus::Complete {
        assert_eq!(
            crust_renderer_step(renderer, 2, &mut st, &mut done),
            CrustStatus::Ok
        );
    }
    assert_eq!(
        crust_renderer_read_color(renderer, small.as_mut_ptr(), 16 * 16),
        CrustStatus::Ok
    );
    assert!(small.iter().all(|v| v.is_finite()));

    // Error paths.
    let ortho: [f64; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, -0.02, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];
    assert_eq!(
        crust_renderer_update_camera(
            renderer,
            VIEW.as_ptr(),
            ortho.as_ptr(),
            0.0,
            3.0,
            ptr::null()
        ),
        CrustStatus::InvalidCamera
    );

    crust_stop_token_destroy(token);
    crust_renderer_destroy(renderer);
    crust_renderer_destroy(renderer_ref);
    crust_scene_destroy(ts.scene);
    crust_scene_destroy(ts_ref.scene);
    }
}

/// The file-based dome light decodes real image files with the CLI's
/// semantics (LDR -> linear), errors cleanly on bad paths, and the v2
/// material defaults carry the coat fields.
#[test]
fn dome_light_file_and_material_v2() {
    // SAFETY: this test drives the C ABI with locally-owned, valid,
    // exclusively-held pointers and correct array lengths - precisely
    // the crust.h contract the exports' `# Safety` sections require.
    unsafe {
    let mut material = std::mem::zeroed::<CrustMaterial>();
    crust_material_default(&mut material);
    assert_eq!(material.coat_weight, 0.0);
    assert_eq!(material.coat_roughness, 0.0);

    // A 2x2 white PNG in the temp dir — sRGB 255 decodes to linear 1.0.
    let png_path = std::env::temp_dir().join("crust_capi_dome_test.png");
    image::RgbImage::from_pixel(2, 2, image::Rgb([255u8, 255, 255]))
        .save(&png_path)
        .expect("write test png");
    let png_cstr = std::ffi::CString::new(png_path.to_str().unwrap()).unwrap();

    let scene = crust_scene_create();
    let identity9: [f32; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let tint = [0.5f32, 0.5, 0.5];
    assert_eq!(
        crust_scene_add_dome_light_file(
            scene,
            tint.as_ptr(),
            png_cstr.as_ptr(),
            identity9.as_ptr()
        ),
        CrustStatus::Ok
    );
    // Error paths: missing file, NULL path.
    let missing = std::ffi::CString::new("/nonexistent/nowhere.exr").unwrap();
    assert_eq!(
        crust_scene_add_dome_light_file(
            scene,
            tint.as_ptr(),
            missing.as_ptr(),
            identity9.as_ptr()
        ),
        CrustStatus::InvalidArgument
    );
    assert_eq!(
        crust_scene_add_dome_light_file(
            scene,
            tint.as_ptr(),
            ptr::null(),
            identity9.as_ptr()
        ),
        CrustStatus::NullArgument
    );

    // Coated sphere under the textured dome: renders finite and nonzero
    // (the dome replaces the sky, so escaping rays return tint * texel).
    material.coat_weight = 0.5;
    material.coat_roughness = 0.1;
    let mut id = 0u32;
    assert_eq!(
        crust_scene_add_sphere(scene, [0.0f32, 0.0, 0.0].as_ptr(), 1.0, &material, &mut id),
        CrustStatus::Ok
    );
    assert_eq!(
        crust_scene_set_camera(scene, VIEW.as_ptr(), perspective().as_ptr(), 0.0, 3.0),
        CrustStatus::Ok
    );
    let mut settings = std::mem::zeroed::<CrustRenderSettings>();
    crust_render_settings_default(&mut settings);
    settings.width = 16;
    settings.height = 16;
    settings.samples_per_pixel = 4;
    settings.max_depth = 4;
    assert_eq!(
        crust_scene_set_render_settings(scene, &settings),
        CrustStatus::Ok
    );
    let renderer = commit(scene, ptr::null());
    let mut status = CrustStepStatus::InProgress;
    let mut done = 0u32;
    while status != CrustStepStatus::Complete {
        assert_eq!(
            crust_renderer_step(renderer, 2, &mut status, &mut done),
            CrustStatus::Ok
        );
    }
    let mut rgba = vec![0.0f32; 16 * 16 * 4];
    assert_eq!(
        crust_renderer_read_color(renderer, rgba.as_mut_ptr(), 16 * 16),
        CrustStatus::Ok
    );
    assert!(rgba.iter().all(|v| v.is_finite()));
    // A corner pixel escapes to the dome: white texel * 0.5 tint = 0.5.
    assert!((rgba[0] - 0.5).abs() < 1e-3, "corner = {}", rgba[0]);

    crust_renderer_destroy(renderer);
    crust_scene_destroy(scene);
    let _ = std::fs::remove_file(&png_path);
    }
}

#[test]
fn error_paths_return_their_exact_status() {
    // SAFETY: this test drives the C ABI with locally-owned, valid,
    // exclusively-held pointers and correct array lengths - precisely
    // the crust.h contract the exports' `# Safety` sections require.
    unsafe {
    // Null handles.
    let mut out_id = 0u32;
    let material = {
        let mut m = std::mem::zeroed::<CrustMaterial>();
        crust_material_default(&mut m);
        m
    };
    let p = [0.0f32; 3];
    assert_eq!(
        crust_scene_add_sphere(ptr::null_mut(), p.as_ptr(), 1.0, &material, &mut out_id),
        CrustStatus::NullArgument
    );

    let scene = crust_scene_create();
    // Null required arrays.
    assert_eq!(
        crust_scene_add_mesh(scene, ptr::null(), 3, ptr::null(), 1, ptr::null(), &material, &mut out_id),
        CrustStatus::NullArgument
    );
    // Invalid values.
    assert_eq!(
        crust_scene_add_sphere(scene, p.as_ptr(), -1.0, &material, &mut out_id),
        CrustStatus::InvalidArgument
    );
    assert_eq!(
        crust_scene_add_sphere(scene, [f32::NAN, 0.0, 0.0].as_ptr(), 1.0, &material, &mut out_id),
        CrustStatus::InvalidArgument
    );

    // Commit without camera/settings.
    let mut renderer: *mut RendererHandle = ptr::null_mut();
    assert_eq!(
        crust_scene_commit(scene, ptr::null(), &mut renderer),
        CrustStatus::BadState
    );

    // Orthographic projection is rejected.
    let ortho: [f64; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, -0.02, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];
    assert_eq!(
        crust_scene_set_camera(scene, VIEW.as_ptr(), ortho.as_ptr(), 0.0, 3.0),
        CrustStatus::InvalidCamera
    );
    crust_scene_destroy(scene);

    // Use-after-commit and read-capacity checks on a real render.
    let ts = build_scene(2);
    let renderer = commit(ts.scene, ptr::null());
    assert_eq!(
        crust_scene_add_sphere(ts.scene, p.as_ptr(), 1.0, &material, &mut out_id),
        CrustStatus::BadState,
        "a committed scene is spent"
    );
    let mut rgba = vec![0.0f32; 8];
    assert_eq!(
        crust_renderer_read_color(renderer, rgba.as_mut_ptr(), 2),
        CrustStatus::BufferTooSmall
    );
    let mut status = CrustStepStatus::InProgress;
    assert_eq!(
        crust_renderer_step(renderer, 0, &mut status, ptr::null_mut()),
        CrustStatus::InvalidArgument
    );
    crust_renderer_destroy(renderer);
    crust_scene_destroy(ts.scene);

    // Status strings exist for every code.
    for status in [
        CrustStatus::Ok,
        CrustStatus::NullArgument,
        CrustStatus::InvalidArgument,
        CrustStatus::InvalidCamera,
        CrustStatus::BadState,
        CrustStatus::BufferTooSmall,
    ] {
        assert!(!crust_status_string(status).is_null());
    }
    assert_eq!(crust_api_version(), 2);
    let (mut ma, mut mi, mut pa) = (0u32, 0u32, 0u32);
    crust_library_version(&mut ma, &mut mi, &mut pa);
    assert!(ma > 0 || mi > 0 || pa > 0);
    }
}
