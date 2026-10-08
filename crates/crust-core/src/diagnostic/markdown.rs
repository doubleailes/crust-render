//! The Markdown report: rendered from the [`Report`] value alone, so it
//! cannot disagree with the JSON. A short verdict block comes first, then
//! one section per top-level key, in the JSON's order.

use std::fmt::Write;

use super::report::{Action, Num, Report};

/// `x` as the JSON writes it (four significant digits), or `–`.
fn n(x: Num) -> String {
    if x.0.is_finite() {
        format!("{}", super::report::sig4(x.0))
    } else {
        "–".into()
    }
}

fn opt(x: Option<Num>) -> String {
    x.map_or_else(|| "–".into(), n)
}

fn pct(x: Num) -> String {
    if x.0.is_finite() {
        format!("{}%", super::report::sig4(100.0 * x.0))
    } else {
        "–".into()
    }
}

fn or_dash(s: &Option<String>) -> &str {
    s.as_deref().unwrap_or("–")
}

impl Report {
    /// The Markdown report.
    pub fn to_markdown(&self) -> String {
        let mut o = String::new();
        // `write!` into a `String` cannot fail.
        let _ = self.write_markdown(&mut o);
        o
    }

    fn write_markdown(&self, o: &mut String) -> std::fmt::Result {
        writeln!(o, "# crust diagnostic: {}", self.scene.path)?;
        writeln!(o)?;

        // -- Verdict ---------------------------------------------------
        writeln!(o, "## Verdict")?;
        writeln!(o)?;
        let sink = self.baseline.profile_top.first().map_or_else(
            || "–".to_owned(),
            |p| format!("{} ({})", p.section, pct(p.share)),
        );
        writeln!(o, "- **Top time sink:** {sink}")?;
        writeln!(
            o,
            "- **Top noise source:** {}",
            or_dash(&self.noise_breakdown.dominant)
        )?;
        let best = self.suggestions.first().map_or_else(
            || "none".to_owned(),
            |s| {
                format!(
                    "{} (ΔEff {})",
                    s.flag
                        .as_ref()
                        .map(|f| format!("`{f} {}`", s.value))
                        .or_else(|| s
                            .usd_attribute
                            .as_ref()
                            .map(|a| format!("`{a} = {}`", s.value)))
                        .unwrap_or_else(|| s.id.clone()),
                    n(s.expected_delta_eff)
                )
            },
        );
        writeln!(o, "- **Best change:** {best}")?;
        writeln!(
            o,
            "- **Converged:** {}",
            if self.converged { "yes" } else { "no" }
        )?;
        writeln!(o)?;

        // -- Scene -----------------------------------------------------
        writeln!(o, "## Scene")?;
        writeln!(o)?;
        let s = &self.scene;
        writeln!(o, "- path: `{}`", s.path)?;
        writeln!(
            o,
            "- frame: {}",
            s.frame.map_or_else(|| "–".into(), |f| f.to_string())
        )?;
        writeln!(o, "- camera: {}", or_dash(&s.camera))?;
        writeln!(o, "- resolution: {}×{}", s.resolution[0], s.resolution[1])?;
        writeln!(
            o,
            "- region: {}",
            s.region
                .map_or_else(|| "full frame".into(), |r| format!("{r:?}"))
        )?;
        writeln!(o)?;

        // -- Effective settings ----------------------------------------
        writeln!(o, "## Effective settings")?;
        writeln!(o)?;
        writeln!(o, "| setting | value | flag | USD attribute |")?;
        writeln!(o, "|---|---|---|---|")?;
        for s in &self.effective_settings {
            writeln!(
                o,
                "| {} | {} | {} | {} |",
                s.name,
                s.value,
                or_dash(&s.flag),
                or_dash(&s.usd_attribute)
            )?;
        }
        writeln!(o)?;

        // -- Run -------------------------------------------------------
        writeln!(o, "## Run")?;
        writeln!(o)?;
        let r = &self.run;
        writeln!(
            o,
            "- budget {} s, used {} s (import {} s, not counted), {} threads, {} repeats",
            n(r.budget_s),
            n(r.used_s),
            n(r.import_s),
            r.threads,
            r.repeats
        )?;
        let p = &r.probe_conditions;
        writeln!(
            o,
            "- probe conditions: indirect clamp {}, adaptive sampling {}, fixed spp {}, {}×{}",
            p.indirect_clamp, p.adaptive_sampling, p.fixed_spp, p.resolution[0], p.resolution[1]
        )?;
        for ph in &r.phases {
            writeln!(
                o,
                "- {}: {} s{}",
                ph.name,
                n(ph.time_s),
                if ph.completed { "" } else { " (incomplete)" }
            )?;
        }
        if let Some(phase) = &r.budget_exceeded_in {
            writeln!(o, "- **budget exceeded in {phase}**")?;
        }
        writeln!(o, "- exit status {}", r.exit)?;
        writeln!(o)?;

        // -- Static findings -------------------------------------------
        writeln!(o, "## Static findings")?;
        writeln!(o)?;
        if self.static_findings.is_empty() {
            writeln!(o, "None.")?;
        }
        for f in &self.static_findings {
            let evidence: Vec<String> = f
                .evidence
                .0
                .iter()
                .map(|(k, v)| format!("{k} {}", n(*v)))
                .collect();
            let action = match &f.action {
                Action::Set {
                    flag,
                    usd_attribute,
                    value,
                } => format!(
                    "set {} = {value}",
                    [flag.as_deref(), usd_attribute.as_deref()]
                        .into_iter()
                        .flatten()
                        .map(|x| format!("`{x}`"))
                        .collect::<Vec<_>>()
                        .join(" / ")
                ),
                Action::None { none } => format!("none — {none}"),
            };
            writeln!(
                o,
                "- **{}** ({}): {}. Evidence: {}. Action: {}.",
                f.id,
                f.kind.name(),
                f.summary,
                evidence.join(", "),
                action
            )?;
        }
        writeln!(o)?;

        // -- Baseline --------------------------------------------------
        writeln!(o, "## Baseline")?;
        writeln!(o)?;
        let b = &self.baseline;
        writeln!(
            o,
            "- {} spp full frame in {} s (setup {} s, render {} s; calibration {} s)",
            b.spp,
            n(b.time_s),
            n(b.setup_s),
            n(b.render_s),
            n(b.calibration_time_s)
        )?;
        writeln!(o, "- MRSE {}, {} rays/s", n(b.mrse), n(b.rays_per_s))?;
        writeln!(
            o,
            "- mean path length {}, Russian roulette kill rate {}, ended by max depth {}, \
             shadow rays per vertex {}",
            n(b.mean_path_length),
            pct(b.rr_kill_rate),
            pct(b.ended_by_depth_share),
            n(b.shadow_rays_per_vertex)
        )?;
        let top: Vec<String> = b
            .profile_top
            .iter()
            .map(|p| format!("{} {}", p.section, pct(p.share)))
            .collect();
        writeln!(
            o,
            "- profile: {}",
            if top.is_empty() {
                "–".into()
            } else {
                top.join(", ")
            }
        )?;
        writeln!(
            o,
            "- cache hit rates: texture {}, Ptex {}",
            b.texture_hit_rate.map_or_else(|| "–".into(), pct),
            b.ptex_hit_rate.map_or_else(|| "–".into(), pct)
        )?;
        if let Some(m) = b.peak_mem_bytes {
            writeln!(o, "- peak memory {:.1} MiB", m as f64 / (1u64 << 20) as f64)?;
        }
        writeln!(o)?;

        // -- Noise breakdown -------------------------------------------
        writeln!(o, "## Noise breakdown")?;
        writeln!(o)?;
        writeln!(
            o,
            "Each row's error is its own (`var / mean²`) and against the beauty \
             (`var / beauty²`); rows are not shares of the beauty's variance."
        )?;
        writeln!(o)?;
        writeln!(
            o,
            "| component | expression | mean luminance | relative error | vs beauty |"
        )?;
        writeln!(o, "|---|---|---|---|---|")?;
        let nb = &self.noise_breakdown;
        for row in nb.components.iter().chain(&nb.light_groups) {
            writeln!(
                o,
                "| {} | `{}` | {} | {} | {} |",
                row.key,
                row.expression,
                n(row.mean_luminance),
                n(row.relative_error),
                n(row.relative_error_vs_beauty)
            )?;
        }
        writeln!(o)?;
        writeln!(o, "Light groups by: {}.", nb.light_groups_by)?;
        writeln!(o)?;

        // -- Crops -----------------------------------------------------
        writeln!(o, "## Crops")?;
        writeln!(o)?;
        writeln!(
            o,
            "| crop | rect | reason | relative variance | baseline thread-s | reference MRSE |"
        )?;
        writeln!(o, "|---|---|---|---|---|---|")?;
        for c in &self.crops {
            writeln!(
                o,
                "| {} | {:?} | {} | {} | {} | {} |",
                c.id,
                c.rect,
                c.reason,
                n(c.relative_variance),
                n(c.baseline_thread_s),
                opt(c.reference_mrse)
            )?;
        }
        writeln!(o)?;

        // -- Trials ----------------------------------------------------
        writeln!(o, "## Trials")?;
        writeln!(o)?;
        if self.trials.is_empty() {
            writeln!(o, "None ran.")?;
        } else {
            writeln!(
                o,
                "| trial | overall ΔEff | verdict | per crop (median [min, max]: verdict) |"
            )?;
            writeln!(o, "|---|---|---|---|")?;
            for t in &self.trials {
                let per: Vec<String> = t
                    .per_crop
                    .iter()
                    .map(|c| {
                        format!(
                            "{} @{} spp: {} [{}, {}]: {}",
                            c.crop,
                            c.spp,
                            n(c.median),
                            n(c.min),
                            n(c.max),
                            c.verdict.name()
                        )
                    })
                    .collect();
                writeln!(
                    o,
                    "| {} | {} | {} | {} |",
                    t.id,
                    opt(t.overall_delta_eff),
                    t.verdict.name(),
                    per.join("; ")
                )?;
            }
        }
        writeln!(o)?;

        // -- Sample budget ---------------------------------------------
        writeln!(o, "## Sample budget (estimates)")?;
        writeln!(o)?;
        match &self.sample_budget {
            None => writeln!(o, "Not measured.")?,
            Some(sb) => {
                writeln!(
                    o,
                    "- target MRSE {} (from {}), with {} settings",
                    n(sb.target_mrse),
                    sb.target_from,
                    sb.settings
                )?;
                writeln!(
                    o,
                    "- estimated spp to reach it: {}; projected full-frame render time {} s \
                     (sampling only)",
                    opt(sb.spp_to_target),
                    opt(sb.projected_render_s)
                )?;
                for a in &sb.adaptive {
                    writeln!(
                        o,
                        "- adaptive on {} (threshold {}, {} spp): mean {} spp, {} stopped early, \
                         {} s against an estimated {} s fixed ({} saved)",
                        a.crop,
                        n(a.variance_threshold),
                        a.spp,
                        n(a.mean_spp),
                        pct(a.early_stopped_share),
                        n(a.time_adaptive_s),
                        n(a.time_fixed_estimate_s),
                        pct(a.time_saved_share)
                    )?;
                }
            }
        }
        writeln!(o)?;

        // -- Picture-changing ------------------------------------------
        writeln!(o, "## Picture-changing settings (measured, not ranked)")?;
        writeln!(o)?;
        let pc = &self.picture_changing;
        match &pc.clamp {
            Some(c) => writeln!(
                o,
                "- indirect clamp {}: removes {} of the image's luminance (mean {} per pixel), \
                 touching {} of the pixels",
                n(c.limit),
                pct(c.removed_luminance_share),
                n(c.mean_removed_luminance),
                pct(c.pixels_affected_share)
            )?,
            None => writeln!(o, "- indirect clamp: off")?,
        }
        let d = &pc.max_depth;
        writeln!(
            o,
            "- max depth {}: {} of paths ended there",
            d.max_depth,
            pct(d.ended_by_depth_share)
        )?;
        if let Some(h) = &d.half_depth {
            writeln!(
                o,
                "- at depth {} on {}: {} faster, mean luminance changes by {}",
                h.depth,
                h.crop,
                pct(h.time_saved_share),
                pct(h.mean_luminance_change)
            )?;
        }
        let sd = &pc.subdivision;
        writeln!(
            o,
            "- subdivision: meshes per level {:?}; the scene holds {} unique triangles in \
             {:.1} MiB of kernel geometry; {}",
            sd.levels,
            sd.triangles,
            sd.mem_bytes as f64 / (1u64 << 20) as f64,
            sd.build_s.map_or_else(
                || "its build time is not recorded".to_owned(),
                |s| format!("built in {} s", n(s))
            )
        )?;
        writeln!(o)?;

        // -- Not tried -------------------------------------------------
        writeln!(o, "## Not tried")?;
        writeln!(o)?;
        if self.not_tried.is_empty() {
            writeln!(o, "Nothing.")?;
        }
        for t in &self.not_tried {
            writeln!(
                o,
                "- {} (tier {}): {}{}",
                t.id,
                t.tier,
                t.reason,
                t.detail
                    .as_ref()
                    .map_or_else(String::new, |d| format!(" — {d}"))
            )?;
        }
        writeln!(o)?;

        // -- Suggestions -----------------------------------------------
        writeln!(o, "## Suggestions")?;
        writeln!(o)?;
        if self.suggestions.is_empty() {
            writeln!(o, "None.")?;
        }
        for s in &self.suggestions {
            writeln!(
                o,
                "- **{}**: flag {}, attribute {}, value `{}`, expected ΔEff {} (evidence: {})",
                s.id,
                s.flag
                    .as_ref()
                    .map_or_else(|| "–".into(), |f| format!("`{f}`")),
                s.usd_attribute
                    .as_ref()
                    .map_or_else(|| "–".into(), |a| format!("`{a}`")),
                s.value,
                n(s.expected_delta_eff),
                s.evidence.join(", ")
            )?;
        }
        writeln!(o)?;

        writeln!(o, "## Converged")?;
        writeln!(o)?;
        writeln!(o, "{}", if self.converged { "Yes." } else { "No." })?;
        writeln!(o)?;

        writeln!(o, "## Suggested command")?;
        writeln!(o)?;
        writeln!(o, "```sh\n{}\n```", self.suggested_command)?;

        if let Some(d) = &self.deltas {
            writeln!(o)?;
            writeln!(o, "## Deltas")?;
            writeln!(o)?;
            if !d.comparable {
                writeln!(o, "{}", or_dash(&d.note))?;
            } else {
                if let Some(c) = &d.baseline_time_s {
                    writeln!(
                        o,
                        "- baseline time {} s → {} s (×{})",
                        n(c.from),
                        n(c.to),
                        n(c.ratio)
                    )?;
                }
                if let Some(c) = &d.baseline_mrse {
                    writeln!(
                        o,
                        "- baseline MRSE {} → {} (×{})",
                        n(c.from),
                        n(c.to),
                        n(c.ratio)
                    )?;
                }
                for s in &d.settings_changed {
                    writeln!(o, "- {}: {} → {}", s.name, s.from, s.to)?;
                }
                let list = |v: &[String]| {
                    if v.is_empty() {
                        "–".to_owned()
                    } else {
                        v.join(", ")
                    }
                };
                writeln!(o, "- findings resolved: {}", list(&d.findings_resolved))?;
                writeln!(o, "- findings new: {}", list(&d.findings_new))?;
                writeln!(
                    o,
                    "- suggestions no longer made: {}",
                    list(&d.suggestions_gone)
                )?;
            }
        }
        Ok(())
    }
}
