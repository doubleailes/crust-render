//! crust-core's modules import only downward (`docs/architecture.md`
//! § Layering): every `crate::` path outside a test module names a module of
//! its own layer or one below, whether it spells the module
//! (`crate::light::LightList`), a crate-root re-export (`crate::LightList`,
//! including the names `pub use material::*` brings), or reaches the root
//! through `super::` (`super::super::tracer` from `light/list.rs`). A new
//! top-level module fails here until it is given a layer.
//!
//! Test-only items (`#[cfg(test)]` modules, inline or declared, and helpers)
//! and comment lines are skipped: a doc link may point upward.

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
    (
        "rays",
        &["aabb", "hittable", "medium", "primvars", "ray", "strands"],
    ),
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
            "precomp",
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

/// Every identifier in `s`, in order.
fn idents(s: &str) -> impl Iterator<Item = &str> {
    s.char_indices().filter_map(move |(i, c)| {
        let starts = is_ident(c) && !s[..i].chars().next_back().is_some_and(is_ident);
        starts.then(|| ident(&s[i..]))
    })
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

/// The public names a module file declares or re-exports: what a glob
/// `pub use module::*` brings to the crate root.
fn public_names(module_src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for stmt in module_src.split(';') {
        if let Some(at) = stmt.find("pub use ") {
            let path = &stmt[at + "pub use ".len()..];
            out.extend(
                idents(path)
                    .skip(1)
                    .filter(|n| !matches!(*n, "crate" | "self" | "super"))
                    .map(str::to_owned),
            );
        }
    }
    for line in module_src.lines() {
        let Some(rest) = line.trim_start().strip_prefix("pub ") else {
            continue;
        };
        for kw in [
            "struct ", "enum ", "trait ", "fn ", "type ", "const ", "static ", "mod ",
        ] {
            if let Some(name) = rest.strip_prefix(kw) {
                out.push(ident(name).to_owned());
            }
        }
    }
    out
}

/// Every name the crate root re-exports, with the module it comes from.
/// `module_src` reads a top-level module's own file, for its glob
/// re-exports.
fn root_exports(
    lib: &str,
    modules: &HashMap<&str, usize>,
    module_src: impl Fn(&str) -> Option<String>,
) -> HashMap<String, String> {
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
        let names: Vec<String> = if rest.trim_end() == "::*" {
            module_src(module)
                .map(|s| public_names(&s))
                .unwrap_or_default()
        } else {
            idents(rest).map(str::to_owned).collect()
        };
        for name in names {
            if !modules.contains_key(name.as_str()) {
                out.insert(name, module.to_owned());
            }
        }
    }
    out
}

/// Code characters of one line at a time, outside string and character
/// literals and line comments: what brace counting must see.
#[derive(Default)]
struct Lexer {
    /// Inside a string literal that `"` and `n` hashes close: `Some(0)` for
    /// an ordinary one, which may span lines.
    in_str: Option<usize>,
    /// The string is raw: a backslash escapes nothing.
    raw: bool,
}

impl Lexer {
    fn code_chars(&mut self, line: &str, mut f: impl FnMut(char)) {
        let b: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < b.len() {
            if let Some(hashes) = self.in_str {
                if !self.raw && b[i] == '\\' {
                    i += 2;
                    continue;
                }
                if b[i] == '"'
                    && b[i + 1..]
                        .iter()
                        .take(hashes)
                        .filter(|&&c| c == '#')
                        .count()
                        == hashes
                {
                    self.in_str = None;
                    i += 1 + hashes;
                    continue;
                }
                i += 1;
                continue;
            }
            match b[i] {
                '/' if b.get(i + 1) == Some(&'/') => return,
                '"' => {
                    self.in_str = Some(0);
                    self.raw = false;
                }
                'r' if !b[..i].last().is_some_and(|&c| is_ident(c))
                    && matches!(b.get(i + 1), Some('"' | '#')) =>
                {
                    let hashes = b[i + 1..].iter().take_while(|&&c| c == '#').count();
                    if b.get(i + 1 + hashes) == Some(&'"') {
                        self.in_str = Some(hashes);
                        self.raw = true;
                        i += 2 + hashes;
                        continue;
                    }
                    f('r');
                }
                // A character literal ('{', '\n'), not a lifetime ('a).
                '\'' if b.get(i + 1) == Some(&'\\') => {
                    i += 2;
                    while i < b.len() && b[i] != '\'' {
                        i += 1;
                    }
                }
                '\'' if b.get(i + 2) == Some(&'\'') => i += 2,
                c => f(c),
            }
            i += 1;
        }
    }
}

/// Where a `#[cfg(test)]` item's scan stands: it ends at its first `;`
/// outside any bracket, or at the brace that closes its body.
#[derive(Default)]
struct Item {
    braces: i32,
    brackets: i32,
    opened: bool,
    ended: bool,
}

impl Item {
    fn feed(&mut self, text: &str, lexer: &mut Lexer) {
        lexer.code_chars(text, |c| match c {
            '{' => {
                self.braces += 1;
                self.opened = true;
            }
            '}' => {
                self.braces -= 1;
                if self.opened && self.braces == 0 {
                    self.ended = true;
                }
            }
            '(' | '[' => self.brackets += 1,
            ')' | ']' => self.brackets -= 1,
            ';' if !self.opened && self.braces == 0 && self.brackets == 0 => self.ended = true,
            _ => {}
        });
    }
}

/// `src` with its test-only items and comment lines blanked, so the line
/// numbers stay the file's. An item under `#[cfg(test)]` ends at its first
/// `;` outside any bracket (`mod tests;`, a `const`) or at the brace that
/// closes its body (`mod tests { … }`, a helper `fn`), wherever it sits in
/// the file.
fn production_code(src: &str) -> String {
    let mut lexer = Lexer::default();
    let mut out = Vec::new();
    let mut lines = src.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let Some(after) = trimmed.strip_prefix("#[cfg(test)]") else {
            let comment = trimmed.starts_with("//") && lexer.in_str.is_none();
            out.push(if comment { "" } else { line });
            lexer.code_chars(line, |_| {});
            continue;
        };
        // Skip the item: the rest of this line, then any further attribute
        // lines and the lines until it ends.
        out.push("");
        let mut item = Item::default();
        item.feed(after, &mut lexer);
        while !item.ended {
            let Some(next) = lines.next() else { break };
            out.push("");
            if !item.opened && item.braces == 0 && next.trim_start().starts_with("#[") {
                continue;
            }
            item.feed(next, &mut lexer);
        }
    }
    out.join("\n")
}

/// The root names the path continuing at `rest` reaches: one, or each head
/// of a `{…}` group.
fn heads(rest: &str) -> Vec<&str> {
    if rest.starts_with('{') {
        group_heads(rest)
    } else {
        vec![ident(rest)]
    }
}

/// The top-level module or crate-root name each path in `code` reaches the
/// root through, with its byte offset: `crate::x`, `crate::{x, …}`, and a
/// `super::` chain long enough to leave the file's module, `depth` deep
/// (`light/list.rs` is 2, `light/mod.rs` and `color.rs` 1).
fn root_paths(code: &str, depth: usize) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    for (at, _) in code.match_indices("crate::") {
        out.extend(
            heads(&code[at + "crate::".len()..])
                .into_iter()
                .map(|n| (at, n)),
        );
    }
    for (at, _) in code.match_indices("super::") {
        // The start of a chain only.
        let before = &code[..at];
        if before.ends_with("::") || before.chars().next_back().is_some_and(is_ident) {
            continue;
        }
        let mut rest = &code[at..];
        let mut ups = 0;
        while let Some(r) = rest.strip_prefix("super::") {
            rest = r;
            ups += 1;
        }
        if ups >= depth {
            out.extend(heads(rest).into_iter().map(|n| (at, n)));
        }
    }
    out.sort_by_key(|&(at, _)| at);
    out
}

/// Each upward import in one file of `module` (`depth` deep), as
/// `(target module, name as written, line)`.
fn upward_imports(
    module: &str,
    depth: usize,
    src: &str,
    modules: &HashMap<&str, usize>,
    root: &HashMap<String, String>,
) -> Vec<(String, String, usize)> {
    let from = modules[module];
    let code = production_code(src);
    let mut out = Vec::new();
    for (at, name) in root_paths(&code, depth) {
        let target = if modules.contains_key(name) {
            Some(name)
        } else {
            root.get(name).map(String::as_str)
        };
        if let Some(to) = target
            && modules[to] > from
        {
            let line = code[..at].matches('\n').count() + 1;
            out.push((to.to_owned(), name.to_owned(), line));
        }
    }
    out
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
    let root = root_exports(&lib, &modules, |m| {
        std::fs::read_to_string(src.join(m).join("mod.rs"))
            .or_else(|_| std::fs::read_to_string(src.join(format!("{m}.rs"))))
            .ok()
    });
    assert!(
        root.get("OpenPBR").is_some_and(|m| m == "material"),
        "the `pub use material::*` glob was not expanded"
    );

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
        if !modules.contains_key(module) {
            unclassified.push(module.to_owned());
            continue;
        }
        // `a/b/mod.rs` is the module `a::b`, `a/b/c.rs` the module `a::b::c`.
        let depth = rel.components().count() - usize::from(rel.ends_with("mod.rs"));
        let text = std::fs::read_to_string(&file).expect("read");
        for (to, name, line) in upward_imports(module, depth, &text, &modules, &root) {
            upward
                .entry(format!("{module} -> {to} ({name})"))
                .or_default()
                .push(format!("{}:{line}", rel.display()));
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

/// The scanner itself, on small sources: each way of reaching a higher
/// layer is caught, and test-only code is skipped wherever it sits.
mod scanner {
    use super::*;

    fn root() -> HashMap<String, String> {
        let lib = "pub use light::{LightList, EVERY_CLASS};\npub use material::*;\n";
        root_exports(lib, &layer_of(), |m| {
            (m == "material").then(|| {
                "pub mod closure;\npub use openpbr::{OpenPBR, ResolvedOpenPBR};\npub struct Probe;\n"
                    .to_owned()
            })
        })
    }

    /// The upward targets found in `src`, as file `module` at `depth`.
    fn found(module: &str, depth: usize, src: &str) -> Vec<(String, usize)> {
        upward_imports(module, depth, src, &layer_of(), &root())
            .into_iter()
            .map(|(to, _, line)| (to, line))
            .collect()
    }

    #[test]
    fn module_paths_and_root_re_exports_are_caught() {
        let src = "use crate::tracer::RenderSettings;\nfn f(_: &crate::LightList) {}\n";
        assert_eq!(
            found("color", 1, src),
            [("tracer".into(), 1), ("light".into(), 2)]
        );
        let group = "use crate::{region::PixelRect, LightList};\n";
        assert_eq!(found("stats", 1, group), [("light".into(), 1)]);
        // Downward and sideways are fine.
        assert!(found("tracer", 1, "use crate::{color, LightList, stats};\n").is_empty());
    }

    #[test]
    fn names_from_a_root_glob_are_caught() {
        for name in ["OpenPBR", "ResolvedOpenPBR", "Probe", "closure"] {
            let src = format!("use crate::{name};\n");
            assert_eq!(found("ray", 1, &src), [("material".into(), 1)], "{name}");
        }
    }

    #[test]
    fn super_chains_that_leave_the_module_are_caught() {
        // `light/list.rs`: two `super`s reach the root, one stays in `light`.
        assert_eq!(
            found("light", 2, "use super::super::tracer::X;\n"),
            [("tracer".into(), 1)]
        );
        assert!(found("light", 2, "use super::learned::LightCache;\n").is_empty());
        // `color.rs`: one `super` is the root.
        assert_eq!(
            found("color", 1, "use super::{LightList, pdf};\n"),
            [("light".into(), 1)]
        );
        // A chain is read once, from its start.
        assert_eq!(
            found("light", 3, "use super::super::super::tracer::X;\n").len(),
            1
        );
    }

    #[test]
    fn code_after_a_test_module_declaration_is_still_checked() {
        let src = "#[cfg(test)]\nmod tests;\n\nuse crate::tracer::Renderer;\n";
        assert_eq!(found("color", 1, src), [("tracer".into(), 4)]);
    }

    #[test]
    fn inline_test_items_are_skipped_wherever_they_sit() {
        let src = "\
#[cfg(test)]
mod tests {
    use crate::tracer::Renderer;
    const OPEN: char = '{';
    const S: &str = \"}}} {\";
    const R: &str = r#\"}\"#;
}
#[cfg(test)]
fn helper(m: &[[f32; 4]; 4]) -> crate::Scene {
    todo!()
}
#[cfg(test)]
pub(super) const K: i32 = crate::tracer::K;
// crate::tracer in a comment
use crate::tracer::Renderer;
";
        assert_eq!(found("color", 1, src), [("tracer".into(), 15)]);
    }
}
