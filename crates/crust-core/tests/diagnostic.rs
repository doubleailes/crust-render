//! `crust_core::diagnostic::run` on a real stage: the report's structure,
//! whatever the machine's speed lets the budget fit.

use std::path::Path;
use std::time::Duration;

use crust_core::Scene;
use crust_core::diagnostic::{self, Options};

#[test]
fn the_cornell_box_is_diagnosed_within_its_budget() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/cornellbox.usda");
    let mut scene = Scene::from_usd(&path).expect("load cornellbox.usda");
    // Small enough for a debug build: one crop, clipped to the frame.
    scene.settings = scene.settings.with_resolution(96, 64);
    let mut o = Options::new(path.display().to_string());
    o.budget = Duration::from_secs(6);
    o.repeats = 1;
    let r = diagnostic::run(scene, &o);
    assert_eq!(r.format, diagnostic::FORMAT);
    assert!((4..=64).contains(&r.baseline.spp));
    assert!(r.baseline.mrse.0 > 0.0 && r.baseline.mrse.0.is_finite());
    assert_eq!(r.scene.resolution, [96, 64]);
    // The frame is smaller than a crop: one crop, the whole frame.
    assert_eq!(r.crops.len(), 1);
    assert_eq!(r.crops[0].rect, [0, 0, 96, 64]);
    // Eight transport rows; one light, so one per-light group.
    assert_eq!(r.noise_breakdown.components.len(), 8);
    assert_eq!(r.noise_breakdown.light_groups_by, "light");
    // One light: light selection cannot change anything.
    assert!(
        r.not_tried
            .iter()
            .any(|n| n.id == "light_selection" && n.reason == "not_applicable")
    );
    // Every planned trial either ran or says why not: on one light, the
    // other MIS heuristic (never light- or BSDF-only), more light samples,
    // guiding.
    for id in [
        "strategy=balance",
        "light_samples=2",
        "light_samples=4",
        "light_samples_indirect=2",
        "guiding=true",
    ] {
        let ran = r.trials.iter().any(|t| t.id == id);
        let skipped = r.not_tried.iter().any(|n| n.id == id);
        assert!(ran != skipped, "{id}: ran {ran}, not tried {skipped}");
    }
    assert!(
        !r.trials
            .iter()
            .any(|t| t.id == "strategy=light" || t.id == "strategy=bsdf")
    );
    assert_eq!(
        r.run.exit,
        if r.run.budget_exceeded_in.is_some() {
            3
        } else {
            0
        }
    );
    assert!(r.suggested_command.starts_with("crust render -i "));
    // A suggestion is a measured gain: the scene is then not converged.
    if !r.suggestions.is_empty() {
        assert!(!r.converged);
    }
    // No unbiased setting changes the picture.
    for t in &r.trials {
        assert_ne!(t.verdict.name(), "biased", "{}: {:?}", t.id, t.per_crop);
    }
    assert_eq!(r.run.seeds.len(), 1);
    // Light sampling reaches all of the Cornell box: one area light, every
    // surface diffuse. Measured, or listed as not fitting the budget.
    match &r.picture_changing.light_sampling_reach {
        Some(reach) => {
            for c in reach {
                assert!(c.z.0.abs() < 4.0, "{c:?}");
            }
        }
        None => assert!(
            r.not_tried
                .iter()
                .any(|n| n.id.starts_with("light_sampling_reach") && n.reason == "budget"),
            "{:?}",
            r.not_tried
        ),
    }
    // None of the picture findings fires on it.
    for f in &r.static_findings {
        assert!(
            !["clamp_bias", "firefly_energy", "light_sampling_misses"].contains(&f.id.as_str()),
            "{f:?}"
        );
    }
}

/// The spec's scenario "Tier 1 overruns": every later measurement that did
/// not run says so, whatever ran out first.
#[test]
fn a_budget_the_baseline_exhausts_lists_every_later_measurement() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/cornellbox.usda");
    let mut scene = Scene::from_usd(&path).expect("load cornellbox.usda");
    scene.settings = scene.settings.with_resolution(96, 64);
    let mut o = Options::new(path.display().to_string());
    o.budget = Duration::from_millis(1);
    o.repeats = 1;
    let r = diagnostic::run(scene, &o);
    assert_eq!(r.run.exit, 3);
    assert_eq!(r.run.budget_exceeded_in.as_deref(), Some("P1"));
    assert!(r.trials.is_empty());
    let skipped = |id: &str| {
        r.not_tried
            .iter()
            .any(|n| n.id == id && n.reason == "budget")
    };
    for id in [
        "adaptive@crop_a",
        "max_depth_half",
        "light_sampling_reach@crop_a",
    ] {
        assert!(skipped(id), "{id} not listed: {:?}", r.not_tried);
    }
    assert!(r.picture_changing.light_sampling_reach.is_none());
}
