//! The override layer (design D1, D5): the file that *is* the session. Its
//! only composition arc is a sublayer on the input; every edit is an opinion
//! in it; it is saved after every edit batch.

use openusd::sdf;
use std::path::{Component, Path, PathBuf};

/// `path` made absolute against the working directory, without resolving
/// links (`std::fs::canonicalize` would, and on Windows answer a `\\?\`
/// path no USD tool writes).
pub fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether two paths name the same file: equal once canonicalized, or,
/// when either does not exist, equal as absolute paths.
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => std::path::absolute(a).ok() == std::path::absolute(b).ok(),
    }
}

/// One path component as it compares on this platform: Windows paths are
/// case-insensitive.
fn key(c: Component<'_>) -> String {
    let s = c.as_os_str().to_string_lossy();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.into_owned()
    }
}

/// How `target` is written in a layer in `dir`: relative to `dir`, as
/// `./a/b.usda` or `../a/b.usda` with forward slashes, when both are on the
/// same volume; absolute otherwise. Both are absolute.
pub fn anchored(target: &Path, dir: &Path) -> String {
    let t: Vec<Component> = target.components().collect();
    let d: Vec<Component> = dir.components().collect();
    // A different drive or UNC share: no relative path reaches it.
    let root = |p: &[Component]| {
        p.iter()
            .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
            .map(|&c| key(c))
            .collect::<Vec<_>>()
    };
    if root(&t) != root(&d) {
        return target.to_string_lossy().replace('\\', "/");
    }
    let common = t
        .iter()
        .zip(&d)
        .take_while(|(a, b)| key(**a) == key(**b))
        .count();
    let mut parts: Vec<String> = Vec::new();
    for _ in common..d.len() {
        parts.push("..".into());
    }
    for c in &t[common..] {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    let joined = parts.join("/");
    if joined.starts_with("..") {
        joined
    } else {
        format!("./{joined}")
    }
}

/// The stage metadata a session's layer copies from its input's root layer,
/// as that layer authors it: the units and the up axis. A stage reads its
/// metadata from its root layer only, and a session's root layer is the
/// override layer, so without the copy the session would read the fallbacks
/// (a centimetre, Y up) whatever the input says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StageMetrics {
    pub meters_per_unit: Option<f64>,
    pub up_axis: Option<String>,
}

impl StageMetrics {
    /// What the root layer at `path` authors; nothing when it cannot be read
    /// (the import reports that).
    pub fn of_layer(path: &Path) -> StageMetrics {
        let Some(layer) = path.to_str().and_then(|p| sdf::Layer::open(p).ok()) else {
            return StageMetrics::default();
        };
        let field = |name: &str| {
            layer
                .data()
                .try_field(&sdf::Path::abs_root(), name)
                .ok()
                .flatten()
        };
        let meters_per_unit = field("metersPerUnit").and_then(|v| match v.as_ref() {
            sdf::Value::Double(d) => Some(*d),
            sdf::Value::Float(f) => Some(f64::from(*f)),
            _ => None,
        });
        let up_axis = field("upAxis").and_then(|v| match v.as_ref() {
            sdf::Value::Token(t) => Some(t.as_str().to_owned()),
            sdf::Value::String(s) => Some(s.clone()),
            _ => None,
        });
        StageMetrics {
            meters_per_unit,
            up_axis,
        }
    }

    /// The fields of `self` that `layer` does not author: what a resumed
    /// session adds to its layer.
    pub fn missing_from(&self, layer: &StageMetrics) -> StageMetrics {
        StageMetrics {
            meters_per_unit: self
                .meters_per_unit
                .filter(|_| layer.meters_per_unit.is_none()),
            up_axis: self.up_axis.clone().filter(|_| layer.up_axis.is_none()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.meters_per_unit.is_none() && self.up_axis.is_none()
    }
}

/// A double as usda writes it: `inf`, `-inf` and `nan` by name.
fn usda_double(d: f64) -> String {
    if d.is_nan() {
        "nan".to_owned()
    } else if d.is_infinite() {
        if d > 0.0 { "inf" } else { "-inf" }.to_owned()
    } else {
        format!("{d}")
    }
}

/// The text of a new override layer in `dir` on `input`: a sublayer, the
/// input's `metrics`, and no opinion.
pub fn new_layer_text(input: &Path, dir: &Path, metrics: &StageMetrics) -> String {
    let mut text = String::from("#usda 1.0\n(\n");
    if let Some(m) = metrics.meters_per_unit {
        text.push_str(&format!("    metersPerUnit = {}\n", usda_double(m)));
    }
    text.push_str(&format!(
        "    subLayers = [\n        @{}@\n    ]\n",
        anchored(input, dir)
    ));
    if let Some(axis) = &metrics.up_axis {
        text.push_str(&format!("    upAxis = {axis:?}\n"));
    }
    text.push_str(")\n\n");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("crust_mcp_layer_tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(name);
        std::fs::write(&path, text).expect("write");
        path
    }

    /// A new layer carries what the input authors of its units and up axis,
    /// beside its sublayer, and only that.
    #[test]
    fn a_new_layer_copies_the_inputs_metrics() {
        let input = written(
            "metrics_both.usda",
            "#usda 1.0\n(\n    metersPerUnit = 1\n    upAxis = \"Z\"\n)\n",
        );
        let metrics = StageMetrics::of_layer(&input);
        assert_eq!(
            metrics,
            StageMetrics {
                meters_per_unit: Some(1.0),
                up_axis: Some("Z".into()),
            }
        );
        let dir = input.parent().unwrap();
        let layer = written(
            "metrics_both_layer.usda",
            &new_layer_text(&input, dir, &metrics),
        );
        assert_eq!(StageMetrics::of_layer(&layer), metrics);
        let opened = sdf::Layer::open(layer.to_str().unwrap()).expect("the layer parses");
        let sublayers = opened
            .data()
            .try_field(&sdf::Path::abs_root(), "subLayers")
            .unwrap()
            .expect("subLayers");
        assert!(format!("{sublayers:?}").contains("metrics_both.usda"));

        // An input authoring neither: the layer as it always was.
        let bare = written("metrics_none.usda", "#usda 1.0\n");
        let metrics = StageMetrics::of_layer(&bare);
        assert!(metrics.is_empty());
        assert_eq!(
            new_layer_text(&bare, dir, &metrics),
            "#usda 1.0\n(\n    subLayers = [\n        @./metrics_none.usda@\n    ]\n)\n\n"
        );
    }

    #[test]
    fn anchored_paths_are_relative_on_one_volume() {
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        let p = |s: &str| PathBuf::from(format!("{root}{s}"));
        assert_eq!(anchored(&p("w/shot.usda"), &p("w")), "./shot.usda");
        assert_eq!(
            anchored(&p("shots/a/shot.usda"), &p("work")),
            "../shots/a/shot.usda"
        );
        assert_eq!(anchored(&p("w/a/b.usda"), &p("w")), "./a/b.usda");
        assert_eq!(anchored(&p("x.usda"), &p("a/b")), "../../x.usda");
    }

    #[cfg(windows)]
    #[test]
    fn another_drive_stays_absolute() {
        assert_eq!(
            anchored(Path::new("D:\\shots\\a.usda"), Path::new("C:\\work")),
            "D:/shots/a.usda"
        );
        assert_eq!(
            anchored(Path::new("c:\\Shots\\a.usda"), Path::new("C:\\shots")),
            "./a.usda"
        );
    }
}
