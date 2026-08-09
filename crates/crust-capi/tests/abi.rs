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
    let scene = crust_scene_create();
    assert!(!scene.is_null());

    let mut material = unsafe { std::mem::zeroed::<CrustMaterial>() };
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

    let mut settings = unsafe { std::mem::zeroed::<CrustRenderSettings>() };
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

fn commit(scene: *mut SceneHandle, token: *const TokenHandle) -> *mut RendererHandle {
    let mut renderer: *mut RendererHandle = ptr::null_mut();
    assert_eq!(
        crust_scene_commit(scene, token, &mut renderer),
        CrustStatus::Ok
    );
    assert!(!renderer.is_null());
    renderer
}

fn read_rgba(renderer: *mut RendererHandle) -> Vec<f32> {
    let mut out = vec![0.0f32; 32 * 32 * 4];
    assert_eq!(
        crust_renderer_read_color(renderer, out.as_mut_ptr(), 32 * 32),
        CrustStatus::Ok
    );
    out
}

#[test]
fn renders_deterministically_through_the_abi() {
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

#[test]
fn aov_planes_report_hits_and_misses() {
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

#[test]
fn stop_token_interrupts_and_resumes() {
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

#[test]
fn error_paths_return_their_exact_status() {
    // Null handles.
    let mut out_id = 0u32;
    let material = {
        let mut m = unsafe { std::mem::zeroed::<CrustMaterial>() };
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
    assert_eq!(crust_api_version(), 1);
    let (mut ma, mut mi, mut pa) = (0u32, 0u32, 0u32);
    crust_library_version(&mut ma, &mut mi, &mut pa);
    assert!(ma > 0 || mi > 0 || pa > 0);
}
