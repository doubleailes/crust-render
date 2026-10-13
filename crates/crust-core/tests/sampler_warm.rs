//! A pass draws from tables the renderer builds before the pass starts its
//! timer (`Renderer::render_pass`), so `--stats` and `bench_ab.sh` never
//! count the PMJ sequence's or the blue-noise rankings' build as sampling.
//!
//! Its own test binary: the tables are process globals, and any other test
//! drawing from PMJ first would build them before this one looks.

use crust_core::{Renderer, SamplerKind, Scene};
use std::path::Path;
use std::time::{Duration, Instant};

/// After a pass with `pmj`, the table is built: a second warm-up is the
/// no-op it is once built, orders of magnitude under the build (1.5 ms
/// measured for PMJ's 2^16 entries), where the pass found it.
#[test]
fn a_pass_builds_its_sampler_tables_before_it_starts() {
    let scene = Scene::from_usd(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/cornellbox.usda"),
    )
    .expect("imports");
    let settings = scene
        .settings
        .with_resolution(4, 4)
        .with_samples_per_pixel(1)
        .with_adaptive_sampling(1, 0.0)
        .with_sampler(SamplerKind::Pmj);
    let renderer = Renderer::new(scene.camera, scene.world, scene.lights, settings)
        .with_volumes(scene.volumes);
    let _ = renderer.render();
    let t = Instant::now();
    openqmc::PmjSampler::warm_cache();
    let again = t.elapsed();
    assert!(
        again < Duration::from_micros(500),
        "the PMJ table was not built by the pass: a warm-up after it took {again:?}"
    );
}
