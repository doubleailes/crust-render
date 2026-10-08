//! The noise breakdown (design D7): the baseline renders the value and the
//! variance of a fixed set of light path expressions, and each row reports
//! its own relative error — never a share of the beauty's variance, which
//! the components do not partition (`add-lpe-variance`).

use super::report::NoiseRow;
use crate::{
    Accumulation, AovFilm, AovProduct, AovRequest, AovSource, AovVar, Buffer, LightList, Precision,
};

/// `var / max(mean², ε)`: the floor `mean_relative_error` uses, so a black
/// pixel does not dominate.
const EPS: f64 = 1e-4;

/// The transport rows: key, expression. The first seven partition the
/// beauty (`C.*[LO]`), as the `aovs` spec's own partition does with glossy
/// and singular reflection merged; `unlit_emitters` overlaps them — it is
/// the part of every row that ends on an emitter outside the light list.
pub const COMPONENTS: &[(&str, &str)] = &[
    ("emission", "C[LO]"),
    ("direct_diffuse", "C<RD>[LO]"),
    ("indirect_diffuse", "C<RD>.+[LO]"),
    ("direct_glossy", "C<R[GS]>[LO]"),
    ("indirect_glossy", "C<R[GS]>.+[LO]"),
    ("transmission", "C<T.>.*[LO]"),
    ("volume", "C<V.>.*[LO]"),
    ("unlit_emitters", "C.*O"),
];

/// How many of [`COMPONENTS`] partition the beauty.
pub const PARTITION: usize = 7;

/// Rows whose dominance points at the light-side settings.
pub const DIRECT_ROWS: &[&str] = &["direct_diffuse", "direct_glossy"];

/// Rows whose dominance points at guiding and the indirect light samples.
pub const INDIRECT_ROWS: &[&str] = &[
    "indirect_diffuse",
    "indirect_glossy",
    "transmission",
    "volume",
];

/// The light-group expression of `tag`.
pub fn group_expression(tag: &str) -> String {
    format!("C.*<L.'{tag}'>")
}

/// How the light groups are formed: by authored tags, by the diagnostic's
/// own per-light labels, or not at all.
#[derive(Debug, Clone, PartialEq)]
pub struct Groups {
    /// `lpe_tag`, `light` or `none`.
    pub by: &'static str,
    /// `(key, tag)` per group, in light order.
    pub tags: Vec<(String, String)>,
}

/// The light groups of `lights`: one per distinct authored
/// `crust:light:lpeTag`; with none authored and at most `max_lights`
/// lights, one per light — labelled here, on the diagnostic's own copy of
/// the list, by the light's name (its prim path) or its index. Labels
/// change only which expression a contribution routes to, never a value.
pub fn label_groups(lights: &mut LightList, max_lights: usize) -> Groups {
    let n = lights.count();
    let mut tags: Vec<(String, String)> = Vec::new();
    for i in 0..n {
        if let Some(t) = lights.lpe_tag(i)
            && !tags.iter().any(|(_, x)| x == t)
        {
            tags.push((t.to_owned(), t.to_owned()));
        }
    }
    if !tags.is_empty() {
        return Groups {
            by: "lpe_tag",
            tags,
        };
    }
    if n == 0 || n > max_lights {
        return Groups {
            by: "none",
            tags: Vec::new(),
        };
    }
    for i in 0..n {
        // A quote would end the label inside the expression.
        let name = match lights.name(i) {
            Some(name) if !name.contains('\'') => name.to_owned(),
            _ => format!("light{i}"),
        };
        lights.set_lpe_tag(i, Some(&name));
        tags.push((name.clone(), name));
    }
    Groups { by: "light", tags }
}

fn var(key: &str, expression: &str, variance: bool) -> AovVar {
    AovVar {
        prim_path: format!("/crust/diagnostic/{key}"),
        name: if variance {
            format!("{key}.variance")
        } else {
            key.to_owned()
        },
        channel_prefix: None,
        source: AovSource::Lpe,
        components: if variance { 1 } else { 3 },
        precision: Precision::Float,
        accumulation: Accumulation::Filtered,
        clear: 0.0,
        expression: Some(expression.to_owned()),
        raw: false,
        variance,
    }
}

/// Every `(key, expression)` the baseline renders: the components, then
/// the light groups.
pub fn rows(groups: &Groups) -> Vec<(String, String)> {
    COMPONENTS
        .iter()
        .map(|&(k, e)| (k.to_owned(), e.to_owned()))
        .chain(
            groups
                .tags
                .iter()
                .map(|(k, t)| (k.clone(), group_expression(t))),
        )
        .collect()
}

/// The engine-built request: a value and a variance var per row.
pub fn request(rows: &[(String, String)]) -> AovRequest {
    AovRequest {
        products: vec![AovProduct {
            prim_path: "/crust/diagnostic".into(),
            name: String::new(),
            vars: rows
                .iter()
                .flat_map(|(k, e)| [var(k, e, false), var(k, e, true)])
                .collect(),
            attributes: Vec::new(),
        }],
    }
}

/// Each row's statistics over the film, from the beauty's luminance.
pub fn measure(
    rows: &[(String, String)],
    film: &AovFilm,
    beauty: &Buffer,
    luma: utils::Luma,
) -> Vec<NoiseRow> {
    let (w, h) = beauty.size();
    let beauty_lum: Vec<f64> = (0..w * h)
        .map(|q| {
            let (r, g, b) = beauty.get_rgb(q % w, q / w);
            luma.of(glam::Vec3A::new(r, g, b)) as f64
        })
        .collect();
    let n = (w * h).max(1) as f64;
    rows.iter()
        .map(|(key, expr)| {
            let value = film.var_channels(beauty, &var(key, expr, false));
            let variance = film.var_channels(beauty, &var(key, expr, true));
            let (mut lum_sum, mut own, mut vs_beauty) = (0.0, 0.0, 0.0);
            for q in 0..w * h {
                let lum = luma.of(glam::Vec3A::new(value[0][q], value[1][q], value[2][q])) as f64;
                let v = variance[0][q] as f64;
                lum_sum += lum;
                own += v / (lum * lum).max(EPS);
                vs_beauty += v / (beauty_lum[q] * beauty_lum[q]).max(EPS);
            }
            NoiseRow {
                key: key.clone(),
                expression: expr.clone(),
                mean_luminance: (lum_sum / n).into(),
                relative_error: (own / n).into(),
                relative_error_vs_beauty: (vs_beauty / n).into(),
            }
        })
        .collect()
}

/// The partition row with the largest error against the beauty, if any row
/// carries any.
pub fn dominant(components: &[NoiseRow]) -> Option<String> {
    components
        .iter()
        .take(PARTITION)
        .filter(|r| r.relative_error_vs_beauty.0 > 0.0)
        .max_by(|a, b| {
            a.relative_error_vs_beauty
                .0
                .total_cmp(&b.relative_error_vs_beauty.0)
        })
        .map(|r| r.key.clone())
}

/// One ordering rule: when it holds, its factors move to the front of
/// tier 1. Data, so the ordering is documented and tested in one place.
pub struct Rule {
    pub when: &'static str,
    pub holds: fn(dominant: Option<&str>, lights: usize) -> bool,
    pub first: &'static [&'static str],
}

/// The tier-1 factors in their default order.
pub const FACTORS: &[&str] = &[
    "strategy",
    "light_selection",
    "light_samples",
    "light_samples_indirect",
    "guiding",
];

/// The ordering rules, strongest first.
pub const RULES: &[Rule] = &[
    Rule {
        when: "more than 8 lights",
        holds: |_, lights| lights > super::checks::MANY_LIGHTS,
        first: &["light_selection"],
    },
    Rule {
        when: "direct rows dominate",
        holds: |d, _| d.is_some_and(|d| DIRECT_ROWS.contains(&d)),
        first: &["light_selection", "light_samples"],
    },
    Rule {
        when: "indirect, glossy or volume rows dominate",
        holds: |d, _| d.is_some_and(|d| INDIRECT_ROWS.contains(&d)),
        first: &["guiding", "light_samples_indirect"],
    },
];

/// What every holding rule says, for the log.
pub fn holding(dominant: Option<&str>, lights: usize) -> Vec<&'static str> {
    RULES
        .iter()
        .filter(|r| (r.holds)(dominant, lights))
        .map(|r| r.when)
        .collect()
}

/// The tier-1 factors in the order the noise asks for: every holding
/// rule's factors first, in rule order, then the rest in [`FACTORS`] order.
/// The order matters only when the budget cannot fit the whole tier.
pub fn factor_order(dominant: Option<&str>, lights: usize) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for rule in RULES {
        if (rule.holds)(dominant, lights) {
            for f in rule.first {
                if !out.contains(f) {
                    out.push(f);
                }
            }
        }
    }
    for f in FACTORS {
        if !out.contains(f) {
            out.push(f);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_expression_parses() {
        for (_, e) in COMPONENTS {
            crate::lpe::validate(e).unwrap_or_else(|err| panic!("{e}: {err:?}"));
        }
        crate::lpe::validate(&group_expression("/World/lights/key")).expect("a group");
    }

    #[test]
    fn indirect_noise_runs_guiding_before_strategy() {
        let order = factor_order(Some("indirect_diffuse"), 2);
        let pos = |f| order.iter().position(|x| *x == f).unwrap();
        assert!(pos("guiding") < pos("strategy"));
        assert_eq!(order[0], "guiding");
        assert_eq!(order.len(), FACTORS.len());
    }

    #[test]
    fn direct_noise_and_many_lights_run_light_selection_first() {
        assert_eq!(
            factor_order(Some("direct_diffuse"), 2)[..2],
            ["light_selection", "light_samples"]
        );
        assert_eq!(factor_order(None, 20)[0], "light_selection");
        assert_eq!(factor_order(None, 2), FACTORS);
    }

    #[test]
    fn dominance_reads_the_partition_only() {
        let row = |k: &str, e: f64| NoiseRow {
            key: k.into(),
            expression: String::new(),
            mean_luminance: 1.0.into(),
            relative_error: e.into(),
            relative_error_vs_beauty: e.into(),
        };
        let mut rows: Vec<NoiseRow> = COMPONENTS.iter().map(|(k, _)| row(k, 0.0)).collect();
        rows[2].relative_error_vs_beauty = 0.5.into();
        rows[1].relative_error_vs_beauty = 0.2.into();
        // `unlit_emitters` overlaps the partition and never dominates.
        rows[7].relative_error_vs_beauty = 9.0.into();
        assert_eq!(dominant(&rows).as_deref(), Some("indirect_diffuse"));
        let quiet: Vec<NoiseRow> = COMPONENTS.iter().map(|(k, _)| row(k, 0.0)).collect();
        assert_eq!(dominant(&quiet), None);
    }
}
