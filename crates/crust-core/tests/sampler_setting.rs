//! `crust:sampler` / `--sampler`: the QMC sampler a render draws from.
//!
//! The default is bit-identical to the renderer before the setting existed
//! (pinned by the goldens, `scripts/check_images.sh`), and explicit and
//! implicit `sobol` agree; every other sampler converges to the same image.

use crust_core::{Buffer, Renderer, SamplerKind, Scene};
use std::path::{Path, PathBuf};

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

/// The cornell box at `w`×`h`, `spp` samples a pixel, unclamped, with no
/// adaptive stop (the minimum at the budget), seeded by `seed`, drawn from
/// `sampler`.
fn render(sampler: SamplerKind, (w, h): (usize, usize), spp: u32, seed: isize) -> Buffer {
    let scene = Scene::from_usd(&sample("cornellbox.usda")).expect("imports");
    let settings = scene
        .settings
        .with_resolution(w, h)
        .with_samples_per_pixel(spp)
        .with_adaptive_sampling(spp, 0.0)
        .with_indirect_clamp(0.0)
        .with_frame(seed)
        .with_sampler(sampler);
    Renderer::new(scene.camera, scene.world, scene.lights, settings)
        .with_volumes(scene.volumes)
        .render()
}

fn pixels(b: &Buffer, (w, h): (usize, usize)) -> Vec<[f32; 3]> {
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| b.get_pixel(x, y).to_array())
        .collect()
}

/// A scene rendered once with no sampler setting and once with `sobol`
/// gives the same pixels, bit for bit.
#[test]
fn explicit_and_implicit_default_agree() {
    let size = (16, 12);
    let scene = Scene::from_usd(&sample("cornellbox.usda")).expect("imports");
    assert_eq!(scene.settings.sampler(), SamplerKind::Sobol);
    let implicit = render(SamplerKind::default(), size, 4, 0);
    let explicit = render(SamplerKind::Sobol, size, 4, 0);
    assert_eq!(
        pixels(&implicit, size)
            .iter()
            .map(|p| p.map(f32::to_bits))
            .collect::<Vec<_>>(),
        pixels(&explicit, size)
            .iter()
            .map(|p| p.map(f32::to_bits))
            .collect::<Vec<_>>()
    );
}

/// Each sampler draws different numbers: no two give the same pixels.
#[test]
fn every_sampler_draws_its_own_numbers() {
    let size = (8, 6);
    let images: Vec<Vec<[f32; 3]>> = SamplerKind::CHOICES
        .iter()
        .map(|&(kind, _, _)| pixels(&render(kind, size, 2, 0), size))
        .collect();
    for (i, a) in images.iter().enumerate() {
        for b in &images[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

/// Rendered unclamped over `n` seeds, every sampler's mean image agrees
/// with `sobol`'s within the seeds' standard error, at every pixel of a
/// small frame. Each seed is an independent render (`crust:frame` seeds the
/// sampler), so the per-pixel standard error of the difference is the usual
/// one, and `z` is the difference in those units.
///
/// Two bounds, because this is 288 comparisons per sampler and a flat three
/// standard errors would fail one of them by chance (it did, at |z| = 3.23):
/// no pixel beyond five standard errors (a false alarm once in millions),
/// and the mean of z² over the frame near its expectation of one — a
/// sampler converging to another image pushes it up with every seed, a
/// lucky pixel does not.
///
/// A few seconds in `cargo test --release` on four cores (the build is the
/// cost); the `rendering` design record has the command.
#[test]
fn every_sampler_converges_to_the_sobol_mean() {
    let size = (12, 8);
    let (spp, seeds) = (8, 32);
    let moments = |kind: SamplerKind| {
        let n = size.0 * size.1;
        let (mut sum, mut sq) = (vec![[0.0f64; 3]; n], vec![[0.0f64; 3]; n]);
        for seed in 0..seeds {
            for (i, p) in pixels(&render(kind, size, spp, seed), size)
                .into_iter()
                .enumerate()
            {
                for c in 0..3 {
                    sum[i][c] += p[c] as f64;
                    sq[i][c] += (p[c] as f64).powi(2);
                }
            }
        }
        let m = seeds as f64;
        let mean: Vec<[f64; 3]> = sum.iter().map(|s| s.map(|v| v / m)).collect();
        // The variance of the mean over the seeds.
        let var: Vec<[f64; 3]> = sq
            .iter()
            .zip(&mean)
            .map(|(q, mu)| std::array::from_fn(|c| (q[c] / m - mu[c] * mu[c]).max(0.0) / m))
            .collect();
        (mean, var)
    };
    let (sobol_mean, sobol_var) = moments(SamplerKind::Sobol);
    for &(kind, name, _) in SamplerKind::CHOICES {
        if kind == SamplerKind::Sobol {
            continue;
        }
        let (mean, var) = moments(kind);
        let (mut worst, mut sum_sq, mut count) = (0.0f64, 0.0f64, 0usize);
        for i in 0..mean.len() {
            for c in 0..3 {
                let se = (sobol_var[i][c] + var[i][c]).sqrt();
                let z = (mean[i][c] - sobol_mean[i][c]).abs() / se.max(1e-6);
                worst = worst.max(z);
                sum_sq += z * z;
                count += 1;
                assert!(
                    z < 5.0,
                    "{name}: pixel {i} channel {c}: {} vs sobol {} (z = {z:.2})",
                    mean[i][c],
                    sobol_mean[i][c]
                );
            }
        }
        let mean_sq = sum_sq / count as f64;
        eprintln!("{name}: worst |z| {worst:.2}, mean z² {mean_sq:.3}");
        assert!(
            mean_sq < 1.5,
            "{name}: mean z² {mean_sq:.3} over {count} comparisons"
        );
    }
}
