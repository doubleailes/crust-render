//! MaterialX `.mtlx` look-dev graphs, read directly and compiled for shading.
//!
//! USD's own answer to MaterialX is a file-format plugin that composes a
//! `.mtlx` into the stage as `UsdShade` prims. The pure-Rust `openusd` crate
//! ships no such plugin, so a `Material` whose only opinion is
//! `references = @foo.mtlx@</MaterialX/Materials/name>` composes **empty**.
//! This crate is what reads the document instead. It is renderer-agnostic —
//! its only dependencies are an XML parser and `glam` — and it knows nothing
//! about any particular material model; that is the consumer's half.
//!
//! The pipeline is three modules:
//!
//! - [`parse`] — XML → a flat, name-addressable node graph.
//! - [`eval`] — that graph compiled once into a slot-indexed [`Program`],
//!   evaluated per shading point with no name lookups and no allocation.
//! - [`bsdf`] — the BSDF half of the graph (`layer`/`mix` over standalone BSDF
//!   nodes) flattened into weighted [`Lobe`]s, which a renderer then pools onto
//!   its own material.
//!
//! [`compile`] runs all three for one material node. The crate decodes *no
//! pixels*: an `image` node's file is handed to the caller's
//! [`TextureLoader`], which returns a [`Texture`] sampler or declines.
//! `forbid(unsafe_code)`: no `unsafe` here, and `roxmltree` was chosen over a
//! streaming parser partly to keep it that way.
#![forbid(unsafe_code)]

pub mod bsdf;
pub mod eval;
pub mod parse;
mod texture;
pub mod value;

pub use bsdf::{Emission, Flattened, Lobe, LobeKind, flatten};
pub use eval::{
    BinOp, Compiler, Op, Program, ShadeCtx, UnOp, perturb_normal, reflectivity_from_ior,
};
pub use parse::{Doc, Input, MtlxError, Node, Source};
pub use texture::{Texture, TextureRef};
pub use value::Val;

/// Resolves an `image` node's `file` input — as authored, relative to the
/// document — plus its `colorspace` attribute, into a sampler. `None` declines
/// the texture and the node falls back to its `default` input.
pub type TextureLoader<'a> = &'a dyn Fn(&str, Option<&str>) -> Option<TextureRef>;

/// One material node, compiled.
pub struct Compiled {
    /// The pattern program every lobe's parameters are computed by.
    pub program: Program,
    /// The BSDF tree, flattened. Slots index into `program`'s output.
    pub lobes: Vec<Lobe>,
    /// The EDF tree, flattened. Empty for a material that authors no `edf`,
    /// which is the overwhelming majority — a consumer can key a "do not
    /// evaluate the graph at all" fast path on that, and crust does.
    pub emission: Vec<Emission>,
    /// The material node's own `name`.
    pub root_name: String,
    /// Node categories the compiler had no operator for, sorted, for one
    /// warning per material instead of one per node. Those inputs fell back
    /// to a constant; the material still built.
    pub unsupported: Vec<String>,
    /// How many `image` nodes resolved to a real texture. A material whose
    /// every texture was declined still renders — on its constant inputs —
    /// but the difference between "no textures authored" and "no textures
    /// found" is worth surfacing.
    pub textures: usize,
}

impl Compiled {
    /// Every program slot a consumer reads: each lobe's and each emitter's.
    pub fn roots(&self) -> Vec<u32> {
        let mut roots = Vec::new();
        for l in &self.lobes {
            roots.extend([l.weight, l.color, l.roughness, l.ior, l.extinction]);
            roots.extend(l.normal);
        }
        for e in &self.emission {
            roots.extend([e.color, e.weight]);
        }
        roots
    }

    /// Replaces the program with [`Program::optimize`]'s and points every
    /// lobe and emitter at its slot's new home. Every root keeps its value
    /// bit for bit at every shading point; only the work to reach it shrinks.
    pub fn optimize(&mut self) {
        let (program, remap) = self.program.optimize(&self.roots());
        // Every root is live, so every root was placed.
        let at = |s: &mut u32| *s = remap[*s as usize].expect("a root slot survives optimization");
        for l in &mut self.lobes {
            at(&mut l.weight);
            at(&mut l.color);
            at(&mut l.roughness);
            at(&mut l.ior);
            at(&mut l.extinction);
            if let Some(n) = &mut l.normal {
                at(n);
            }
        }
        for e in &mut self.emission {
            at(&mut e.color);
            at(&mut e.weight);
        }
        self.program = program;
    }
}

/// Parses a `.mtlx` and compiles the named material node.
///
/// `material_node` is the `name` of the `surfacematerial` (or `surface`) node
/// to start from. When `None`, the first `surfacematerial` in the document is
/// used, which is what a single-material document means.
pub fn compile(
    path: &std::path::Path,
    material_node: Option<&str>,
    load_texture: TextureLoader<'_>,
) -> Result<Compiled, MtlxError> {
    let doc = Doc::open(path)?;
    let root = match material_node {
        Some(n) => doc
            .find("", n)
            .cloned()
            .ok_or_else(|| MtlxError::NoSuchMaterial(n.to_string()))?,
        None => doc
            .by_category("surfacematerial")
            .next()
            .or_else(|| doc.by_category("surface").next())
            .cloned()
            .ok_or_else(|| MtlxError::NoSuchMaterial("<any surfacematerial>".into()))?,
    };

    let mut c = Compiler::new(&doc, load_texture);
    let one = c.constant(Val::ONE);
    let mut flat = Flattened::default();
    flatten(&mut c, &root, one, 0, &mut flat);
    // Counted off the compiled program rather than inside the loader
    // closure: the compiler memoises, so a texture feeding three nodes is
    // loaded once, and the program is the record of what actually resolved.
    let textures = c
        .program
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Texture { tex: Some(_), .. }))
        .count();
    let unsupported: Vec<String> = c.unsupported.iter().cloned().collect();

    Ok(Compiled {
        program: c.program,
        lobes: flat.lobes,
        emission: flat.emission,
        root_name: root.name.clone(),
        unsupported,
        textures,
    })
}
