//! The report's contract: its key order, its Markdown, `--baseline`.

use super::report::*;

/// A report with every section filled, deltas included.
pub(super) fn fixture() -> Report {
    let n = |x: f64| Num(x);
    Report {
        format: FORMAT.into(),
        crust_version: "0.0.0".into(),
        scene: SceneInfo {
            path: "samples/cornellbox.usda".into(),
            frame: None,
            camera: None,
            resolution: [640, 360],
            region: None,
        },
        effective_settings: vec![Setting {
            name: "light_selection".into(),
            value: "power".into(),
            flag: Some("--light-selection".into()),
            usd_attribute: Some("crust:lightSelection".into()),
        }],
        run: RunInfo {
            budget_s: n(120.0),
            used_s: n(61.23456),
            import_s: n(0.5),
            threads: 16,
            repeats: 3,
            probe_conditions: ProbeConditions {
                indirect_clamp: "off".into(),
                adaptive_sampling: "off".into(),
                fixed_spp: true,
                resolution: [640, 360],
            },
            phases: vec![PhaseRun {
                name: "P1".into(),
                time_s: n(4.0),
                completed: true,
            }],
            budget_exceeded_in: None,
            exit: 0,
            seeds: vec![0, 2_246_822_507, 4_493_645_014],
            tier2_reserve_s: n(4.0),
            tier3_reserve_s: n(1.5),
        },
        static_findings: vec![Finding {
            id: "textures_without_tx".into(),
            kind: FindingKind::Time,
            summary: "3 UV texture(s) have no .tx beside them".into(),
            evidence: Evidence::new().with("textures", 3.0),
            action: Action::Set {
                flag: Some("--auto-tx".into()),
                usd_attribute: None,
                value: "on".into(),
            },
        }],
        baseline: Baseline {
            calibration_time_s: n(0.2),
            spp: 16,
            time_s: n(3.2),
            setup_s: n(0.0),
            render_s: n(3.2),
            mrse: n(0.012345678),
            rays_per_s: n(12_345_678.0),
            mean_path_length: n(1.5),
            rr_kill_rate: n(0.25),
            ended_by_depth_share: n(0.0),
            shadow_rays_per_vertex: n(0.5),
            profile_top: vec![ProfileShare {
                section: "Trace".into(),
                share: n(0.41),
            }],
            texture_hit_rate: None,
            ptex_hit_rate: None,
            peak_mem_bytes: Some(1 << 30),
        },
        noise_breakdown: NoiseBreakdown {
            components: vec![NoiseRow {
                key: "indirect_diffuse".into(),
                expression: "C<RD>.+[LO]".into(),
                mean_luminance: n(0.2),
                relative_error: n(0.03),
                relative_error_vs_beauty: n(0.01),
            }],
            light_groups: Vec::new(),
            light_groups_by: "none".into(),
            dominant: Some("indirect_diffuse".into()),
        },
        crops: vec![Crop {
            id: "crop_a".into(),
            rect: [0, 0, 128, 128],
            reason: "highest_relative_variance".into(),
            relative_variance: n(0.5),
            baseline_thread_s: n(0.3),
            reference_mrse: Some(n(0.001)),
        }],
        trials: vec![Trial {
            id: "light_samples=2".into(),
            tier: 1,
            factor: "light_samples".into(),
            value: "2".into(),
            flag: Some("--light-samples".into()),
            usd_attribute: Some("crust:lightSamples".into()),
            per_crop: vec![CropTrial {
                crop: "crop_a".into(),
                spp: 16,
                delta_eff: vec![n(1.2), n(1.3), n(1.25)],
                median: n(1.25),
                min: n(1.2),
                max: n(1.3),
                mrse_baseline: n(0.02),
                mrse_trial: n(0.012),
                render_baseline_s: n(0.1),
                render_trial_s: n(0.13),
                setup_trial_s: n(0.0),
                verdict: Verdict::Better,
                mean_luminance_baseline: n(0.21),
                mean_luminance_trial: n(0.2102),
                luminance_shift: n(0.001),
                luminance_shift_z: n(0.3),
                mrse_baseline_untrimmed: n(0.025),
                mrse_trial_untrimmed: n(0.016),
                delta_eff_untrimmed: vec![n(1.15), n(1.25), n(1.2)],
                noise_floor: Some(n(1.04)),
            }],
            overall_delta_eff: Some(n(1.25)),
            verdict: Verdict::Better,
            delta_eff_at_target: Some(n(1.25)),
        }],
        sample_budget: Some(SampleBudget {
            target_mrse: n(0.0025),
            target_from: "variance_threshold".into(),
            settings: "light_samples=2".into(),
            estimate: true,
            spp_to_target: Some(n(48.0)),
            projected_render_s: Some(n(9.6)),
            adaptive: Vec::new(),
            projected_setup_s: Some(n(0.0)),
        }),
        picture_changing: PictureChanging {
            clamp: Some(ClampResult {
                limit: n(10.0),
                removed_luminance_share: n(0.004),
                mean_removed_luminance: n(0.001),
                pixels_affected_share: n(0.02),
            }),
            max_depth: DepthResult {
                max_depth: 32,
                ended_by_depth_share: n(0.0),
                half_depth: None,
            },
            subdivision: SubdivisionResult::default(),
            light_sampling_reach: Some(vec![Reach {
                crop: "crop_a".into(),
                reach: n(0.998),
                z: n(-0.4),
            }]),
        },
        not_tried: vec![NotTried {
            id: "combined".into(),
            tier: 1,
            reason: "not_applicable".into(),
            detail: Some("1 factor(s) better; a combination needs two".into()),
        }],
        suggestions: vec![Suggestion {
            id: "light_samples=2".into(),
            flag: Some("--light-samples".into()),
            usd_attribute: Some("crust:lightSamples".into()),
            value: "2".into(),
            expected_delta_eff: n(1.25),
            evidence: vec!["light_samples=2".into()],
            expected_delta_eff_at_target: n(1.25),
        }],
        converged: false,
        suggested_command: "crust render -i samples/cornellbox.usda --light-samples 2".into(),
        deltas: Some(Deltas {
            comparable: false,
            note: Some("not comparable: a different camera".into()),
            ..Deltas::default()
        }),
    }
}

const KEYS: &[&str] = &[
    "format",
    "crust_version",
    "scene",
    "effective_settings",
    "run",
    "static_findings",
    "baseline",
    "noise_breakdown",
    "crops",
    "trials",
    "sample_budget",
    "picture_changing",
    "not_tried",
    "suggestions",
    "converged",
    "suggested_command",
    "deltas",
];

/// The top-level keys of a JSON object, in the order the text has them.
/// `serde_json::Value` would sort them, so they are read off the text: a
/// pretty-printed top-level key sits at two spaces of indent.
fn top_level_keys(json: &str) -> Vec<String> {
    json.lines()
        .filter_map(|l| l.strip_prefix("  \""))
        .filter_map(|l| l.split_once('"').map(|(k, _)| k.to_owned()))
        .collect()
}

#[test]
fn the_json_keys_are_in_the_spec_order() {
    let json = fixture().to_json();
    assert_eq!(top_level_keys(&json), KEYS);
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(v["format"], "crust-diagnostic/1");
    // Without `--baseline`, no deltas key at all.
    let mut r = fixture();
    r.deltas = None;
    assert_eq!(top_level_keys(&r.to_json()), &KEYS[..KEYS.len() - 1]);
}

#[test]
fn floats_carry_four_significant_digits() {
    let json = fixture().to_json();
    assert!(json.contains("\"used_s\": 61.23,"), "{json}");
    assert!(json.contains("\"mrse\": 0.01235,"));
    assert!(json.contains("\"rays_per_s\": 12350000.0,"));
    assert_eq!(sig4(0.000123456), 0.0001235);
    assert_eq!(sig4(-98765.4), -98770.0);
    assert_eq!(sig4(0.0), 0.0);
    // Not finite is null.
    assert_eq!(serde_json::to_string(&Num(f64::NAN)).unwrap(), "null");
    assert_eq!(serde_json::to_string(&Num(f64::INFINITY)).unwrap(), "null");
}

#[test]
fn a_report_reads_back() {
    let json = fixture().to_json();
    let back: Report = serde_json::from_str(&json).expect("reads back");
    assert_eq!(back.to_json(), json);
}

/// The Markdown of the fixture, pinned. Rendered from the value alone: the
/// verdict block first, then the sections in the JSON's order.
#[test]
fn the_markdown_snapshot() {
    let md = fixture().to_markdown();
    // A Windows checkout with `core.autocrlf` has the file in CRLF; the
    // rendering is LF everywhere, so only the committed side is normalised.
    let expected = include_str!("snapshots/fixture.md").replace("\r\n", "\n");
    if md != expected {
        let out = std::env::temp_dir().join("crust-diagnostic-fixture.md");
        let _ = std::fs::write(&out, &md);
        panic!(
            "the Markdown changed; the new rendering is at {} — review it and copy it to \
             src/diagnostic/snapshots/fixture.md",
            out.display()
        );
    }
    // Sections in JSON order, the verdict first.
    let headings: Vec<&str> = md.lines().filter(|l| l.starts_with("## ")).collect();
    assert_eq!(
        headings,
        [
            "## Verdict",
            "## Scene",
            "## Effective settings",
            "## Run",
            "## Static findings",
            "## Baseline",
            "## Noise breakdown",
            "## Crops",
            "## Trials",
            "## Sample budget (estimates)",
            "## Picture-changing settings (measured, not ranked)",
            "## Not tried",
            "## Suggestions",
            "## Converged",
            "## Suggested command",
            "## Deltas",
        ]
    );
}

// -- --baseline ---------------------------------------------------------------

fn previous() -> Report {
    let mut r = fixture();
    r.deltas = None;
    r
}

#[test]
fn a_comparable_baseline_gives_deltas() {
    let prev = previous();
    let mut now = previous();
    now.effective_settings[0].value = "learned".into();
    now.baseline.time_s = Num(1.6);
    now.baseline.mrse = Num(0.006);
    now.static_findings.clear();
    now.suggestions.clear();
    let d = super::compare::deltas(&prev.to_json(), &now);
    assert!(d.comparable, "{:?}", d.note);
    assert_eq!(
        d.settings_changed,
        [SettingChange {
            name: "light_selection".into(),
            from: "power".into(),
            to: "learned".into(),
        }]
    );
    let t = d.baseline_time_s.as_ref().expect("time");
    assert_eq!((t.from.0, t.to.0, t.ratio.0), (3.2, 1.6, 0.5));
    // The spp each calibration picked travels with the MRSE, and the
    // Markdown says the time ratio is no evidence.
    let spp = d.baseline_spp.as_ref().expect("spp");
    assert_eq!((spp.from.0, spp.to.0), (16.0, 16.0));
    now.deltas = Some(d.clone());
    let md = now.to_markdown();
    assert!(md.contains("indicative only"), "{md}");
    assert!(md.contains("at 16 → 16 spp"), "{md}");
    // From the JSON: four significant digits on the way.
    assert!((d.baseline_mrse.expect("mrse").from.0 - 0.01235).abs() < 1e-12);
    assert_eq!(d.findings_resolved, ["textures_without_tx"]);
    assert!(d.findings_new.is_empty());
    assert_eq!(d.suggestions_gone, ["light_samples=2"]);
}

#[test]
fn another_camera_is_not_comparable() {
    let prev = previous();
    let mut now = previous();
    now.scene.camera = Some("/cams/other".into());
    let d = super::compare::deltas(&prev.to_json(), &now);
    assert!(!d.comparable);
    assert_eq!(
        d.note.as_deref(),
        Some("not comparable: a different camera")
    );
    assert!(d.baseline_time_s.is_none() && d.settings_changed.is_empty());
}

#[test]
fn another_format_version_is_not_comparable() {
    let json = previous()
        .to_json()
        .replace("crust-diagnostic/1", "crust-diagnostic/0");
    let d = super::compare::deltas(&json, &previous());
    assert!(!d.comparable);
    assert!(d.note.unwrap().contains("format crust-diagnostic/0"));
    let d = super::compare::deltas("not json", &previous());
    assert!(!d.comparable);
}

#[test]
fn every_other_scene_difference_refuses_too() {
    let prev = previous().to_json();
    let cases: [fn(&mut Report); 4] = [
        |r| r.scene.path = "other.usda".into(),
        |r| r.scene.frame = Some(12.0),
        |r| r.scene.resolution = [320, 180],
        |r| r.scene.region = Some([0, 0, 64, 64]),
    ];
    for change in cases {
        let mut now = previous();
        change(&mut now);
        assert!(!super::compare::deltas(&prev, &now).comparable);
    }
}

// -- The baseline's measurements ------------------------------------------------

use crate::tracer::Instruments;
use crate::{Buffer, Renderer, Scene};

fn sample(name: &str) -> Scene {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name);
    Scene::from_usd(&path).unwrap_or_else(|e| panic!("load {name}: {e}"))
}

fn bits(b: &Buffer) -> Vec<u32> {
    let (w, h) = b.size();
    (0..w * h)
        .flat_map(|q| {
            let (r, g, bl) = b.get_rgb(q % w, q / w);
            [r.to_bits(), g.to_bits(), bl.to_bits()]
        })
        .collect()
}

/// The baseline as the diagnostic renders it: probe settings, the noise
/// request, every instrument on.
fn baseline(name: &str, label: bool) -> crate::tracer::Measured {
    let mut scene = sample(name);
    let settings = super::probe(scene.settings.with_resolution(48, 32), 16);
    let groups = if label {
        super::noise::label_groups(&mut scene.lights, super::MAX_GROUP_LIGHTS)
    } else {
        super::noise::Groups {
            by: "none",
            tags: Vec::new(),
        }
    };
    let request = super::noise::request(&super::noise::rows(&groups));
    let r = Renderer::new(scene.camera, scene.world, scene.lights, settings);
    r.render_measured(
        Some(&request),
        Instruments {
            tile_times: true,
            clamp: Some(1.0),
            variance: true,
            ..Instruments::default()
        },
    )
}

/// D5 rests on this: a configuration renders the same image,
/// and the same variance, every time — only its time varies between
/// repeats. And the instruments change nothing of it.
#[test]
fn the_baseline_is_deterministic() {
    for name in ["cornellbox.usda", "veach_mis.usda"] {
        let a = baseline(name, true);
        let b = baseline(name, true);
        assert!(
            bits(&a.buffer) == bits(&b.buffer),
            "{name}: two baselines differ"
        );
        assert!(a.var_map == b.var_map, "{name}: variance differs");
        let mut scene = sample(name);
        let settings = super::probe(scene.settings.with_resolution(48, 32), 16);
        scene.settings = settings;
        let plain =
            Renderer::new(scene.camera, scene.world, scene.lights, settings).render_with_tiles();
        assert!(
            bits(&a.buffer) == bits(&plain),
            "{name}: the instruments changed the image"
        );
    }
}

/// Per-light labels route contributions to light groups; the beauty does
/// not move by a bit (veach_mis: four lights, none tagged).
#[test]
fn labelling_lights_changes_no_value() {
    let mut lights = sample("veach_mis.usda").lights;
    let groups = super::noise::label_groups(&mut lights, super::MAX_GROUP_LIGHTS);
    assert_eq!(groups.by, "light");
    assert_eq!(groups.tags.len(), 4);
    assert!(
        groups.tags.iter().all(|(k, _)| k.starts_with('/')),
        "{groups:?}"
    );
    let labelled = baseline("veach_mis.usda", true);
    let plain = baseline("veach_mis.usda", false);
    assert!(bits(&labelled.buffer) == bits(&plain.buffer));
    // Authored tags win; too many lights gives no groups.
    let mut many = crate::LightList::new();
    for _ in 0..9 {
        many.add(crate::DistantLight::new(
            crate::Vec3A::Y,
            crate::Vec3A::ONE,
            0.0,
        ));
    }
    assert_eq!(super::noise::label_groups(&mut many, 8).by, "none");
    many.set_lpe_tag(3, Some("key"));
    let g = super::noise::label_groups(&mut many, 8);
    assert_eq!((g.by, g.tags.len()), ("lpe_tag", 1));
}

/// The seven transport rows partition the beauty on the Cornell box
/// (`unlit_emitters` overlaps them and is left out).
#[test]
fn the_transport_rows_sum_to_the_beauty() {
    let m = baseline("cornellbox.usda", false);
    let film = m.film.as_ref().expect("a film");
    let (w, h) = m.buffer.size();
    let parts: Vec<Vec<Vec<f32>>> = super::noise::COMPONENTS[..super::noise::PARTITION]
        .iter()
        .map(|(k, e)| {
            let v = crate::AovVar {
                prim_path: format!("/crust/diagnostic/{k}"),
                name: (*k).into(),
                channel_prefix: None,
                source: crate::AovSource::Lpe,
                components: 3,
                precision: crate::Precision::Float,
                accumulation: crate::Accumulation::Filtered,
                clear: 0.0,
                expression: Some((*e).into()),
                raw: false,
                variance: false,
            };
            film.var_channels(&m.buffer, &v)
        })
        .collect();
    let mut lit = 0;
    for q in 0..w * h {
        let (r, g, b) = m.buffer.get_rgb(q % w, q / w);
        for (c, whole) in [r, g, b].into_iter().enumerate() {
            let sum: f64 = parts.iter().map(|p| p[c][q] as f64).sum();
            assert!(
                (sum - whole as f64).abs() <= 1e-4 * (1.0 + whole.abs() as f64),
                "pixel {q} channel {c}: rows sum to {sum}, beauty {whole}"
            );
            lit += (whole > 0.0) as usize;
        }
    }
    assert!(lit > 0);
}

// -- Convergence and the suggested command ----------------------------------------

#[test]
fn converged_needs_no_better_trial_and_no_actionable_finding() {
    let r = fixture();
    // Better at 1.25 overall: not converged.
    assert!(!super::converged(&r.trials, &[]));
    let mut t = r.trials.clone();
    // Better, but vetoed at the target.
    t[0].delta_eff_at_target = Some(Num(0.9));
    assert!(super::converged(&t, &[]));
    t[0].delta_eff_at_target = None;
    assert!(super::converged(&t, &[]));
    t[0].delta_eff_at_target = Some(Num(1.25));
    t[0].overall_delta_eff = Some(Num(1.08));
    // Better, but under the suggestion bar.
    assert!(super::converged(&t, &[]));
    // Biased whatever its ΔEff: not a reason to keep going.
    t[0].verdict = Verdict::Biased;
    t[0].overall_delta_eff = Some(Num(14.0));
    assert!(super::converged(&t, &[]));
    t[0].verdict = Verdict::Inconclusive;
    t[0].overall_delta_eff = Some(Num(3.0));
    assert!(super::converged(&t, &[]));
    // An actionable time finding keeps it open; an action-less one does not.
    assert!(!super::converged(&t, &r.static_findings));
    let mut f = r.static_findings.clone();
    f[0].action = Action::None {
        none: "no setting".into(),
    };
    assert!(super::converged(&t, &f));
    // A memory finding never blocks convergence.
    f[0].action = r.static_findings[0].action.clone();
    f[0].kind = FindingKind::Memory;
    assert!(super::converged(&t, &f));
}

#[test]
fn the_suggested_command_applies_every_suggestion() {
    let mut o = super::Options::new("shots/a b.usda");
    o.camera = Some("/cams/main".into());
    o.frame = Some(1012.0);
    o.auto_tx = true;
    let s = crate::RenderSettings::default()
        .with_light_selection(crate::LightSelection::Learned)
        .with_light_samples(2, 1);
    let stage_only = Suggestion {
        id: "variance_threshold=0.01".into(),
        flag: None,
        usd_attribute: Some("crust:varianceThreshold".into()),
        value: "0.01".into(),
        expected_delta_eff: Num(1.3),
        evidence: vec!["combined".into()],
        expected_delta_eff_at_target: Num(1.3),
    };
    let cmd = super::command(&o, &s, &[stage_only], None);
    assert_eq!(
        cmd,
        "crust render -i 'shots/a b.usda' -f 1012 --camera /cams/main --strategy power \
         --light-selection learned --light-samples 2 --light-samples-indirect 1 --auto-tx  \
         # and author on the stage: crust:varianceThreshold = 0.01"
    );
    o.region = Some(crate::PixelRect::new(0, 0, 64, 32));
    let cmd = super::command(&o, &s, &[], o.region);
    assert!(
        cmd.contains("--region 0,0,64,32") && !cmd.contains('#'),
        "{cmd}"
    );
}

// -- Review fixes -------------------------------------------------------------------

/// Tier 1 tries the other MIS heuristic only: `light` and `bsdf` are not
/// swaps (ALab: light-only was ranked ΔEff 14 and renders 61% darker).
#[test]
fn tier_one_never_tries_a_single_strategy() {
    use crate::SamplingStrategy::{BalanceMis, BsdfOnly, LightOnly, PowerMis};
    let values = |base: crate::SamplingStrategy| -> Vec<String> {
        let s = crate::RenderSettings::default().with_sampling_strategy(base);
        super::changes("strategy", s, 4)
            .expect("applicable")
            .into_iter()
            .map(|c| c.value)
            .collect()
    };
    assert_eq!(values(PowerMis), ["balance"]);
    assert_eq!(values(BalanceMis), ["power"]);
    // A stage authoring a single strategy gets both heuristics tried.
    assert_eq!(values(LightOnly), ["power", "balance"]);
    assert_eq!(values(BsdfOnly), ["power", "balance"]);
    // No factor, from any authored strategy, ever tries one.
    for base in [PowerMis, BalanceMis, LightOnly, BsdfOnly] {
        let s = crate::RenderSettings::default().with_sampling_strategy(base);
        for factor in super::noise::FACTORS {
            for c in super::changes(factor, s, 20).unwrap_or_default() {
                let tried = (c.apply)(s, &c.value).sampling_strategy();
                assert!(
                    tried == base || matches!(tried, PowerMis | BalanceMis),
                    "{factor}={} from {base}",
                    c.value
                );
            }
        }
    }
}

/// The Ptex rate is the reader cache's own: a tiled lookup can make
/// several cache operations, so hits can outnumber reader lookups.
#[test]
fn the_ptex_hit_rate_never_passes_one() {
    let before = crate::PtexCacheStats::default();
    let after = crate::PtexCacheStats {
        micro_hits: 100,
        reader_lookups: 50,
        cache_hits: 140,
        cache_misses: 10,
        ..Default::default()
    };
    // The old ratio, (micro + cache hits) / (micro + reader lookups): 1.6.
    assert!((100 + 140) as f64 / (100 + 50) as f64 > 1.0);
    let (lookups, hits) = super::ptex_cache_delta(&before, &after);
    assert_eq!((lookups, hits), (150, 140));
    assert!(hits <= lookups);
}

// -- Hardened verdicts ----------------------------------------------------------------

/// Pair 0 renders with the scene's own seed, bit for bit the image a
/// render without a seed override makes; every other pair with its own,
/// the same in every run.
#[test]
fn each_pair_has_its_own_fixed_seed() {
    assert_eq!(super::seed(1004, 0), 1004);
    assert_eq!(super::seed(1004, 1), 1004 + 0x85EB_CA6B);
    assert_eq!(super::seed(1004, 2), super::seed(1004, 2));
    let scene = sample("cornellbox.usda");
    // 16 spp, as every image comparison here: adaptive sampling is off in
    // a probe, so every pixel takes exactly that many.
    let settings = super::probe(scene.settings.with_resolution(48, 32), 16);
    let mut r = Renderer::new(scene.camera, scene.world, scene.lights, settings);
    let mut image = |s: crate::RenderSettings| bits(&super::shoot(&mut r, s).1.buffer);
    let today = image(settings);
    let frame = settings.frame();
    assert!(image(settings.with_frame(super::seed(frame, 0))) == today);
    let one = image(settings.with_frame(super::seed(frame, 1)));
    assert!(one != today, "pairs 0 and 1 render the same image");
    assert!(image(settings.with_frame(super::seed(frame, 1))) == one);
    assert!(image(settings.with_frame(super::seed(frame, 2))) != one);
}

/// Every pair is an independent draw (D7): no pair's seed is another's. The
/// tracer seeds with `frame as u32`, so the check is modulo 2³², and it does
/// not depend on the frame.
#[test]
fn no_pair_shares_a_seed() {
    for frame in [0isize, 1004, -7] {
        let mut seen = std::collections::HashMap::new();
        for i in 0..64u32 {
            let s = super::seed(frame, i) as u32;
            if let Some(other) = seen.insert(s, i) {
                panic!("frame {frame}: pair {i} reuses pair {other}'s seed");
            }
        }
    }
}

/// A 20×20 crop rendered at luminance `lum`, every pixel's variance `var`.
fn shot(lum: f64, var: f64, render_s: f64) -> super::Shot {
    super::Shot {
        image: super::CropImage {
            lum: vec![lum; 400],
            var: vec![var; 400],
        },
        selection_s: 0.0,
        render_s,
    }
}

/// A trial of three pairs on one crop: the baseline at 0.5, the trial at
/// `lum` with variance `var`, both rendering in 0.1 s.
fn running(id: &str, lum: f64, var: f64) -> super::Running {
    let (factor, value) = id.split_once('=').unwrap_or((id, ""));
    let pairs = (0..3)
        .map(|_| (shot(0.5, 1e-4, 0.1), shot(lum, var, 0.1)))
        .collect();
    super::Running {
        changes: Vec::new(),
        id: id.into(),
        factor: factor.into(),
        value: value.into(),
        per_crop: vec![super::CropRun::new(pairs)],
    }
}

fn judge_all(runs: &[super::Running]) -> (Vec<super::CropImage>, Vec<Trial>) {
    let crops = vec![Crop {
        id: "crop_a".into(),
        rect: [0, 0, 20, 20],
        reason: "highest_relative_variance".into(),
        relative_variance: Num(0.1),
        baseline_thread_s: Num(0.1),
        reference_mrse: None,
    }];
    let refs = super::references(runs, 1);
    let floors = super::noise_floors(runs, &refs);
    let j = super::Judging {
        crops: &crops,
        refs: &refs,
        floors: &floors,
        spp: 4,
        setup_b: 0.0,
        render_b: 10.0,
    };
    let judged = runs.iter().map(|r| super::judge(r, &j)).collect();
    (refs, judged)
}

/// The spec's scenario "A setting that darkens the image": a trial at 39%
/// of the baseline's luminance, with a far lower error, is `biased` — out
/// of the reference, the winners and the suggestions, and reported with
/// its efficiency and its shift.
#[test]
fn a_biased_trial_is_reported_never_used() {
    let runs = [
        running("strategy=light", 0.195, 1e-6),
        running("light_samples=2", 0.5, 0.5e-4),
    ];
    let (refs, trials) = judge_all(&runs);
    // The reference blends the baseline and the unbiased trial only: the
    // darkened image, the least noisy, would otherwise carry it.
    assert!(refs[0].lum.iter().all(|&l| (l - 0.5).abs() < 1e-12));
    let light = &trials[0];
    assert_eq!(light.verdict, Verdict::Biased);
    assert!(light.overall_delta_eff.expect("measured").0 > 10.0);
    let c = &light.per_crop[0];
    assert_eq!(c.verdict, Verdict::Biased);
    assert!((c.luminance_shift.0 + 0.61).abs() < 1e-9, "{c:?}");
    assert!(c.luminance_shift_z.0 < -4.0);
    assert!((c.mean_luminance_baseline.0 - 0.5).abs() < 1e-12);
    assert!((c.mean_luminance_trial.0 - 0.195).abs() < 1e-12);
    // Identical baselines across seeds: a floor of exactly 1.
    assert_eq!(c.noise_floor, Some(Num(1.0)));
    let samples = &trials[1];
    assert_eq!(samples.verdict, Verdict::Better);
    assert_eq!(samples.per_crop[0].luminance_shift.0, 0.0);
    // No setup on either side: at the target as overall.
    assert_eq!(samples.delta_eff_at_target, samples.overall_delta_eff);
    assert_eq!(
        super::winners(&trials)
            .iter()
            .map(|(i, _)| trials[*i].id.as_str())
            .collect::<Vec<_>>(),
        ["light_samples=2"]
    );
    assert_eq!(
        super::best(&trials).map(|t| t.id.as_str()),
        Some("light_samples=2")
    );
    // Its report carries the shift beside the efficiency.
    let mut r = fixture();
    r.trials = trials;
    let md = r.to_markdown();
    assert!(
        md.contains("`strategy=light` changes the picture: luminance −61% on crop_a"),
        "{md}"
    );
}

/// The combined trial is guarded like the others: biased, it is reported
/// and never suggested, however efficient.
#[test]
fn a_biased_combination_is_not_suggested() {
    let runs = [
        running("light_samples=2", 0.5, 0.5e-4),
        running("combined", 0.3, 1e-7),
    ];
    let (_, trials) = judge_all(&runs);
    assert_eq!(trials[1].verdict, Verdict::Biased);
    assert_eq!(
        super::best(&trials).map(|t| t.id.as_str()),
        Some("light_samples=2")
    );
}

/// `converged` waits for every trial the probe could not decide.
#[test]
fn insufficient_samples_is_not_converged() {
    let mut t = fixture().trials;
    t[0].verdict = Verdict::Inconclusive;
    assert!(super::converged(&t, &[]));
    t[0].verdict = Verdict::InsufficientSamples;
    assert!(!super::converged(&t, &[]));
    // The verdict block says what to do about it.
    let mut r = fixture();
    r.trials = t;
    r.suggestions.clear();
    r.converged = false;
    let md = r.to_markdown();
    assert!(
        md.contains(
            "- **Converged:** no — 1 trial(s) need more samples to decide: raise `--budget`"
        ),
        "{md}"
    );
}

/// The spec's scenario "The picture comes first": the clamp's bias and
/// the firefly numbers lead the verdict, before the best change.
#[test]
fn the_picture_comes_first() {
    let mut r = fixture();
    let facts = super::checks::Facts {
        clamp: Some((10.0, 0.66, 0.076)),
        top_pixels: Some(super::noise::TopPixels {
            share: 0.41,
            row: Some(("indirect_diffuse".into(), 0.62)),
        }),
        spp: 16,
        reach: vec![("crop_a".into(), 0.39, -30.0)],
        ..super::checks::Facts::default()
    };
    r.static_findings = super::checks::run(&facts);
    let md = r.to_markdown();
    let line = |label: &str| {
        md.lines()
            .position(|l| l.starts_with(&format!("- **{label}:**")))
            .unwrap_or_else(|| panic!("no {label} line"))
    };
    assert!(line("Picture") < line("Best change"));
    let picture = md.lines().nth(line("Picture")).unwrap();
    assert!(
        picture.contains("`clamp_bias`") && picture.contains("66%"),
        "{picture}"
    );
    let noise = md.lines().nth(line("Top noise source")).unwrap();
    assert_eq!(
        noise,
        "- **Top noise source:** indirect_diffuse — 41% of the energy in 0.1% of the pixels; \
         light sampling reaches 39%"
    );
    // Five lines: nothing else joined the block.
    let block: Vec<&str> = md
        .split("## Verdict")
        .nth(1)
        .unwrap()
        .split("## Scene")
        .next()
        .unwrap()
        .lines()
        .filter(|l| l.starts_with("- "))
        .collect();
    assert_eq!(block.len(), 5, "{block:?}");
}

/// `--baseline` reads a report written before the verdicts were hardened:
/// the renamed time keys through their aliases, every new key defaulted.
#[test]
fn a_report_from_before_the_hardening_still_compares() {
    let old = include_str!("snapshots/before_hardening.json");
    assert!(old.contains("\"time_baseline_s\""));
    let back: Report = serde_json::from_str(old).expect("an older report parses");
    let c = &back.trials[0].per_crop[0];
    assert_eq!((c.render_baseline_s.0, c.render_trial_s.0), (0.1, 0.13));
    assert_eq!(c.noise_floor, None);
    assert!(back.run.seeds.is_empty());
    assert!(back.picture_changing.light_sampling_reach.is_none());
    let mut now = previous();
    now.suggestions.clear();
    let d = super::compare::deltas(old, &now);
    assert!(d.comparable, "{:?}", d.note);
    assert_eq!(d.suggestions_gone, ["light_samples=2"]);
}

/// A report from before the guiding trial was removed names a `guiding`
/// trial, its suggestion and its finding: `--baseline` reads it all the same,
/// comparing what is still there and reporting the vanished ones as gone.
#[test]
fn a_baseline_that_ran_the_removed_guiding_trial_still_compares() {
    let mut old: serde_json::Value =
        serde_json::from_str(include_str!("snapshots/before_hardening.json")).unwrap();
    let mut trial = old["trials"][0].clone();
    trial["id"] = "guiding=true".into();
    trial["factor"] = "guiding".into();
    trial["value"] = "true".into();
    trial["flag"] = serde_json::Value::Null;
    trial["usd_attribute"] = "crust:pathGuiding".into();
    old["trials"].as_array_mut().unwrap().push(trial);
    let mut suggestion = old["suggestions"][0].clone();
    suggestion["id"] = "guiding=true".into();
    old["suggestions"].as_array_mut().unwrap().push(suggestion);
    let mut finding = old["static_findings"][0].clone();
    finding["id"] = "guiding_without_indirect".into();
    old["static_findings"].as_array_mut().unwrap().push(finding);
    let old = old.to_string();
    let mut now = previous();
    now.suggestions.clear();
    let d = super::compare::deltas(&old, &now);
    assert!(d.comparable, "{:?}", d.note);
    assert!(d.suggestions_gone.contains(&"light_samples=2".to_owned()));
    assert!(d.suggestions_gone.contains(&"guiding=true".to_owned()));
    assert!(d.baseline_mrse.is_some());
}

/// Light seen directly or in a reflection is never what the brightest
/// pixels' energy is attributed to: `veach_mis`'s lights in its glossy
/// plates are highlights, not fireflies.
#[test]
fn highlights_are_not_fireflies() {
    let m = baseline("veach_mis.usda", false);
    let groups = super::noise::Groups {
        by: "none",
        tags: Vec::new(),
    };
    let rows = super::noise::rows(&groups);
    let luma = sample("veach_mis.usda").lights.luma();
    let top = super::noise::top_pixels(&rows, m.film.as_ref().expect("a film"), &m.buffer, luma)
        .expect("lit");
    assert!(top.share > 0.0 && top.share < 1.0, "{top:?}");
    if let Some((row, _)) = &top.row {
        assert!(!super::noise::SEEN_ROWS.contains(&row.as_str()), "{top:?}");
    }
    assert!(top.share < super::checks::FIREFLY_SHARE, "{top:?}");
}
