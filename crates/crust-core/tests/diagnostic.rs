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
        "adaptive_record@crop_a",
        "max_depth_half",
        "light_sampling_reach@crop_a",
    ] {
        assert!(skipped(id), "{id} not listed: {:?}", r.not_tried);
    }
    assert!(r.picture_changing.light_sampling_reach.is_none());
}

/// The Cornell box at a size a debug build diagnoses in seconds, at
/// `spp` with adaptive sampling as `threshold` says.
fn small_cornell(spp: u32, threshold: f32) -> (Scene, Options<'static>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/cornellbox.usda");
    let mut scene = Scene::from_usd(&path).expect("load cornellbox.usda");
    scene.settings = scene
        .settings
        .with_resolution(48, 32)
        .with_samples_per_pixel(spp)
        .with_adaptive_sampling(8, threshold);
    let mut o = Options::new(path.display().to_string());
    o.budget = Duration::from_secs(20);
    o.repeats = 1;
    (scene, o)
}

/// The spec's scenarios "The authored setting is always on the grid", "The
/// sweep's settings are reported" and "The prediction reproduces": tier 2
/// records the crop, replays the grid, and renders `best` with the
/// `sampleCount` and `variance` AOVs, which match the replay in every
/// pixel. An adaptive suggestion leaves the scene not converged and is in
/// the suggested command.
#[test]
fn the_adaptive_sweep_is_verified_by_a_render() {
    let (scene, o) = small_cornell(64, 0.05);
    let r = diagnostic::run(scene, &o);
    let sb = r
        .sample_budget
        .as_ref()
        .expect("a target from the threshold");
    let Some(a) = &sb.adaptive else {
        // A slow machine: the recording did not fit, and says so.
        assert!(
            r.not_tried.iter().any(|n| n.id == "adaptive_record@crop_a"),
            "{:?}",
            r.not_tried
        );
        return;
    };
    assert_eq!(a.spp_recorded, 64);
    assert!(!a.authored_spp_capped && !a.adaptive_off_as_authored);
    assert_eq!(a.target_from, "variance_threshold");
    let s = &a.authored.setting;
    assert_eq!((s.samples_per_pixel, s.min_samples_per_pixel), (64, 8));
    assert!((s.variance_threshold.0 - 0.05).abs() < 1e-7);
    assert_eq!(a.grid.samples_per_pixel, [16, 32, 64]);
    assert!(a.grid.settings > 50, "{:?}", a.grid);
    assert_eq!(a.authored.per_crop.len(), 1);
    assert!(a.authored.per_crop[0].mean_spp.0 <= 64.0);
    // The errors are measured against a second render of each crop, or —
    // when it did not fit — each pixel's own variance, which says why.
    for c in &a.authored.per_crop {
        match c.error_from.as_str() {
            "second_render" => {}
            "variance" => assert!(
                r.not_tried
                    .iter()
                    .any(|n| n.id == format!("adaptive_second@{}", c.crop)),
                "{:?}",
                r.not_tried
            ),
            other => panic!("error_from {other}"),
        }
    }
    assert!(a.replay_s.0 >= 0.0);
    assert!(!a.front[0].points.is_empty());
    for v in &a.verification {
        assert!(v.sample_count_matches, "{v:?}");
        assert_eq!(v.mismatched_pixels, 0);
    }
    if a.verification.is_empty() {
        assert!(r.not_tried.iter().any(|n| n.id == "adaptive_verify@crop_a"));
    }
    let adaptive: Vec<_> = r
        .suggestions
        .iter()
        .filter(|s| s.id.starts_with("adaptive_sampling."))
        .collect();
    if !adaptive.is_empty() {
        assert!(!r.converged);
        assert!(a.delta_eff_over_authored.0 >= 1.10);
        for s in adaptive {
            let flag = s.flag.as_deref().unwrap();
            assert!(
                r.suggested_command.contains(&format!("{flag} {}", s.value)),
                "{} lacks {flag}",
                r.suggested_command
            );
        }
    }
    // No maps without the flag.
    assert!(r.adaptive_maps.is_none() && a.maps.is_none());
}

/// The spec's scenario "Adaptive sampling off in the stage": the sweep still
/// runs; the authored point is the fixed-count render; with no target, the
/// authored point's own error is the bar.
#[test]
fn with_adaptive_sampling_off_the_sweep_still_runs() {
    let (scene, mut o) = small_cornell(64, 0.0);
    o.adaptive_maps = Some("maps.exr".into());
    let r = diagnostic::run(scene, &o);
    let Some(a) = r.sample_budget.as_ref().and_then(|sb| sb.adaptive.as_ref()) else {
        assert!(
            r.not_tried.iter().any(|n| n.id == "adaptive_record@crop_a"),
            "{:?}",
            r.not_tried
        );
        return;
    };
    assert!(a.adaptive_off_as_authored);
    assert_eq!(a.target_from, "authored");
    assert_eq!(a.authored.per_crop[0].mean_spp.0, 64.0);
    assert_eq!(a.authored.per_crop[0].early_stopped_share.0, 0.0);
    assert!(a.best.per_crop[0].reaches_target);
    // The maps: named, and one crop of four planes inside the frame.
    let info = a.maps.as_ref().expect("asked for");
    assert_eq!(info.path, "maps.exr");
    assert_eq!(info.layers, diagnostic::report::ADAPTIVE_MAP_LAYERS);
    let maps = r.adaptive_maps.as_ref().expect("planes for the host");
    assert_eq!(maps.resolution, [48, 32]);
    let c = &maps.crops[0];
    let [x0, y0, x1, y1] = c.rect;
    let area = (x1 - x0) * (y1 - y0);
    assert_eq!(c.authored_sample_count.len(), area);
    assert!(c.authored_sample_count.iter().all(|&n| n == 64.0));
    assert_eq!(c.best_sample_count.len(), area);
}

/// The findings `crust check` reports come from the import alone: on a stage
/// with nine uniformly picked lights and preloaded textures, exactly
/// `textures_without_tx` and `many_lights_uniform` — and each with the id,
/// kind, evidence and action the diagnostic gives once its baseline facts
/// are added to the same import facts.
#[test]
fn import_only_findings_match_the_diagnostics() {
    use diagnostic::checks::{self, Facts};
    let lights: String = (0..9)
        .map(|i| {
            format!(
                "    def SphereLight \"l{i}\"\n    {{\n        double3 xformOp:translate = ({i}, 3, 0)\n        uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    }}\n"
            )
        })
        .collect();
    let text = format!(
        "#usda 1.0\ndef Xform \"World\"\n{{\n    def Camera \"cam\"\n    {{\n    }}\n{lights}}}\ndef Scope \"Render\"\n{{\n    def RenderSettings \"settings\"\n    {{\n        token crust:lightSelection = \"uniform\"\n    }}\n}}\n"
    );
    let dir = std::env::temp_dir().join("crust_diagnostic_tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("nine_uniform_lights.usda");
    std::fs::write(&path, text).unwrap();
    let scene = Scene::from_usd(&path).expect("loads");
    assert_eq!(scene.lights.count(), 9);

    let import = Facts::from_import(&scene, false, 3, None);
    let found = checks::run(&import);
    let ids: Vec<&str> = found.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, ["textures_without_tx", "many_lights_uniform"]);

    // The diagnostic's facts: the same import facts, plus a quiet baseline.
    let diagnosed = checks::run(&Facts {
        texture_lookups: Some((100, 100)),
        ptex_lookups: Some((0, 0)),
        unlit_emission: Some(0.0),
        indirect_dominant: Some(false),
        spp: 16,
        ..import.clone()
    });
    for f in &found {
        let d = diagnosed
            .iter()
            .find(|d| d.id == f.id)
            .unwrap_or_else(|| panic!("the diagnostic has no {}", f.id));
        assert_eq!(
            serde_json::to_value(f).unwrap(),
            serde_json::to_value(d).unwrap(),
            "{}",
            f.id
        );
    }
}

/// What `unlit_emitters` names, read off real stages: the emissive card of
/// `mesh_light_no_api.usda`; nothing once `mesh_light.usda` applies the API;
/// and a refused mesh light with the code that refused it.
#[test]
fn unlit_prims_are_the_emissive_prims_outside_the_light_list() {
    use diagnostic::checks;
    let sample = |name: &str| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../samples")
            .join(name);
        Scene::from_usd(&path).unwrap_or_else(|e| panic!("{name}: {e}"))
    };
    let without = sample("mesh_light_no_api.usda");
    assert_eq!(without.lights.count(), 0);
    assert_eq!(
        checks::unlit_prims(&without),
        [("/World/Card".to_owned(), None)]
    );
    let with = sample("mesh_light.usda");
    assert_eq!(with.lights.count(), 1);
    assert!(checks::unlit_prims(&with).is_empty());

    let text = r#"#usda 1.0
def Xform "World"
{
    def Material "Coated"
    {
        token outputs:surface.connect = </World/Coated/S.outputs:surface>
        def Shader "S"
        {
            uniform token info:id = "crust:openpbr"
            float inputs:emissionLuminance = 3
            float inputs:coatWeight = 1
            token outputs:surface
        }
    }
    def Mesh "Sign" (prepend apiSchemas = ["MaterialBindingAPI", "MeshLightAPI"])
    {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
        rel material:binding = </World/Coated>
    }
}
"#;
    let dir = std::env::temp_dir().join("crust_diagnostic_tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("refused_mesh_light.usda");
    std::fs::write(&path, text).unwrap();
    let refused = Scene::from_usd(&path).expect("loads");
    assert_eq!(
        checks::unlit_prims(&refused),
        [(
            "/World/Sign".to_owned(),
            Some("light.mesh_light_varying_emission")
        )]
    );
}
