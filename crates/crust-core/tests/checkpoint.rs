//! End-to-end checkpoint/resume validation: a render interrupted at a pass
//! boundary and resumed from its checkpoint must reproduce the
//! uninterrupted render **bit for bit** — the stateless sampler, the pure
//! pass schedule and the raw-sum checkpoint state together guarantee it.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use crust_core::{Buffer, CheckpointState, Error, RenderOptions, RenderSettings, Renderer, Scene};

fn sample_scene() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("samples")
        .join("cornellbox.usda")
}

const RES: usize = 48;
const SPP: u32 = 16;

fn renderer(settings: RenderSettings) -> Renderer {
    let scene = Scene::from_usd(&sample_scene()).expect("load cornellbox.usda");
    Renderer::new(scene.camera, scene.world, scene.lights, settings)
}

/// `variance_threshold` 0 disables adaptive sampling; a positive value
/// exercises the tile stage machine (including its resume re-derivation).
fn settings(variance_threshold: f32) -> RenderSettings {
    RenderSettings::new(SPP, 6, RES, RES, 4, variance_threshold, 0)
}

fn expect_err(r: Result<Buffer, Error>) -> Error {
    match r {
        Err(e) => e,
        Ok(_) => panic!("expected an error, got a rendered buffer"),
    }
}

fn assert_buffers_bit_equal(a: &Buffer, b: &Buffer, what: &str) {
    for y in 0..RES {
        for x in 0..RES {
            let (pa, pb) = (a.get_pixel(x, y), b.get_pixel(x, y));
            assert_eq!(pa, pb, "{what}: pixel ({x}, {y}) differs: {pa:?} vs {pb:?}");
        }
    }
}

fn straight_and_resumed(variance_threshold: f32) -> (Buffer, Buffer, Buffer) {
    // Straight render, no checkpointing.
    let straight = renderer(settings(variance_threshold))
        .render_with_options(RenderOptions::default())
        .expect("straight render");

    // Same render, snapshotting at every eligible pass boundary; the
    // callback must not perturb the result.
    let snapshots: Mutex<Vec<CheckpointState>> = Mutex::new(Vec::new());
    let on_checkpoint = |state: &CheckpointState| {
        snapshots.lock().unwrap().push(state.clone());
    };
    let checkpointed = renderer(settings(variance_threshold))
        .render_with_options(RenderOptions {
            checkpoint_interval: Some(Duration::ZERO),
            on_checkpoint: Some(&on_checkpoint),
            ..Default::default()
        })
        .expect("checkpointed render");

    // Resume from a mid-render snapshot and finish.
    let snapshots = snapshots.into_inner().unwrap();
    assert!(
        snapshots.len() >= 2,
        "expected several pass-boundary snapshots, got {}",
        snapshots.len()
    );
    let mid = snapshots[snapshots.len() / 2].clone();
    assert!(mid.next_sample > 0 && mid.next_sample < SPP);
    let resumed = renderer(settings(variance_threshold))
        .render_with_options(RenderOptions {
            resume: Some(mid),
            ..Default::default()
        })
        .expect("resumed render");

    (straight, checkpointed, resumed)
}

#[test]
fn resume_is_bit_exact() {
    let (straight, checkpointed, resumed) = straight_and_resumed(0.0);
    assert_buffers_bit_equal(&straight, &checkpointed, "checkpoint callback perturbed");
    assert_buffers_bit_equal(&straight, &resumed, "resume diverged");
}

#[test]
fn adaptive_resume_is_bit_exact() {
    let (straight, checkpointed, resumed) = straight_and_resumed(0.15);
    assert_buffers_bit_equal(&straight, &checkpointed, "checkpoint callback perturbed");
    assert_buffers_bit_equal(&straight, &resumed, "adaptive resume diverged");
}

/// A checkpoint that already holds the full budget is returned as-is.
#[test]
fn full_checkpoint_returns_without_rendering() {
    let snapshots: Mutex<Vec<CheckpointState>> = Mutex::new(Vec::new());
    let on_checkpoint = |state: &CheckpointState| {
        snapshots.lock().unwrap().push(state.clone());
    };
    let full = renderer(settings(0.0))
        .render_with_options(RenderOptions {
            checkpoint_interval: Some(Duration::ZERO),
            on_checkpoint: Some(&on_checkpoint),
            ..Default::default()
        })
        .expect("render");
    let last = snapshots.into_inner().unwrap().pop().unwrap();

    // Resume with a *smaller* budget than the checkpoint holds.
    let shrunk = RenderSettings::new(last.next_sample, 6, RES, RES, 4, 0.0, 0);
    let restored = renderer(shrunk)
        .render_with_options(RenderOptions {
            resume: Some(last.clone()),
            ..Default::default()
        })
        .expect("resume at full budget");
    // The restored image is the checkpoint's own estimate — compare a few
    // pixels against sum/count directly.
    for (x, y) in [(0usize, 0usize), (RES / 2, RES / 2), (RES - 1, RES - 1)] {
        let i = y * RES + x;
        let expect = last.sum[i] / last.count[i] as f32;
        assert_eq!(restored.get_pixel(x, y), expect);
    }
    // And it must not equal a *finished* render only by construction —
    // sanity: full render at SPP differs from the truncated estimate
    // somewhere (they hold different sample counts).
    let mut any_diff = false;
    'outer: for y in 0..RES {
        for x in 0..RES {
            if full.get_pixel(x, y) != restored.get_pixel(x, y) {
                any_diff = true;
                break 'outer;
            }
        }
    }
    assert!(any_diff, "truncated checkpoint unexpectedly equals the full render");
}

#[test]
fn mismatched_checkpoint_is_rejected() {
    let snapshots: Mutex<Vec<CheckpointState>> = Mutex::new(Vec::new());
    let on_checkpoint = |state: &CheckpointState| {
        snapshots.lock().unwrap().push(state.clone());
    };
    renderer(settings(0.0))
        .render_with_options(RenderOptions {
            checkpoint_interval: Some(Duration::ZERO),
            on_checkpoint: Some(&on_checkpoint),
            ..Default::default()
        })
        .expect("render");
    let state = snapshots.into_inner().unwrap().pop().unwrap();

    // Different frame → different fingerprint.
    let other_frame = RenderSettings::new(SPP, 6, RES, RES, 4, 0.0, 1);
    let err = expect_err(renderer(other_frame).render_with_options(RenderOptions {
        resume: Some(state.clone()),
        ..Default::default()
    }));
    assert!(matches!(err, Error::CheckpointMismatch { .. }), "{err}");

    // Different resolution.
    let other_res = RenderSettings::new(SPP, 6, RES * 2, RES, 4, 0.0, 0);
    let err = expect_err(renderer(other_res).render_with_options(RenderOptions {
        resume: Some(state.clone()),
        ..Default::default()
    }));
    assert!(matches!(err, Error::CheckpointMismatch { .. }), "{err}");

    // Corrupted buffers.
    let mut corrupt = state;
    corrupt.count.pop();
    let err = expect_err(renderer(settings(0.0)).render_with_options(RenderOptions {
        resume: Some(corrupt),
        ..Default::default()
    }));
    assert!(matches!(err, Error::CheckpointMismatch { .. }), "{err}");
}

#[test]
fn guided_renders_refuse_to_resume() {
    let guided = settings(0.0).with_guiding(true, 2, 0.5);
    let dummy = CheckpointState {
        width: RES,
        height: RES,
        sum: vec![Default::default(); RES * RES],
        odd_sum: vec![Default::default(); RES * RES],
        count: vec![0; RES * RES],
        next_sample: 4,
        fingerprint: 0,
    };
    let err = expect_err(renderer(guided).render_with_options(RenderOptions {
        resume: Some(dummy),
        ..Default::default()
    }));
    assert!(matches!(err, Error::CheckpointUnsupported(_)), "{err}");
}
