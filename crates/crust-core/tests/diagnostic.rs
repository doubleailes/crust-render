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
    // Every planned trial either ran or says why not.
    assert!(r.trials.len() + r.not_tried.iter().filter(|n| n.tier == 1).count() >= 7);
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
}
