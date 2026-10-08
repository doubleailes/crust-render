//! Static findings (design D9): what the import and the baseline say about
//! a scene before any trial. Each check is a function from [`Facts`] to at
//! most one [`Finding`]; adding a check is adding a function to [`CHECKS`]
//! and its test.
//!
//! Every action names a flag or attribute crust has, or is `none` with the
//! reason crust has no setting for it — never an invented one.

use super::report::{Action, Evidence, Finding, FindingKind};
use crate::LightSelection;

/// Above this many lights, picking them uniformly wastes most shadow rays.
pub const MANY_LIGHTS: usize = 8;

/// A cache answering fewer lookups than this without the disk is a finding.
pub const MIN_HIT_RATE: f64 = 0.90;

/// A peak resident size above this share of the machine's memory is a
/// finding.
pub const MAX_MEMORY_SHARE: f64 = 0.80;

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
            "the render peaked at {:.0}% of the machine's memory",
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

    /// Every flag and attribute an action names is one crust has.
    #[test]
    fn actions_name_only_real_settings() {
        let flags = ["--auto-tx", "--light-selection"];
        let attributes = ["crust:lightSelection", "crust:pathGuiding"];
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
            ..Facts::default()
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
