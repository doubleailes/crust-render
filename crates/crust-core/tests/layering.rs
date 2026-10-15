//! crust-core's modules import only downward (`docs/architecture.md`
//! § Layering): every `crate::` path outside a test module names a module of
//! its own layer or one below, whether it spells the module
//! (`crate::light::LightList`) or a crate-root re-export (`crate::LightList`).
//! A new top-level module fails here until it is given a layer.
//!
//! Comment lines are skipped: a doc link may point upward.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Lowest first. A module may import from its own layer and the ones before.
const LAYERS: &[(&str, &[&str])] = &[
    (
        "foundation",
        &[
            "buffer",
            "color",
            "config",
            "distribution",
            "error",
            "lux",
            "names",
            "pdf",
            "profile",
            "region",
            "report",
            "stats",
            "texture",
            "warnings",
        ],
    ),
    ("rays", &["aabb", "hittable", "medium", "ray"]),
    ("materials", &["lpe", "material"]),
    ("world", &["rt_world"]),
    ("lights", &["environment", "light"]),
    (
        "integrator",
        &[
            "aov",
            "camera",
            "filter",
            "guiding",
            "sampler",
            "subsurface",
            "tracer",
            "volume",
        ],
    ),
    ("import", &["live", "place", "scene", "world"]),
    (
        "tools",
        &["check", "compare", "diagnostic", "inspect", "stamp"],
    ),
];

fn layer_of() -> HashMap<&'static str, usize> {
    LAYERS
        .iter()
        .enumerate()
        .flat_map(|(i, (_, modules))| modules.iter().map(move |m| (*m, i)))
        .collect()
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The identifier at the start of `s`.
fn ident(s: &str) -> &str {
    &s[..s.find(|c| !is_ident(c)).unwrap_or(s.len())]
}

/// The first path segment of each item of the `{…}` group `s` opens: what
/// `crate::{a::B, C}` names at the crate root (`a`, `C`).
fn group_heads(s: &str) -> Vec<&str> {
    let mut heads = Vec::new();
    let mut depth = 0usize;
    let mut expect = true;
    for (i, c) in s.char_indices() {
        match c {
            '{' => {
                depth += 1;
                expect = depth == 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            ',' if depth == 1 => expect = true,
            c if expect && depth == 1 && is_ident(c) => {
                heads.push(ident(&s[i..]));
                expect = false;
            }
            _ => {}
        }
    }
    heads
}

/// Every name the crate root re-exports, with the module it comes from.
fn root_exports(lib: &str, modules: &HashMap<&str, usize>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for stmt in lib.split(';') {
        let Some(at) = stmt.find("pub use ") else {
            continue;
        };
        let path = &stmt[at + "pub use ".len()..];
        let module = ident(path);
        if !modules.contains_key(module) {
            continue;
        }
        let rest = &path[module.len()..];
        for (i, c) in rest.char_indices() {
            let starts = is_ident(c) && (i == 0 || !is_ident(rest[..i].chars().last().unwrap()));
            if starts {
                let name = ident(&rest[i..]);
                if !modules.contains_key(name) {
                    out.insert(name.to_owned(), module.to_owned());
                }
            }
        }
    }
    out
}

/// `src` without its test modules, comment lines blanked so the line
/// numbers stay the file's.
fn production_code(src: &str) -> String {
    let src = match src.find("\n#[cfg(test)]\nmod ") {
        Some(k) => &src[..k],
        None => src,
    };
    src.lines()
        .map(|l| {
            if l.trim_start().starts_with("//") {
                ""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn walk(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            walk(&path, files);
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn modules_import_only_downward() {
    let modules = layer_of();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let lib = std::fs::read_to_string(src.join("lib.rs")).expect("lib.rs");
    let root = root_exports(&lib, &modules);

    let mut files = Vec::new();
    walk(&src, &mut files);
    let mut unclassified = Vec::new();
    let mut upward: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in files {
        let rel = file.strip_prefix(&src).expect("under src");
        let top = rel.components().next().expect("a component");
        let top = top.as_os_str().to_str().expect("utf-8 path");
        let module = top.strip_suffix(".rs").unwrap_or(top);
        let is_test = rel.components().any(|c| c.as_os_str() == "tests")
            || rel.file_name().is_some_and(|f| f == "tests.rs");
        if module == "lib" || is_test {
            continue;
        }
        let Some(&from) = modules.get(module) else {
            unclassified.push(module.to_owned());
            continue;
        };
        let code = production_code(&std::fs::read_to_string(&file).expect("read"));
        for (at, _) in code.match_indices("crate::") {
            let rest = &code[at + "crate::".len()..];
            let names = if rest.starts_with('{') {
                group_heads(rest)
            } else {
                vec![ident(rest)]
            };
            for name in names {
                let target = if modules.contains_key(name) {
                    Some(name)
                } else {
                    root.get(name).map(String::as_str)
                };
                if let Some(to) = target
                    && modules[to] > from
                {
                    let line = code[..at].lines().count();
                    upward
                        .entry(format!("{module} -> {to} ({name})"))
                        .or_default()
                        .push(format!("{}:{line}", rel.display()));
                }
            }
        }
    }
    unclassified.sort();
    unclassified.dedup();
    assert!(
        unclassified.is_empty(),
        "give these top-level modules a layer in LAYERS: {unclassified:?}"
    );
    assert!(
        upward.is_empty(),
        "imports from a higher layer (move the item down, or the importer up):\n{}",
        upward
            .iter()
            .map(|(k, v)| format!("  {k}: {}", v.join(", ")))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
