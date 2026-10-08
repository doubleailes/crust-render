//! `--baseline PREV.json` (design D12): what changed since a previous
//! report, so an agent can see whether its last action did what that
//! report predicted.

use super::report::{Change, Deltas, FORMAT, Report, SettingChange};

/// The deltas of `current` against the previous report's JSON text. A
/// report of another format, scene path, frame, camera, resolution or
/// region is refused as not comparable, with no deltas.
pub fn deltas(previous_json: &str, current: &Report) -> Deltas {
    let refuse = |why: String| Deltas {
        comparable: false,
        note: Some(format!("not comparable: {why}")),
        ..Deltas::default()
    };
    let value: serde_json::Value = match serde_json::from_str(previous_json) {
        Ok(v) => v,
        Err(e) => return refuse(format!("the baseline is not JSON ({e})")),
    };
    match value.get("format").and_then(|f| f.as_str()) {
        Some(f) if f == FORMAT => {}
        Some(f) => return refuse(format!("format {f}, not {FORMAT}")),
        None => return refuse("the baseline has no format".into()),
    }
    let prev: Report = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => return refuse(format!("the baseline is not a {FORMAT} report ({e})")),
    };
    let (a, b) = (&prev.scene, &current.scene);
    for (what, same) in [
        ("scene path", a.path == b.path),
        ("frame", a.frame == b.frame),
        ("camera", a.camera == b.camera),
        ("resolution", a.resolution == b.resolution),
        ("region", a.region == b.region),
    ] {
        if !same {
            return refuse(format!("a different {what}"));
        }
    }
    let change = |from: f64, to: f64| Change {
        from: from.into(),
        to: to.into(),
        ratio: (to / from).into(),
    };
    let settings_changed = current
        .effective_settings
        .iter()
        .filter_map(|s| {
            let before = prev.effective_settings.iter().find(|p| p.name == s.name);
            match before {
                Some(p) if p.value == s.value => None,
                _ => Some(SettingChange {
                    name: s.name.clone(),
                    from: before.map_or_else(|| "(absent)".to_owned(), |p| p.value.clone()),
                    to: s.value.clone(),
                }),
            }
        })
        .collect();
    let ids = |r: &Report| {
        r.static_findings
            .iter()
            .map(|f| f.id.clone())
            .collect::<Vec<_>>()
    };
    let (before, now) = (ids(&prev), ids(current));
    Deltas {
        comparable: true,
        note: None,
        baseline_time_s: Some(change(prev.baseline.time_s.0, current.baseline.time_s.0)),
        baseline_mrse: Some(change(prev.baseline.mrse.0, current.baseline.mrse.0)),
        settings_changed,
        findings_resolved: before
            .iter()
            .filter(|f| !now.contains(f))
            .cloned()
            .collect(),
        findings_new: now
            .iter()
            .filter(|f| !before.contains(f))
            .cloned()
            .collect(),
        suggestions_gone: prev
            .suggestions
            .iter()
            .filter(|s| !current.suggestions.iter().any(|c| c.id == s.id))
            .map(|s| s.id.clone())
            .collect(),
    }
}
