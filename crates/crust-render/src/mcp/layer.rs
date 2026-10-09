//! The override layer (design D1, D5): the file that *is* the session. Its
//! only composition arc is a sublayer on the input; every edit is an opinion
//! in it; it is saved after every edit batch.

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

/// The text of a new override layer in `dir` on `input`: a sublayer and no
/// opinion.
pub fn new_layer_text(input: &Path, dir: &Path) -> String {
    format!(
        "#usda 1.0\n(\n    subLayers = [\n        @{}@\n    ]\n)\n\n",
        anchored(input, dir)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
