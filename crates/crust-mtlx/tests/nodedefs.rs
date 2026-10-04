//! The surface builders' default tables must be MaterialX's own nodedefs.
//!
//! The nodedefs are vendored under `tests/nodedefs/` (MaterialX 1.39,
//! Apache-2.0). A table drifting from them — a transcription slip, a MaterialX
//! update not carried over — would make every document that leaves an input
//! unauthored shade with the wrong value, and render plausibly.

use crust_mtlx::hair::{
    CHIANG_HAIR_ABSORPTION_FROM_COLOR, CHIANG_HAIR_BSDF, CHIANG_HAIR_ROUGHNESS,
    DEON_HAIR_ABSORPTION_FROM_MELANIN,
};
use crust_mtlx::surface::{GLTF_PBR, InputDef, OPEN_PBR_SURFACE, STANDARD_SURFACE};
use crust_mtlx::value::parse_literal;
use std::path::Path;

/// `(name, type, value)` of every input of `nodedef` in `file`, with an
/// `inherit`ed nodedef's inputs first and the inheriting one's overriding.
fn inputs(file: &str, nodedef: &str) -> Vec<(String, String, Option<String>)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/nodedefs")
        .join(file);
    let text = std::fs::read_to_string(&path).expect("vendored nodedef");
    let doc = roxmltree::Document::parse(&text).expect("well-formed");
    let find = |name: &str| {
        doc.descendants()
            .find(|n| n.has_tag_name("nodedef") && n.attribute("name") == Some(name))
            .unwrap_or_else(|| panic!("{name} in {file}"))
    };
    let nd = find(nodedef);
    let mut out: Vec<(String, String, Option<String>)> = match nd.attribute("inherit") {
        Some(parent) => inputs(file, parent),
        None => Vec::new(),
    };
    for i in nd.children().filter(|c| c.has_tag_name("input")) {
        let name = i.attribute("name").unwrap().to_string();
        let ty = i.attribute("type").unwrap().to_string();
        let value = i.attribute("value").map(str::to_string);
        match out.iter_mut().find(|(n, _, _)| *n == name) {
            Some(slot) => *slot = (name, ty, value),
            None => out.push((name, ty, value)),
        }
    }
    out
}

fn check(table: &[InputDef], file: &str, nodedef: &str) {
    let expected = inputs(file, nodedef);
    let names: Vec<&str> = table.iter().map(|d| d.name).collect();
    let want: Vec<&str> = expected.iter().map(|(n, _, _)| n.as_str()).collect();
    assert_eq!(names, want, "{nodedef}: the inputs, in order");
    for (d, (name, ty, value)) in table.iter().zip(&expected) {
        assert_eq!(d.ty, ty, "{name}'s type");
        let parsed = value.as_deref().and_then(|v| parse_literal(v, ty));
        match (d.default, parsed) {
            (None, None) => {}
            (Some(a), Some(b)) => {
                let lanes = (b.arity as usize).max(a.arity as usize).clamp(1, 4);
                let (a, b) = (a.with_arity(lanes as u8), b.with_arity(lanes as u8));
                assert_eq!(a.v[..lanes], b.v[..lanes], "{name}'s default");
            }
            (a, b) => panic!("{name}: table {a:?}, nodedef {b:?}"),
        }
    }
}

#[test]
fn nodedef_tables_match_materialx() {
    check(
        OPEN_PBR_SURFACE,
        "ND_open_pbr_surface_surfaceshader.mtlx",
        "ND_open_pbr_surface_surfaceshader",
    );
    check(
        STANDARD_SURFACE,
        "ND_standard_surface_surfaceshader.mtlx",
        "ND_standard_surface_surfaceshader",
    );
    check(
        GLTF_PBR,
        "ND_gltf_pbr_surfaceshader.mtlx",
        "ND_gltf_pbr_surfaceshader",
    );
}

#[test]
fn hair_nodedef_tables_match_materialx() {
    let file = "ND_chiang_hair.mtlx";
    check(CHIANG_HAIR_BSDF, file, "ND_chiang_hair_bsdf");
    check(CHIANG_HAIR_ROUGHNESS, file, "ND_chiang_hair_roughness");
    check(
        CHIANG_HAIR_ABSORPTION_FROM_COLOR,
        file,
        "ND_chiang_hair_absorption_from_color",
    );
    check(
        DEON_HAIR_ABSORPTION_FROM_MELANIN,
        file,
        "ND_deon_hair_absorption_from_melanin",
    );
}

#[test]
fn standard_surface_1_0_1_overrides_its_parent() {
    // 1.0.1 inherits 1.0.0 and changes exactly these two defaults.
    let base = STANDARD_SURFACE.iter().find(|d| d.name == "base").unwrap();
    let color = STANDARD_SURFACE
        .iter()
        .find(|d| d.name == "base_color")
        .unwrap();
    assert_eq!(base.default.unwrap().x(), 1.0);
    assert_eq!(color.default.unwrap().v[..3], [0.8, 0.8, 0.8]);
}
