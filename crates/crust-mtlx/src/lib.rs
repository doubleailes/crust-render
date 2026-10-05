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
//! pixels* and knows no colour space: both are the [`Host`]'s. An `image`
//! node's file is handed to its [`TextureLoader`], which returns a
//! [`Texture`] sampler or declines, and every authored colour literal goes
//! through its [`ColorConverter`] once, at compile time.
//!
//! **Colour spaces** follow the MaterialX specification. A `colorspace`
//! attribute may sit on the document, a nodegraph, a node or an input, and an
//! input's *effective* space is the nearest of those ([`Doc::colorspace_of`]).
//! It applies only to colour: an authored `color3` / `color4` literal is
//! converted from its effective space into the host's working space (RGB
//! only, alpha untouched), and an `image` / `tiledimage` whose output is a
//! colour hands its `file`'s effective space to the loader. A `float` or
//! `vector*` value — a roughness map, a normal map, a literal direction — is
//! never managed, and a non-colour image's file is passed with `None`, which
//! tells the host it is data. Nodedef defaults (unauthored inputs) are taken
//! as already in the working space, as is a colour no scope declares a space
//! for.
//! `forbid(unsafe_code)`: no `unsafe` here, and `roxmltree` was chosen over a
//! streaming parser partly to keep it that way.
#![forbid(unsafe_code)]

// Private modules: every public item has exactly one path, at the crate root.
mod bsdf;
mod eval;
mod hair;
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
/// The nodedef input tables the hair nodes and the surface-shader expansions
/// are built from, transcribed from MaterialX's own `stdlib` / `pbrlib`
/// definitions (pinned against them by `tests/nodedefs.rs`).
pub use hair::{
    CHIANG_HAIR_ABSORPTION_FROM_COLOR, CHIANG_HAIR_BSDF, CHIANG_HAIR_ROUGHNESS,
    DEON_HAIR_ABSORPTION_FROM_MELANIN,
};
pub use parse::{Doc, Input, MtlxError, Node, Source};
pub use surface::{GLTF_PBR, InputDef, OPEN_PBR_SURFACE, STANDARD_SURFACE};
pub use texture::{Texture, TextureRef};
pub use value::{Val, arity_of, parse_literal};

/// Resolves an `image` node's `file` input — as authored, relative to the
/// document — into a sampler. The second argument is the colour space the
/// texels are in: the `file` input's effective `colorspace` when the image
/// outputs a `color3` / `color4`, and `None` for a non-colour image (data,
/// never converted) or a colour one no scope declares a space for. `None`
/// from the loader declines the texture, and the node falls back to its
/// `default` input.
pub type TextureLoader<'a> = &'a dyn Fn(&str, Option<&str>) -> Option<TextureRef>;

/// Converts an RGB value authored in the named colour space into the
/// renderer's working space. Called at compile time, once per authored
/// `color3` / `color4` literal that has an effective colour space (and per
/// authored colour `default` of an image); never per shading point.
///
/// The pruning that drops a literal-zero weight tests the *converted* value,
/// so a conversion that does not keep black at black (a log encoding) is
/// still exact; one that does — any matrix, any transfer function through
/// zero — prunes exactly what the authored zero would have.
pub type ColorConverter<'a> = &'a dyn Fn(&str, [f32; 3]) -> [f32; 3];

/// What the compiler asks of the renderer: textures and colour conversion.
#[derive(Clone, Copy)]
pub struct Host<'a> {
    pub load_texture: TextureLoader<'a>,
    pub convert_color: ColorConverter<'a>,
}

impl<'a> Host<'a> {
    /// A host with `load_texture` whose working space *is* every authored
    /// space: literals pass through untouched, bit for bit. What a caller
    /// with no colour management (a test, a benchmark) wants.
    pub fn new(load_texture: TextureLoader<'a>) -> Host<'a> {
        Host {
            load_texture,
            convert_color: &identity_color,
        }
    }
}

/// The [`ColorConverter`] that converts nothing.
pub fn identity_color(_: &str, rgb: [f32; 3]) -> [f32; 3] {
    rgb
}

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
    /// The material's scalar displacement, when its `displacementshader` is
    /// a `displacement` node with a `float` input. Not one of [`roots`]: it
    /// is evaluated at mesh vertices, never per hit, so the host slices it
    /// out with [`Compiled::displacement_program`] *before*
    /// [`Compiled::optimize`], which then prunes it from the surface program
    /// (and clears this, whose slots it no longer holds).
    ///
    /// [`roots`]: Compiled::roots
    pub displacement: Option<DisplacementRoots>,
}

/// Where a `displacement` node's two inputs land in [`Compiled::program`]:
/// the offset is `value · scale`, along the normal, in object space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplacementRoots {
    pub value: u32,
    pub scale: u32,
}

impl Compiled {
    /// Every program slot a consumer reads.
    pub fn roots(&self) -> Vec<u32> {
        let mut roots = Vec::new();
        // `for_each_slot` visits mutably; walk a copy, the tree is small.
        self.closures.clone().for_each_slot(|s| roots.push(*s));
        roots
    }

    /// Replaces the program with [`Program::optimize`]'s and points every
    /// closure parameter at its slot's new home. Every root keeps its value
    /// bit for bit at every shading point; only the work to reach it shrinks.
    pub fn optimize(&mut self) {
        self.displacement = None;
        let (program, remap) = self.program.optimize(&self.roots());
        // Every root is live, so every root was placed.
        self.closures.for_each_slot(|s| {
            *s = remap[*s as usize].expect("a root slot survives optimization");
        });
        self.program = program;
    }

    /// The displacement's own program: both roots, optimised to what they
    /// depend on when `optimize` is set (bit for bit the values the full
    /// program computes), else the whole program as compiled. The roots come
    /// back at their slots in the returned program.
    pub fn displacement_program(&self, optimize: bool) -> Option<(Program, DisplacementRoots)> {
        let d = self.displacement?;
        if !optimize {
            return Some((self.program.clone(), d));
        }
        let (program, remap) = self.program.optimize(&[d.value, d.scale]);
        let at = |s: u32| remap[s as usize].expect("a root survives optimization");
        Some((
            program,
            DisplacementRoots {
                value: at(d.value),
                scale: at(d.scale),
            },
        ))
    }
}

/// Reads a `surfacematerial`'s `displacementshader` into `c`'s program.
///
/// Only MaterialX's scalar form is applied: a `displacement` node whose
/// `displacement` input is a `float`, times its `scale`. A `vector3` input
/// (vector displacement) or any other node is reported in `reported` and
/// yields nothing, so the surface renders undisplaced with one warning.
fn displacement(
    c: &mut Compiler<'_>,
    root: &Node,
    reported: &mut std::collections::BTreeSet<String>,
) -> Option<DisplacementRoots> {
    if root.category != "surfacematerial" {
        return None;
    }
    let node = bsdf::connected_node(c, root, "displacementshader")?;
    if node.category != "displacement" {
        reported.insert(format!(
            "displacementshader '{}' is a {} node, not displacement — not applied",
            node.name, node.category
        ));
        return None;
    }
    let input = node.input("displacement");
    let producer = bsdf::connected_node(c, &node, "displacement");
    let vector = input.is_some_and(|i| i.type_name == "vector3")
        || producer.is_some_and(|p| p.type_name == "vector3");
    if vector {
        reported.insert(format!(
            "displacement '{}' is vector3 (vector displacement) — not applied",
            node.name
        ));
        return None;
    }
    input?;
    let value = c.input_or(&node, "displacement", Val::ZERO);
    let scale = c.input_or(&node, "scale", Val::ONE);
    // A literal zero offset displaces nothing.
    let zero = |s: u32| c.fold(s).is_some_and(|v| v.x() == 0.0);
    if zero(value) || zero(scale) {
        return None;
    }
    Some(DisplacementRoots { value, scale })
}

/// Parses a `.mtlx` and compiles the named material node.
///
/// `material_node` is the `name` of the `surfacematerial` (or `surface`) node
/// to start from. When `None`, the first `surfacematerial` in the document is
/// used, which is what a single-material document means. Textures and colour
/// conversion go through `host`.
pub fn compile(
    path: &std::path::Path,
    material_node: Option<&str>,
    host: &Host<'_>,
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

    let mut c = Compiler::new(&doc, host);
    let mut closures = Closures::default();
    flatten(&mut c, &root, &mut closures);
    let displacement = displacement(&mut c, &root, &mut closures.reported);
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
        displacement,
    })
}
