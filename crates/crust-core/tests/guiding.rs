//! End-to-end validation of path guiding: rendering the guided sample scene
//! with and without guiding must produce the same image in expectation.
//!
//! Ignored by default because it renders several full frames; run with
//! `cargo test -p crust-core --release --test guiding -- --ignored`.

use std::path::PathBuf;

use crust_core::{RenderSettings, Renderer, Scene};

fn sample_scene() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("samples")
        .join("cornellbox_guided.usda")
}

/// Render the sample scene small and return per-pixel luminance.
fn render_lum(guided: bool, spp: u32, train_iterations: u32) -> Vec<f64> {
    const RES: usize = 96;
    let scene = Scene::from_usd(&sample_scene()).expect("load cornellbox_guided.usda");
    let settings = RenderSettings::new(spp, 8, RES, RES, 16, 0.05, 0).with_guiding(
        guided,
        train_iterations,
        0.5,
    );
    let renderer = Renderer::new(scene.camera, scene.world, scene.lights, settings);
    let buf = renderer.render();
    let mut out = Vec::with_capacity(RES * RES);
    for y in 0..RES {
        for x in 0..RES {
            let c = buf.get_pixel(x, y);
            out.push((0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z) as f64);
        }
    }
    out
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

/// Guided and unguided renders must agree in expectation: mean image
/// luminance within a few percent of each other (both are unbiased
/// estimators of the same integral; the tolerance covers Monte Carlo noise
/// at this sample count).
#[test]
#[ignore = "renders several frames; run explicitly with --ignored"]
fn guided_render_is_unbiased() {
    let unguided = mean(&render_lum(false, 64, 4));
    let guided = mean(&render_lum(true, 32, 6));
    let rel = (guided - unguided).abs() / unguided.max(1e-6);
    assert!(
        rel < 0.05,
        "guided mean {guided:.5} vs unguided mean {unguided:.5} — relative diff {rel:.4} > 5%"
    );
}

/// The guided path must also work through the tiled/bucket renderer.
#[test]
#[ignore = "renders a frame; run explicitly with --ignored"]
fn guided_render_with_tiles_smoke() {
    let scene = Scene::from_usd(&sample_scene()).expect("load cornellbox_guided.usda");
    let settings = RenderSettings::new(8, 6, 64, 64, 4, 0.05, 0).with_guiding(true, 3, 0.5);
    let renderer = Renderer::new(scene.camera, scene.world, scene.lights, settings);
    let buf = renderer.render_with_tiles();
    // The closed box is lit: the image cannot be black.
    let mut total = 0.0f64;
    for y in 0..64 {
        for x in 0..64 {
            let c = buf.get_pixel(x, y);
            total += (c.x + c.y + c.z) as f64;
        }
    }
    assert!(total > 1.0, "tiled guided render came back black");
}

/// A render mode is scheduling only: tiles and rows must give the same bits,
/// guided renders included — the tiled pass hands its training samples and
/// variance sum on in scanline order, since both are order-dependent in
/// floating point. 37×21 leaves partial tiles on both edges, and the final
/// pass stops pixels adaptively (min 4 spp at a 5% threshold).
#[test]
fn guided_tiles_and_rows_are_bit_identical() {
    let (w, h) = (37, 21);
    let render = |tiled: bool| {
        let scene = Scene::from_usd(&sample_scene()).expect("load cornellbox_guided.usda");
        let settings = RenderSettings::new(8, 6, w, h, 4, 0.05, 0).with_guiding(true, 3, 0.5);
        let renderer = Renderer::new(scene.camera, scene.world, scene.lights, settings);
        if tiled {
            renderer.render_with_tiles()
        } else {
            renderer.render()
        }
    };
    let (rows, tiles) = (render(false), render(true));
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (rows.get_pixel(x, y), tiles.get_pixel(x, y));
            assert_eq!(
                (a.x.to_bits(), a.y.to_bits(), a.z.to_bits()),
                (b.x.to_bits(), b.y.to_bits(), b.z.to_bits()),
                "pixel ({x}, {y})"
            );
        }
    }
}
