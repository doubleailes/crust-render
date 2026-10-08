//! End-to-end validation of path guiding: rendering the guided sample scene
//! with and without guiding must produce the same image in expectation.
//!
//! Ignored by default because it renders several full frames; run with
//! `cargo test -p crust-core --release --test guiding -- --ignored`.

use std::path::PathBuf;

use crust_core::{RenderSettings, Renderer, Scene};

fn sample(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("samples")
        .join(name)
}

fn sample_scene() -> PathBuf {
    sample("cornellbox_guided.usda")
}

/// Render the sample scene small and return per-pixel luminance.
fn render_lum(guided: bool, spp: u32, train_iterations: u32) -> Vec<f64> {
    const RES: usize = 96;
    let scene = Scene::from_usd(&sample_scene()).expect("load cornellbox_guided.usda");
    let settings = RenderSettings::default()
        .with_resolution(RES, RES)
        .with_samples_per_pixel(spp)
        .with_max_depth(8)
        .with_adaptive_sampling(16, 0.05)
        .with_guiding(guided, train_iterations, 0.5);
    let renderer = Renderer::new(scene.camera, scene.world, scene.lights, settings);
    let buf = renderer.render();
    let mut out = Vec::with_capacity(RES * RES);
    for y in 0..RES {
        for x in 0..RES {
            out.push(utils::luminance(buf.get_pixel(x, y)) as f64);
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

/// Mean luminance over the shadow of `caustic_guided.usda`'s glass ball,
/// where nearly all the light is a caustic, averaged over `frames` seeds.
fn caustic_shadow_mean(guided: bool, frames: std::ops::RangeInclusive<isize>) -> f64 {
    const RES: usize = 64;
    const SPP: u32 = 256;
    // The shadow spans 51–88% of the image from the top; the buffer's row 0
    // is the bottom (the writer flips rows on output).
    let (y0, y1) = (RES * 12 / 100, RES * 49 / 100);
    let (x0, x1) = (RES * 12 / 100, RES * 64 / 100);
    let n = frames.clone().count() as f64;
    let mut total = 0.0;
    for frame in frames {
        let scene =
            Scene::from_usd(&sample("caustic_guided.usda")).expect("load caustic_guided.usda");
        // Every pixel takes the whole budget (min = spp), and the firefly
        // clamp is off: it would remove most of the caustic on both sides.
        let settings = RenderSettings::default()
            .with_resolution(RES, RES)
            .with_samples_per_pixel(SPP)
            .with_max_depth(10)
            .with_adaptive_sampling(SPP, 0.05)
            .with_frame(frame)
            .with_guiding(guided, 4, 0.5)
            .with_indirect_clamp(0.0);
        let buf = Renderer::new(scene.camera, scene.world, scene.lights, settings).render();
        let mut sum = 0.0;
        for y in y0..y1 {
            for x in x0..x1 {
                sum += utils::luminance(buf.get_pixel(x, y)) as f64;
            }
        }
        total += sum / ((y1 - y0) * (x1 - x0)) as f64;
    }
    total / n
}

/// Rare, very bright paths must not darken a guided render. With passes
/// blended by inverse *estimated* variance, a low-spp training pass that
/// missed the caustic looked low-variance and took most of the weight: this
/// test measured a ratio of 0.51 (0.50–0.52 over four blocks of 8 seeds),
/// the shadow at half its energy. Weighted by sample budget it measures 0.97
/// (0.80–1.07 per block of 8: the caustic is heavy-tailed, hence 32 seeds).
/// The tolerance sits between the two outcomes.
#[test]
#[ignore = "renders 64 small frames; run explicitly with --ignored"]
fn guided_caustic_is_not_darkened() {
    let unguided = caustic_shadow_mean(false, 1..=32);
    let guided = caustic_shadow_mean(true, 1..=32);
    let ratio = guided / unguided.max(1e-6);
    assert!(
        (ratio - 1.0).abs() < 0.25,
        "guided caustic shadow {guided:.4} vs unguided {unguided:.4} — ratio {ratio:.3}"
    );
}

/// The guided path must also work through the tiled/bucket renderer.
#[test]
#[ignore = "renders a frame; run explicitly with --ignored"]
fn guided_render_with_tiles_smoke() {
    let scene = Scene::from_usd(&sample_scene()).expect("load cornellbox_guided.usda");
    let settings = RenderSettings::default()
        .with_resolution(64, 64)
        .with_samples_per_pixel(8)
        .with_max_depth(6)
        .with_adaptive_sampling(4, 0.05)
        .with_guiding(true, 3, 0.5);
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
/// guided renders included — the tiled pass hands its training samples on
/// in scanline order, since the SD-tree's accumulation is order-dependent in
/// floating point. 37×21 leaves partial tiles on both edges, and the final
/// pass stops pixels adaptively (min 4 spp at a 5% threshold).
#[test]
fn guided_tiles_and_rows_are_bit_identical() {
    let (w, h) = (37, 21);
    let render = |tiled: bool| {
        let scene = Scene::from_usd(&sample_scene()).expect("load cornellbox_guided.usda");
        let settings = RenderSettings::default()
            .with_resolution(w, h)
            .with_samples_per_pixel(8)
            .with_max_depth(6)
            .with_adaptive_sampling(4, 0.05)
            .with_guiding(true, 3, 0.5);
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
