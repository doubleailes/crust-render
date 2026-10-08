//! Is guiding biased on a crop, or does it only move energy between the
//! brightest pixels and the rest? Renders the crop with guiding off and on,
//! `SEEDS` seeds each, clamp and adaptive sampling off, and reports each
//! side's mean luminance with the standard error of the mean across seeds —
//! independent draws, so fireflies count, and no per-pixel variance estimate
//! is trusted — then guided against unguided, unpaired and paired by seed.
//! The calibration probe of harden-diagnostic-verdicts, which found #244 and
//! measured its fix (`rendering`'s design record, "Path guiding").
//!
//! guided_bias_probe STAGE FRAME X0 Y0 X1 Y1 SPP SEEDS [STEP]
//!
//! Seed `i` renders with frame `FRAME + i·STEP` on both sides (`STEP`
//! defaults to 0x9E3779B9, guiding's own pass step, which #244 used: a
//! guided render's training passes then share seeds with later seeds'
//! renders). Logs INFO, and the tracer's DEBUG lines (each guided render's
//! ΔEff decision and pass weight shares), to stderr.

use crust_assets::FileAssets;
use crust_core::{PixelRect, Renderer, Scene, UsdImportOptions};
use tracing::Level;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(filter_fn(|m| {
                    *m.level() <= Level::INFO
                        || (*m.level() == Level::DEBUG && m.target() == "crust_core::tracer")
                })),
        )
        .init();
    let a: Vec<String> = std::env::args().collect();
    let path = std::path::Path::new(&a[1]);
    let frame: f64 = a[2].parse().unwrap();
    let r: Vec<usize> = a[3..7].iter().map(|s| s.parse().unwrap()).collect();
    let spp: u32 = a[7].parse().unwrap();
    let seeds: u32 = a[8].parse().unwrap();
    let step: isize = match a.get(9) {
        Some(s) => s.parse().unwrap(),
        None => 0x9E37_79B9_u32 as isize,
    };
    let assets = FileAssets::new();
    let options = UsdImportOptions {
        frame: Some(frame),
        skip_stage_teardown: true,
        ..UsdImportOptions::default()
    };
    let t = std::time::Instant::now();
    let scene = Scene::from_usd_with_options(path, &assets, &options).expect("import");
    eprintln!("imported in {:.1}s", t.elapsed().as_secs_f64());
    let base = scene
        .settings
        .with_indirect_clamp(0.0)
        .with_samples_per_pixel(spp)
        .with_adaptive_sampling(spp, 0.0)
        .with_region(PixelRect::new(r[0], r[1], r[2], r[3]))
        .unwrap();
    let frame0 = base.frame();
    let mut renderer =
        Renderer::new(scene.camera, scene.world, scene.lights, base).with_volumes(scene.volumes);
    let luma = renderer.lights.luma();
    let mut sides: Vec<Vec<f64>> = Vec::new();
    for guided in [false, true] {
        let mut means = Vec::new();
        for i in 0..seeds {
            let seed = frame0.wrapping_add((i as isize).wrapping_mul(step));
            let s = base.with_frame(seed).with_guiding(
                guided,
                base.guiding_train_iterations(),
                base.guiding_prob(),
            );
            renderer.reconfigure(s);
            let t = std::time::Instant::now();
            let b = renderer.render_with_tiles();
            let (w, h) = b.size();
            let mut sum = 0.0f64;
            for y in 0..h {
                for x in 0..w {
                    let (r, g, bl) = b.get_rgb(x, y);
                    sum += luma.of(crust_core::Vec3A::new(r, g, bl)) as f64;
                }
            }
            let mean = sum / (w * h) as f64;
            eprintln!(
                "guided={guided} seed#{i}: mean {mean:.5} in {:.1}s",
                t.elapsed().as_secs_f64()
            );
            means.push(mean);
        }
        let (m, se) = mean_se(&means);
        println!(
            "guided={guided}: mean {m:.5} ± {se:.5} (s.e. over {} seeds at {spp} spp), median {:.4}; per seed {:?}",
            seeds,
            median(&means),
            means.iter().map(|x| format!("{x:.4}")).collect::<Vec<_>>()
        );
        sides.push(means);
    }
    // Guided against unguided: unpaired (the two sides as independent
    // samples), then paired by seed — both sides render seed i's final pass
    // with the same seed, so their difference is the sharper test.
    let (u, g) = (&sides[0], &sides[1]);
    let ((mu, seu), (mg, seg)) = (mean_se(u), mean_se(g));
    let diffs: Vec<f64> = g.iter().zip(u).map(|(g, u)| g - u).collect();
    let (md, sed) = mean_se(&diffs);
    println!(
        "guided − unguided: {:+.5} ({:+.2}%), z {:+.2} unpaired, {:+.2} paired (s.e. {:.5}); \
         guided darker on {}/{} seeds, above the unguided mean on {}/{}",
        mg - mu,
        (mg / mu - 1.0) * 100.0,
        (mg - mu) / (seu * seu + seg * seg).sqrt(),
        md / sed,
        sed,
        diffs.iter().filter(|d| **d < 0.0).count(),
        diffs.len(),
        g.iter().filter(|x| **x > mu).count(),
        g.len()
    );
}

/// Mean and standard error of the mean.
fn mean_se(v: &[f64]) -> (f64, f64) {
    let n = v.len() as f64;
    let m = v.iter().sum::<f64>() / n;
    let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0);
    (m, (var / n).sqrt())
}

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        0.5 * (s[n / 2 - 1] + s[n / 2])
    }
}
