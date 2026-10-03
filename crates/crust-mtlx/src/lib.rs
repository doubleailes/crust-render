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
//! The pipeline is three private modules, their items re-exported here:
//!
//! - `parse` — XML → a flat, name-addressable node graph ([`Doc`]).
//! - `eval` — that graph compiled once into a slot-indexed [`Program`],
//!   evaluated per shading point with no name lookups and no allocation.
//! - `bsdf` — the closure half of the graph read as the tree MaterialX
//!   defines: BSDF leaves combined by `layer` / `mix` / `add` / `multiply`
//!   ([`Closures`]), with the EDF terms and the interior volume beside it.
//!   The three surface-shader nodes (`open_pbr_surface`, `standard_surface`,
//!   `gltf_pbr`) expand into the tree of their MaterialX nodegraphs
//!   (`surface`).
//!
//! [`compile`] runs all three for one material node. The crate decodes *no
//! pixels*: an `image` node's file is handed to the caller's
//! [`TextureLoader`], which returns a [`Texture`] sampler or declines.
//! `forbid(unsafe_code)`: no `unsafe` here, and `roxmltree` was chosen over a
//! streaming parser partly to keep it that way.
#![forbid(unsafe_code)]

mod bsdf;
mod eval;
mod parse;
mod surface;
mod texture;
mod value;

pub use bsdf::{
    Bsdf, Closure, Closures, DiffuseModel, Emission, Leaf, NodeId, ScatterMode, SheenMode, Slot,
    ThinFilm, Volume, flatten,
};
pub use eval::{
    BinOp, Compiler, Op, Program, ShadeCtx, UnOp, perturb_normal, reflectivity_from_ior,
};
pub use parse::{Doc, Input, MtlxError, Node, Source};
pub use surface::{GLTF_PBR, InputDef, OPEN_PBR_SURFACE, STANDARD_SURFACE};
pub use texture::{Texture, TextureRef};
pub use value::{Val, arity_of, parse_literal};

/// Resolves an `image` node's `file` input — as authored, relative to the
/// document — plus its `colorspace` attribute, into a sampler. `None` declines
/// the texture and the node falls back to its `default` input.
pub type TextureLoader<'a> = &'a dyn Fn(&str, Option<&str>) -> Option<TextureRef>;

/// One material node, compiled.
pub struct Compiled {
    /// The pattern program every closure parameter is computed by.
    pub program: Program,
    /// The closure tree, EDF terms and volume. Slots index into `program`'s
    /// output.
    pub closures: Closures,
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
    /// Every program slot a consumer reads.
    pub fn roots(&self) -> Vec<u32> {
        // `for_each_slot` visits mutably; walk a copy, the tree is small.
        slots_of(&mut self.closures.clone())
    }

    /// Replaces the program with [`Program::optimize`]'s and points every
    /// closure parameter at its slot's new home. Every root keeps its value
    /// bit for bit at every shading point; only the work to reach it shrinks.
    pub fn optimize(&mut self) {
        let roots = slots_of(&mut self.closures);
        let (program, remap) = self.program.optimize(&roots);
        // Every root is live, so every root was placed.
        self.closures.for_each_slot(|s| {
            *s = remap[*s as usize].expect("a root slot survives optimization");
        });
        self.program = program;
    }
}

/// Every slot `closures` reads, in [`Closures::for_each_slot`]'s order. The
/// visitor takes `&mut` but this walk changes nothing.
fn slots_of(closures: &mut Closures) -> Vec<u32> {
    let mut slots = Vec::new();
    closures.for_each_slot(|s| slots.push(*s));
    slots
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
    let mut closures = Closures::default();
    flatten(&mut c, &root, &mut closures);
    // Counted off the compiled program rather than inside the loader
    // closure: the compiler memoises, so a texture feeding three nodes is
    // loaded once, and the program is the record of what actually resolved.
    // Distinct samplers, not lookups: `heighttonormal` reads one image at
    // four offsets through four ops sharing one handle.
    let textures = c
        .program
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Texture { tex: Some(t), .. } => Some(std::sync::Arc::as_ptr(&t.0).cast::<()>()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>()
        .len();
    let unsupported: Vec<String> = c.unsupported.iter().cloned().collect();

    Ok(Compiled {
        program: c.program,
        closures,
        root_name: root.name.clone(),
        unsupported,
        textures,
    })
}
