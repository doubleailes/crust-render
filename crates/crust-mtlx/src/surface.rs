//! MaterialX surface-shader nodes expanded into the closure tree of their
//! implementation nodegraphs.
use crate::bsdf::Closures;
use crate::eval::Compiler;
use crate::parse::Node;

/// Expands `node` — an `open_pbr_surface`, `standard_surface` or `gltf_pbr` —
/// into `out`.
pub fn build(c: &mut Compiler<'_>, node: &Node, out: &mut Closures) {
    let _ = out;
    c.unsupported.insert(node.category.clone());
}
