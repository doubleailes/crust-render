//! Static findings (design D9): what the import, the baseline and tier 3's
//! measurements say about a scene. Each check is a function from [`Facts`]
//! to at most one [`Finding`]; adding a check is adding a function to
//! [`CHECKS`] and its test. The list is assembled when the run ends, and
//! reported first.
//!
//! Every action names a flag or attribute crust has, or is `none` with the
//! reason crust has no setting for it — never an invented one.

use super::noise::{TOP_PIXELS, TopPixels};
use super::report::{Action, Evidence, Finding, FindingKind, sig4};
use crate::{LightSelection, SamplingStrategy};

/// Above this many lights, picking them uniformly wastes most shadow rays.
pub const MANY_LIGHTS: usize = 8;

/// A cache answering fewer lookups than this without the disk is a finding.
pub const MIN_HIT_RATE: f64 = 0.90;

/// A peak resident size above this share of the machine's memory is a
/// finding.
pub const MAX_MEMORY_SHARE: f64 = 0.80;

/// An authored clamp removing at least this share of the luminance is a
/// correctness finding: about 0.07 stops.
pub const CLAMP_BIAS: f64 = 0.05;

/// The brightest 0.1% of the pixels holding at least this share of the
/// luminance (light seen directly or in a reflection aside) is a finding:
/// a converged, smooth image puts of the order of 0.1–1% there.
pub const FIREFLY_SHARE: f64 = 0.20;

/// Light sampling reaching less than this share of a crop's energy, beyond
/// [`REACH_Z`] standard errors, is a finding.
pub const MIN_REACH: f64 = 0.90;
pub const REACH_Z: f64 = 4.0;

/// Why no setting fixes energy only BSDF sampling finds.
const NO_SETTING_REACHES: &str = "no crust setting makes these paths reachable by light \
     sampling; the indirect clamp (--indirect-clamp, crust:indirectClamp) trades this energy \
     for fireflies, and no clamp value is a gain";

/// What the checks read. The `Option`s are the facts only the baseline
/// knows: `None` before it has run (or when the platform cannot say).
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub lights: usize,
    pub light_selection: LightSelection,
    pub auto_tx: bool,
    /// UV textures that were preloaded because no `.tx` stands beside them.
    pub textures_without_tx: u64,
    /// The baseline's texture lookups and how many the cache answered.
    pub texture_lookups: Option<(u64, u64)>,
    pub ptex_lookups: Option<(u64, u64)>,
    /// Mean luminance of emission from emitters outside the light list
    /// (`C.*O`) in the baseline.
    pub unlit_emission: Option<f64>,
    pub guiding: bool,
    /// Whether any indirect, glossy or volume row dominates the noise.
    pub indirect_dominant: Option<bool>,
    pub peak_mem_bytes: Option<u64>,
    pub machine_mem_bytes: Option<u64>,
    /// The authored sampling strategy.
    pub strategy: SamplingStrategy,
    /// The authored clamp's limit, and the share of the baseline's
    /// luminance and of its pixels it would remove and touch.
    pub clamp: Option<(f64, f64, f64)>,
    /// Where the baseline's brightest pixels' energy is.
    pub top_pixels: Option<TopPixels>,
    /// The baseline's samples per pixel.
    pub spp: u32,
    /// Tier 3's light-sampling reach: `(crop, reach, z)` per crop measured.
    pub reach: Vec<(String, f64, f64)>,
}

impl Facts {
    /// The facts a scene's import alone establishes, every baseline fact
    /// `None` — what `crust check` reports from, and what the diagnostic
    /// builds its own facts on. `textures_without_tx` is the host's count of
    /// UV textures preloaded for want of a `.tx`; `import_peak` the peak
    /// resident size the import reached.
    pub fn from_import(
        scene: &crate::Scene,
        auto_tx: bool,
        textures_without_tx: u64,
        import_peak: Option<u64>,
    ) -> Facts {
        Facts {
            lights: scene.lights.count(),
            light_selection: scene.settings.light_selection(),
            auto_tx,
            textures_without_tx,
            guiding: scene.settings.guiding(),
            peak_mem_bytes: import_peak,
            machine_mem_bytes: crate::machine_memory_bytes(),
            strategy: scene.settings.sampling_strategy(),
            ..Facts::default()
        }
    }
}

type Check = fn(&Facts) -> Option<Finding>;

/// Every check, in report order.
pub const CHECKS: &[Check] = &[
    textures_without_tx,
    texture_cache_hit_rate,
    ptex_cache_hit_rate,
    unlit_emitters,
    many_lights_uniform,
    guiding_without_indirect,
    peak_memory,
    visualization_strategy,
    clamp_bias,
    firefly_energy,
    light_sampling_misses,
];

/// Every finding the facts support, in [`CHECKS`] order.
pub fn run(facts: &Facts) -> Vec<Finding> {
    CHECKS.iter().filter_map(|c| c(facts)).collect()
}

fn finding(
    id: &str,
    kind: FindingKind,
    summary: String,
    evidence: Evidence,
    action: Action,
) -> Option<Finding> {
    Some(Finding {
        id: id.to_owned(),
        kind,
        summary,
        evidence,
        action,
    })
}

/// UV textures without a sibling `.tx` are decoded whole at import and kept
/// resident, rather than streamed by the tile; `--auto-tx` writes the
/// missing ones.
fn textures_without_tx(f: &Facts) -> Option<Finding> {
    if f.auto_tx || f.textures_without_tx == 0 {
        return None;
    }
    finding(
        "textures_without_tx",
        FindingKind::Time,
        format!(
            "{} UV texture(s) have no .tx beside them: each is decoded whole at import and \
             held resident instead of streamed",
            f.textures_without_tx
        ),
        Evidence::new().with("textures", f.textures_without_tx as f64),
        Action::Set {
            flag: Some("--auto-tx".into()),
            usd_attribute: None,
            value: "on".into(),
        },
    )
}

fn hit_rate_finding(
    id: &str,
    what: &str,
    budget_env: &str,
    lookups: Option<(u64, u64)>,
) -> Option<Finding> {
    let (lookups, hits) = lookups?;
    if lookups == 0 {
        return None;
    }
    let rate = hits as f64 / lookups as f64;
    if rate >= MIN_HIT_RATE {
        return None;
    }
    finding(
        id,
        FindingKind::Time,
        format!(
            "the {what} cache answered {:.1}% of lookups without the disk",
            100.0 * rate
        ),
        Evidence::new()
            .with("hit_rate", rate)
            .with("lookups", lookups as f64),
        Action::None {
            none: format!(
                "no render flag or crust:* attribute sets the {what} cache budget; it is the \
                 environment switch {budget_env}"
            ),
        },
    )
}

fn texture_cache_hit_rate(f: &Facts) -> Option<Finding> {
    hit_rate_finding(
        "texture_cache_hit_rate",
        "texture",
        "CRUST_TEX_CACHE_MB",
        f.texture_lookups,
    )
}

fn ptex_cache_hit_rate(f: &Facts) -> Option<Finding> {
    hit_rate_finding(
        "ptex_cache_hit_rate",
        "Ptex",
        "CRUST_PTEX_CACHE_MB",
        f.ptex_lookups,
    )
}

/// Emissive geometry outside the light list is reached only by BSDF
/// sampling, never by next-event estimation.
fn unlit_emitters(f: &Facts) -> Option<Finding> {
    let lum = f.unlit_emission?;
    if lum.is_nan() || lum <= 0.0 {
        return None;
    }
    finding(
        "unlit_emitters",
        FindingKind::Noise,
        "emissive geometry outside the light list lights the scene: only BSDF sampling can \
         find it, never a shadow ray"
            .into(),
        Evidence::new().with("mean_luminance", lum),
        Action::None {
            none: "crust has no setting that makes emissive geometry a light \
                   (MeshLightAPI is not supported); a UsdLux light authored in its place is"
                .into(),
        },
    )
}

/// Many lights picked uniformly: most shadow rays go to lights that barely
/// contribute.
fn many_lights_uniform(f: &Facts) -> Option<Finding> {
    if f.lights <= MANY_LIGHTS || f.light_selection != LightSelection::Uniform {
        return None;
    }
    finding(
        "many_lights_uniform",
        FindingKind::Noise,
        format!(
            "{} lights are picked uniformly: a shadow ray is as likely to go to the dimmest as \
             to the brightest",
            f.lights
        ),
        Evidence::new().with("lights", f.lights as f64),
        Action::Set {
            flag: Some("--light-selection".into()),
            usd_attribute: Some("crust:lightSelection".into()),
            value: "power".into(),
        },
    )
}

/// Guiding is authored on, but no indirect row dominates the noise: its
/// training passes are likely spent for nothing.
fn guiding_without_indirect(f: &Facts) -> Option<Finding> {
    if !f.guiding || f.indirect_dominant? {
        return None;
    }
    finding(
        "guiding_without_indirect",
        FindingKind::Time,
        "path guiding is on, but direct light dominates the noise: its training passes guide \
         paths that carry little of it"
            .into(),
        Evidence::new().with("guiding", 1.0),
        Action::Set {
            flag: None,
            usd_attribute: Some("crust:pathGuiding".into()),
            value: "false".into(),
        },
    )
}

fn peak_memory(f: &Facts) -> Option<Finding> {
    let (peak, total) = (f.peak_mem_bytes?, f.machine_mem_bytes?);
    if total == 0 || (peak as f64) <= MAX_MEMORY_SHARE * total as f64 {
        return None;
    }
    finding(
        "peak_memory",
        FindingKind::Memory,
        format!(
            "memory peaked at {:.0}% of the machine's memory",
            100.0 * peak as f64 / total as f64
        ),
        Evidence::new()
            .with("peak_mem_bytes", peak as f64)
            .with("machine_mem_bytes", total as f64),
        Action::None {
            none: "no single setting bounds memory; subdivision (--subdiv-level, \
                   --subdiv-edge-length) and texture residency are the usual consumers — see \
                   picture_changing.subdivision"
                .into(),
        },
    )
}

/// A single-strategy mode authored: it shows what MIS balances between,
/// and does not converge to the same image wherever one strategy alone
/// cannot reach a light.
fn visualization_strategy(f: &Facts) -> Option<Finding> {
    if !matches!(
        f.strategy,
        SamplingStrategy::LightOnly | SamplingStrategy::BsdfOnly
    ) {
        return None;
    }
    finding(
        "visualization_strategy",
        FindingKind::Correctness,
        format!(
            "the sampling strategy is {}, a visualization mode: it does not converge to the \
             same image as MIS on every scene",
            f.strategy
        ),
        Evidence::new(),
        Action::Set {
            flag: Some("--strategy".into()),
            usd_attribute: Some("crust:samplingStrategy".into()),
            value: "power".into(),
        },
    )
}

/// The authored clamp removes a visible share of the image.
fn clamp_bias(f: &Facts) -> Option<Finding> {
    let (limit, removed, touched) = f.clamp?;
    if removed.is_nan() || removed < CLAMP_BIAS {
        return None;
    }
    finding(
        "clamp_bias",
        FindingKind::Correctness,
        format!(
            "the indirect clamp ({limit}) removes {}% of the luminance, touching {}% of the \
             pixels",
            sig4(100.0 * removed),
            sig4(100.0 * touched)
        ),
        Evidence::new()
            .with("limit", limit)
            .with("removed_luminance_share", removed)
            .with("pixels_affected_share", touched),
        Action::None {
            none: NO_SETTING_REACHES.into(),
        },
    )
}

/// A few pixels hold much of the energy: rare paths carry the picture.
fn firefly_energy(f: &Facts) -> Option<Finding> {
    let top = f.top_pixels.as_ref()?;
    if top.share.is_nan() || top.share < FIREFLY_SHARE {
        return None;
    }
    let mut evidence = Evidence::new()
        .with("top_pixels_share", TOP_PIXELS)
        .with("luminance_share", top.share)
        .with("spp", f.spp as f64);
    let row = match &top.row {
        Some((row, share)) => {
            evidence = evidence.with("row_share", *share);
            format!("; {}% of theirs is {row}", sig4(100.0 * share))
        }
        None => String::new(),
    };
    finding(
        "firefly_energy",
        FindingKind::Noise,
        format!(
            "the brightest {}% of the pixels hold {}% of the luminance (light seen directly \
             or in a reflection aside) at {} spp{row}",
            sig4(100.0 * TOP_PIXELS),
            sig4(100.0 * top.share),
            f.spp
        ),
        evidence,
        Action::None {
            none: NO_SETTING_REACHES.into(),
        },
    )
}

/// Light sampling alone misses part of a crop's energy: it arrives only on
/// paths BSDF sampling finds, which MIS has no partner strategy for.
fn light_sampling_misses(f: &Facts) -> Option<Finding> {
    let (crop, reach, z) = f
        .reach
        .iter()
        .filter(|(_, r, z)| r.is_finite() && z.abs() > REACH_Z)
        .min_by(|a, b| a.1.total_cmp(&b.1))?;
    if *reach >= MIN_REACH {
        return None;
    }
    finding(
        "light_sampling_misses",
        FindingKind::Noise,
        format!(
            "light sampling alone reaches {}% of {crop}'s energy: the rest arrives only on \
             paths BSDF sampling finds",
            sig4(100.0 * reach)
        ),
        Evidence::new().with("reach", *reach).with("z", *z),
        Action::None {
            none: NO_SETTING_REACHES.into(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(f: &Facts) -> Vec<String> {
        run(f).into_iter().map(|f| f.id).collect()
    }

    #[test]
    fn a_quiet_scene_has_no_finding() {
        assert!(ids(&Facts::default()).is_empty());
    }

    #[test]
    fn textures_without_tx_suggest_auto_tx() {
        let f = Facts {
            textures_without_tx: 12,
            ..Facts::default()
        };
        let found = run(&f);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, FindingKind::Time);
        assert_eq!(found[0].evidence.get("textures"), Some(12.0));
        assert!(
            matches!(&found[0].action, Action::Set { flag: Some(flag), .. } if flag == "--auto-tx")
        );
        // Already on: nothing to say.
        assert!(ids(&Facts { auto_tx: true, ..f }).is_empty());
    }

    #[test]
    fn a_cache_below_ninety_percent_is_a_finding_without_a_setting() {
        let f = Facts {
            texture_lookups: Some((1000, 800)),
            ptex_lookups: Some((1000, 950)),
            ..Facts::default()
        };
        let found = run(&f);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "texture_cache_hit_rate");
        assert!(!found[0].action.is_actionable());
        // No lookups is no finding.
        assert!(
            ids(&Facts {
                texture_lookups: Some((0, 0)),
                ..Facts::default()
            })
            .is_empty()
        );
    }

    #[test]
    fn unlit_emitters_have_no_action() {
        let f = Facts {
            unlit_emission: Some(0.3),
            ..Facts::default()
        };
        let found = run(&f);
        assert_eq!(found[0].id, "unlit_emitters");
        assert_eq!(found[0].kind, FindingKind::Noise);
        assert!(!found[0].action.is_actionable());
        assert!(
            ids(&Facts {
                unlit_emission: Some(0.0),
                ..Facts::default()
            })
            .is_empty()
        );
    }

    #[test]
    fn many_lights_picked_uniformly_suggest_power() {
        let f = Facts {
            lights: 9,
            light_selection: LightSelection::Uniform,
            ..Facts::default()
        };
        let found = run(&f);
        assert_eq!(found[0].id, "many_lights_uniform");
        assert_eq!(
            found[0].action,
            Action::Set {
                flag: Some("--light-selection".into()),
                usd_attribute: Some("crust:lightSelection".into()),
                value: "power".into(),
            }
        );
        assert!(
            ids(&Facts {
                lights: 8,
                ..f.clone()
            })
            .is_empty()
        );
        assert!(
            ids(&Facts {
                light_selection: LightSelection::Power,
                ..f
            })
            .is_empty()
        );
    }

    #[test]
    fn guiding_on_direct_noise_suggests_turning_it_off() {
        let f = Facts {
            guiding: true,
            indirect_dominant: Some(false),
            ..Facts::default()
        };
        assert_eq!(ids(&f), ["guiding_without_indirect"]);
        assert!(
            ids(&Facts {
                indirect_dominant: Some(true),
                ..f.clone()
            })
            .is_empty()
        );
        // Unknown before the baseline: no finding yet.
        assert!(
            ids(&Facts {
                indirect_dominant: None,
                ..f
            })
            .is_empty()
        );
    }

    #[test]
    fn peak_memory_near_the_machine_is_a_memory_finding() {
        let gib = 1u64 << 30;
        let f = Facts {
            peak_mem_bytes: Some(14 * gib),
            machine_mem_bytes: Some(16 * gib),
            ..Facts::default()
        };
        let found = run(&f);
        assert_eq!(found[0].id, "peak_memory");
        assert_eq!(found[0].kind, FindingKind::Memory);
        assert!(
            ids(&Facts {
                peak_mem_bytes: Some(8 * gib),
                ..f
            })
            .is_empty()
        );
    }

    #[test]
    fn a_visualization_strategy_suggests_power() {
        for strategy in [SamplingStrategy::LightOnly, SamplingStrategy::BsdfOnly] {
            let found = run(&Facts {
                strategy,
                ..Facts::default()
            });
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].id, "visualization_strategy");
            assert_eq!(found[0].kind, FindingKind::Correctness);
            assert_eq!(
                found[0].action,
                Action::Set {
                    flag: Some("--strategy".into()),
                    usd_attribute: Some("crust:samplingStrategy".into()),
                    value: "power".into(),
                }
            );
        }
        for strategy in [SamplingStrategy::PowerMis, SamplingStrategy::BalanceMis] {
            assert!(
                ids(&Facts {
                    strategy,
                    ..Facts::default()
                })
                .is_empty()
            );
        }
    }

    /// The spec's scenario "A clamp that removes most of the image".
    #[test]
    fn a_clamp_removing_five_percent_is_a_correctness_finding() {
        let at = |removed| Facts {
            clamp: Some((10.0, removed, 0.076)),
            ..Facts::default()
        };
        let found = run(&at(0.66));
        assert_eq!(found[0].id, "clamp_bias");
        assert_eq!(found[0].kind, FindingKind::Correctness);
        assert_eq!(found[0].evidence.get("removed_luminance_share"), Some(0.66));
        assert_eq!(found[0].evidence.get("pixels_affected_share"), Some(0.076));
        assert_eq!(found[0].evidence.get("limit"), Some(10.0));
        assert!(found[0].summary.contains("66%"), "{}", found[0].summary);
        assert!(!found[0].action.is_actionable());
        assert_eq!(ids(&at(CLAMP_BIAS)), ["clamp_bias"]);
        assert!(ids(&at(0.0499)).is_empty());
        assert!(ids(&Facts::default()).is_empty());
    }

    #[test]
    fn energy_in_a_few_pixels_is_a_noise_finding() {
        let at = |share| Facts {
            top_pixels: Some(TopPixels {
                share,
                row: Some(("indirect_diffuse".into(), 0.62)),
            }),
            spp: 16,
            ..Facts::default()
        };
        let found = run(&at(0.41));
        assert_eq!(found[0].id, "firefly_energy");
        assert_eq!(found[0].kind, FindingKind::Noise);
        assert_eq!(found[0].evidence.get("luminance_share"), Some(0.41));
        assert_eq!(found[0].evidence.get("spp"), Some(16.0));
        assert_eq!(found[0].evidence.get("row_share"), Some(0.62));
        assert!(found[0].summary.contains("indirect_diffuse"));
        assert!(!found[0].action.is_actionable());
        assert_eq!(ids(&at(FIREFLY_SHARE)), ["firefly_energy"]);
        assert!(ids(&at(0.199)).is_empty());
    }

    /// The spec's scenario "Energy only BSDF sampling finds".
    #[test]
    fn light_sampling_that_misses_energy_is_a_noise_finding() {
        let at = |reach: Vec<(String, f64, f64)>| Facts {
            reach,
            ..Facts::default()
        };
        let found = run(&at(vec![
            ("crop_a".into(), 0.95, -9.0),
            ("crop_b".into(), 0.39, -30.0),
            // The lowest, but within the noise: not the evidence.
            ("crop_c".into(), 0.2, -3.0),
        ]));
        assert_eq!(found[0].id, "light_sampling_misses");
        assert_eq!(found[0].evidence.get("reach"), Some(0.39));
        assert_eq!(found[0].evidence.get("z"), Some(-30.0));
        assert!(found[0].summary.contains("crop_b"));
        assert!(!found[0].action.is_actionable());
        assert!(ids(&at(vec![("crop_a".into(), MIN_REACH, -9.0)])).is_empty());
        assert!(ids(&at(vec![("crop_a".into(), 0.5, 3.9)])).is_empty());
        assert!(ids(&at(Vec::new())).is_empty());
    }

    /// The three picture findings have no action: none blocks `converged`.
    #[test]
    fn picture_findings_never_block_convergence() {
        let f = Facts {
            clamp: Some((10.0, 0.66, 0.076)),
            top_pixels: Some(TopPixels {
                share: 0.41,
                row: None,
            }),
            reach: vec![("crop_a".into(), 0.39, -30.0)],
            ..Facts::default()
        };
        let found = run(&f);
        assert_eq!(found.len(), 3);
        assert!(crate::diagnostic::converged(&[], &found));
    }

    /// Every flag and attribute an action names is one crust has.
    #[test]
    fn actions_name_only_real_settings() {
        let flags = ["--auto-tx", "--light-selection", "--strategy"];
        let attributes = [
            "crust:lightSelection",
            "crust:pathGuiding",
            "crust:samplingStrategy",
        ];
        let all = Facts {
            lights: 20,
            light_selection: LightSelection::Uniform,
            textures_without_tx: 1,
            texture_lookups: Some((10, 1)),
            ptex_lookups: Some((10, 1)),
            unlit_emission: Some(1.0),
            guiding: true,
            indirect_dominant: Some(false),
            peak_mem_bytes: Some(10),
            machine_mem_bytes: Some(10),
            strategy: SamplingStrategy::LightOnly,
            clamp: Some((10.0, 0.5, 0.1)),
            top_pixels: Some(TopPixels {
                share: 0.5,
                row: None,
            }),
            spp: 4,
            reach: vec![("crop_a".into(), 0.1, -20.0)],
            auto_tx: false,
        };
        let found = run(&all);
        assert_eq!(found.len(), CHECKS.len());
        for f in found {
            if let Action::Set {
                flag,
                usd_attribute,
                ..
            } = &f.action
            {
                assert!(flag.is_some() || usd_attribute.is_some(), "{}", f.id);
                if let Some(flag) = flag {
                    assert!(flags.contains(&flag.as_str()), "{}: {flag}", f.id);
                }
                if let Some(a) = usd_attribute {
                    assert!(attributes.contains(&a.as_str()), "{}: {a}", f.id);
                }
            }
        }
    }
}
