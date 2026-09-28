//! A MaterialX closure graph, read as the tree it is.
//!
//! MaterialX assembles a look from *standalone* BSDF nodes —
//! `oren_nayar_diffuse_bsdf`, `dielectric_bsdf`, `conductor_bsdf`,
//! `generalized_schlick_bsdf`, `sheen_bsdf`, … — combined by `layer`, `mix`,
//! `add` and `multiply`, and its surface-shader nodes (`open_pbr_surface`,
//! `standard_surface`, `gltf_pbr`) are nodegraphs of exactly those. This
//! module keeps that structure: [`flatten`] turns the graph under a material
//! into a [`Closures`] — an arena of [`Closure`] nodes whose parameters are
//! slots of the same [`crate::Program`] the pattern graph compiles to — plus
//! the EDF terms and the volume the surface describes.
//!
//! What the tree *means* is MaterialX's (`libraries/pbrlib/genglsl`):
//!
//! - `mix(fg, bg, m)` is `m·fg + (1 − m)·bg`;
//! - `add(a, b)` is `a + b`;
//! - `multiply(x, w)` scales `x`'s response by `w` and leaves its throughput;
//! - `layer(top, base)` is `top + base · T_top(ωo)`, where `T_top` is the
//!   top's directional throughput — `1 − E(ωo)·weight` for a reflecting leaf;
//! - a branch pruned at compile time is MaterialX's empty `BSDF(0, 1)`: no
//!   response, and a throughput of 1 (see `Closures::mix`).
//!
//! Evaluating it is the renderer's half. Nothing here knows what a GGX lobe
//! is; the crate names MaterialX's leaves and their inputs, and crust-core's
//! `material/closure` evaluates them.
//!
//! The tree used to be flattened into a list of lobes pooled onto one OpenPBR
//! parameter set — roughness averaged, a `layer` guessed into "coat or base"
//! by its shape. Keeping the leaves apart is the point of this module now.

use crate::eval::{BinOp, Compiler, Op};
use crate::parse::{Node, Source};
use crate::value::Val;
use glam::Vec3A;
use std::collections::BTreeSet;

/// A program slot index.
pub type Slot = u32;

/// An index into [`Closures::nodes`].
pub type NodeId = u32;

/// A dielectric's or generalized Schlick's `scatter_mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScatterMode {
    /// Reflection only (the default).
    R,
    /// Transmission only.
    T,
    /// Both.
    RT,
}

impl ScatterMode {
    fn parse(text: Option<&str>) -> ScatterMode {
        match text.map(str::trim) {
            Some("T") => ScatterMode::T,
            Some("RT") => ScatterMode::RT,
            _ => ScatterMode::R,
        }
    }

    /// Whether this mode reflects.
    pub fn reflects(self) -> bool {
        self != ScatterMode::T
    }

    /// Whether this mode transmits.
    pub fn transmits(self) -> bool {
        self != ScatterMode::R
    }
}

/// A thin-film coating on a specular leaf: MaterialX 1.39's
/// `thinfilm_thickness` (nanometres) and `thinfilm_ior`.
#[derive(Clone, Copy, Debug)]
pub struct ThinFilm {
    pub thickness: Slot,
    pub ior: Slot,
}

/// Which diffuse model a diffuse leaf evaluates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffuseModel {
    /// `oren_nayar_diffuse_bsdf` without energy compensation: MaterialX's
    /// qualitative Oren–Nayar.
    OrenNayar,
    /// `oren_nayar_diffuse_bsdf` with `energy_compensation`: the
    /// energy-preserving Oren–Nayar (EON) OpenPBR names.
    Eon,
    /// `burley_diffuse_bsdf`.
    Burley,
}

/// A `sheen_bsdf`'s `mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheenMode {
    ContyKulla,
    Zeltner,
}

/// One MaterialX BSDF leaf, its inputs as program slots.
///
/// Microfacet `roughness` is MaterialX's: a `vector2` of GGX **alphas**
/// (what `roughness_anisotropy` produces), not a perceptual roughness. A
/// `float` connected to it arrives at arity 1 and means an isotropic alpha.
#[derive(Clone, Debug)]
pub enum Bsdf {
    Diffuse {
        model: DiffuseModel,
        color: Slot,
        /// Oren–Nayar sigma, or Burley's roughness.
        roughness: Slot,
    },
    Dielectric {
        tint: Slot,
        ior: Slot,
        roughness: Slot,
        mode: ScatterMode,
        thin_film: Option<ThinFilm>,
        /// An Abbe number for per-channel dispersion, when a surface builder
        /// supplies one. No standalone MaterialX node authors it.
        abbe: Option<Slot>,
    },
    Conductor {
        ior: Slot,
        extinction: Slot,
        roughness: Slot,
        thin_film: Option<ThinFilm>,
    },
    Schlick {
        color0: Slot,
        color82: Slot,
        color90: Slot,
        exponent: Slot,
        roughness: Slot,
        mode: ScatterMode,
        thin_film: Option<ThinFilm>,
    },
    Sheen {
        color: Slot,
        roughness: Slot,
        mode: SheenMode,
    },
    Subsurface {
        color: Slot,
        radius: Slot,
        anisotropy: Slot,
    },
    Translucent {
        color: Slot,
    },
}

impl Bsdf {
    /// The MaterialX node category this leaf came from, for probes and tests.
    pub fn category(&self) -> &'static str {
        match self {
            Bsdf::Diffuse {
                model: DiffuseModel::Burley,
                ..
            } => "burley_diffuse_bsdf",
            Bsdf::Diffuse { .. } => "oren_nayar_diffuse_bsdf",
            Bsdf::Dielectric { .. } => "dielectric_bsdf",
            Bsdf::Conductor { .. } => "conductor_bsdf",
            Bsdf::Schlick { .. } => "generalized_schlick_bsdf",
            Bsdf::Sheen { .. } => "sheen_bsdf",
            Bsdf::Subsurface { .. } => "subsurface_bsdf",
            Bsdf::Translucent { .. } => "translucent_bsdf",
        }
    }

    fn thin_film_mut(&mut self) -> Option<&mut Option<ThinFilm>> {
        match self {
            Bsdf::Dielectric { thin_film, .. }
            | Bsdf::Conductor { thin_film, .. }
            | Bsdf::Schlick { thin_film, .. } => Some(thin_film),
            _ => None,
        }
    }

    fn for_each_slot(&mut self, f: &mut impl FnMut(&mut Slot)) {
        let tf = |t: &mut Option<ThinFilm>, f: &mut dyn FnMut(&mut Slot)| {
            if let Some(t) = t {
                f(&mut t.thickness);
                f(&mut t.ior);
            }
        };
        match self {
            Bsdf::Diffuse {
                color, roughness, ..
            } => {
                f(color);
                f(roughness);
            }
            Bsdf::Dielectric {
                tint,
                ior,
                roughness,
                thin_film,
                abbe,
                ..
            } => {
                f(tint);
                f(ior);
                f(roughness);
                tf(thin_film, f);
                if let Some(a) = abbe {
                    f(a);
                }
            }
            Bsdf::Conductor {
                ior,
                extinction,
                roughness,
                thin_film,
            } => {
                f(ior);
                f(extinction);
                f(roughness);
                tf(thin_film, f);
            }
            Bsdf::Schlick {
                color0,
                color82,
                color90,
                exponent,
                roughness,
                thin_film,
                ..
            } => {
                f(color0);
                f(color82);
                f(color90);
                f(exponent);
                f(roughness);
                tf(thin_film, f);
            }
            Bsdf::Sheen {
                color, roughness, ..
            } => {
                f(color);
                f(roughness);
            }
            Bsdf::Subsurface {
                color,
                radius,
                anisotropy,
            } => {
                f(color);
                f(radius);
                f(anisotropy);
            }
            Bsdf::Translucent { color } => f(color),
        }
    }
}

/// A BSDF leaf with the inputs every leaf shares.
#[derive(Clone, Debug)]
pub struct Leaf {
    pub bsdf: Bsdf,
    /// The leaf's own `weight` input. It scales the response and enters the
    /// throughput (`1 − E·weight`), exactly as MaterialX's GLSL does.
    pub weight: Slot,
    /// The leaf's `normal`, when authored; the interpolated normal otherwise.
    pub normal: Option<Slot>,
    /// The leaf's `tangent`, when authored.
    pub tangent: Option<Slot>,
}

/// One node of the closure tree.
#[derive(Clone, Debug)]
pub enum Closure {
    Leaf(Leaf),
    /// `top` over `base`: `top + base · T_top(ωo)`.
    Layer {
        top: NodeId,
        base: NodeId,
    },
    /// `mix·fg + (1 − mix)·bg`, `mix` clamped to [0, 1].
    Mix {
        fg: NodeId,
        bg: NodeId,
        mix: Slot,
    },
    /// `a + b`; throughput `max(T_a + T_b − 1, 0)`.
    Add {
        a: NodeId,
        b: NodeId,
    },
    /// `weight · input`, `weight` clamped to [0, 1] per channel; the
    /// throughput is the input's.
    Multiply {
        input: NodeId,
        weight: Slot,
    },
    /// No BSDF: no response and a throughput of 1 — MaterialX's `BSDF(0, 1)`,
    /// which is what a zero-weight dielectric, generalized Schlick or sheen
    /// evaluates to. It holds the place of a branch pruned under a `mix`
    /// (`Closures::mix`); a `layer`, `add` or `multiply` over a pruned
    /// branch simplifies exactly without one.
    Empty,
}

/// One flattened EDF leaf: the weight reaching it and the program slot its
/// emitted radiance is computed in.
///
/// Emission stays a list rather than a tree: EDFs only ever add, a `mix`
/// partitions radiance exactly as it partitions weight, and no EDF has a
/// throughput for a `layer` to consult.
#[derive(Clone, Copy, Debug)]
pub struct Emission {
    /// Slot holding the emitted radiance — `uniform_edf`'s `color`.
    ///
    /// Deliberately not clamped anywhere downstream: radiance has no upper
    /// bound, and this is the one shading input in the renderer for which a
    /// value above 1.0 is meaningful rather than an authoring error.
    pub color: Slot,
    /// Slot holding the weight reaching this leaf — every `mix` factor along
    /// its path multiplied together.
    ///
    /// **Not necessarily a scalar.** MaterialX declares `ND_multiply_edfC`, a
    /// `multiply` on an EDF by a `color3`, and `Op::Binary { Mul }` promotes
    /// arity through `Val::zip` — so a tinted emitter arrives here as an
    /// arity-3 value with three distinct channels. Read it with `Val::rgb`,
    /// which broadcasts an arity-1 value.
    pub weight: Slot,
    /// A `generalized_schlick_edf` over this term: its radiance is scaled by
    /// `mix(color0, color90, (1 − cosθ)^exponent)` toward a view at θ from the
    /// normal. `None` for a uniform emitter.
    pub falloff: Option<EdfFalloff>,
}

/// `generalized_schlick_edf`'s angular falloff (MaterialX:
/// `mx_fresnel_schlick(NdotV, color0, color90, exponent)` times its `base`).
#[derive(Clone, Copy, Debug)]
pub struct EdfFalloff {
    pub color0: Slot,
    pub color90: Slot,
    pub exponent: Slot,
}

/// A homogeneous interior medium: MaterialX's `anisotropic_vdf`, or the
/// volume a surface node describes through its transmission inputs.
#[derive(Clone, Copy, Debug)]
pub struct Volume {
    /// Absorption coefficient σₐ per unit length (`vector3`).
    pub absorption: Slot,
    /// Scattering coefficient σₛ per unit length (`vector3`).
    pub scattering: Slot,
    /// Henyey–Greenstein anisotropy.
    pub anisotropy: Slot,
}

/// What [`flatten`] produces for one material.
#[derive(Clone, Default)]
pub struct Closures {
    /// The closure arena; children always precede their parents.
    pub nodes: Vec<Closure>,
    /// The BSDF tree's root, `None` for a surface with no BSDF.
    pub root: Option<NodeId>,
    /// The EDF terms, summed.
    pub emission: Vec<Emission>,
    /// The interior medium, when the material describes one.
    pub volume: Option<Volume>,
    /// The surface's `thin_walled` (0 or 1), when authored.
    pub thin_walled: Option<Slot>,
    /// Authored inputs the renderer cannot represent, and closures it
    /// approximates, for one warning per material.
    pub reported: BTreeSet<String>,
}

impl Closures {
    fn push(&mut self, c: Closure) -> NodeId {
        self.nodes.push(c);
        (self.nodes.len() - 1) as NodeId
    }

    /// `mix(fg, bg, m)` over branches either of which may have been pruned
    /// (`None`) — the one place a mix is built, for [`bsdf_tree`] and the
    /// surface builders alike.
    ///
    /// A pruned branch contributes no response, but it keeps its share of the
    /// throughput, `mix(T_bg, T_fg, m)` at `T = 1`, so the mix stays a mix and
    /// the pruned side becomes a [`Closure::Empty`]. Rewriting `mix(fg, ∅, m)`
    /// as `multiply(fg, m)` gets the response right and the throughput wrong:
    /// a `multiply` passes `T_fg` through, and for an opaque `fg` that is 0.
    /// The DPEL assets mix a diffuse against a zero-weight dielectric to turn
    /// a dust mask into a *coverage*, and under that rewrite their dust layer
    /// hid the entire look beneath it.
    pub(crate) fn mix(
        &mut self,
        fg: Option<NodeId>,
        bg: Option<NodeId>,
        mix: Slot,
    ) -> Option<NodeId> {
        if fg.is_none() && bg.is_none() {
            return None;
        }
        let fg = fg.unwrap_or_else(|| self.push(Closure::Empty));
        let bg = bg.unwrap_or_else(|| self.push(Closure::Empty));
        Some(self.push(Closure::Mix { fg, bg, mix }))
    }

    /// Number of BSDF leaves reachable from the root — what a shading point
    /// has to hold. The arena may also carry leaves a builder made and then
    /// pruned the branch of; those are never visited.
    pub fn leaf_count(&self) -> usize {
        fn count(cl: &Closures, id: NodeId) -> usize {
            match &cl.nodes[id as usize] {
                Closure::Leaf(_) => 1,
                Closure::Layer { top: a, base: b }
                | Closure::Mix { fg: a, bg: b, .. }
                | Closure::Add { a, b } => count(cl, *a) + count(cl, *b),
                Closure::Multiply { input, .. } => count(cl, *input),
                Closure::Empty => 0,
            }
        }
        self.root.map_or(0, |r| count(self, r))
    }

    /// The leaves reachable from the root, each with its **structural**
    /// weight at the evaluated `slots`: the product of every `mix` factor,
    /// `multiply` weight and its own `weight` along its path, with every
    /// `layer` treated as passing full weight to both branches.
    ///
    /// This is the weight a leaf has *before* layering attenuates it by the
    /// throughput of what sits above it — which needs the BSDFs themselves,
    /// and so is the renderer's to add. Probes and tests read it to check the
    /// tree's plumbing independently of any BSDF.
    pub fn structural_weights(&self, slots: &[Val]) -> Vec<(NodeId, Vec3A)> {
        let mut out = Vec::new();
        if let Some(r) = self.root {
            self.walk_weights(r, Vec3A::ONE, slots, &mut out);
        }
        out
    }

    fn walk_weights(&self, id: NodeId, w: Vec3A, slots: &[Val], out: &mut Vec<(NodeId, Vec3A)>) {
        match &self.nodes[id as usize] {
            Closure::Leaf(l) => {
                let own = slots[l.weight as usize].x();
                out.push((id, w * own));
            }
            Closure::Layer { top, base } => {
                self.walk_weights(*top, w, slots, out);
                self.walk_weights(*base, w, slots, out);
            }
            Closure::Mix { fg, bg, mix } => {
                let m = slots[*mix as usize].x().clamp(0.0, 1.0);
                self.walk_weights(*fg, w * m, slots, out);
                self.walk_weights(*bg, w * (1.0 - m), slots, out);
            }
            Closure::Add { a, b } => {
                self.walk_weights(*a, w, slots, out);
                self.walk_weights(*b, w, slots, out);
            }
            Closure::Multiply { input, weight } => {
                let k = slots[*weight as usize].rgb().clamp(Vec3A::ZERO, Vec3A::ONE);
                self.walk_weights(*input, w * k, slots, out);
            }
            Closure::Empty => {}
        }
    }

    /// Calls `f` on every program slot the closures read, in a fixed order.
    pub fn for_each_slot(&mut self, mut f: impl FnMut(&mut Slot)) {
        for n in &mut self.nodes {
            match n {
                Closure::Leaf(l) => {
                    f(&mut l.weight);
                    if let Some(s) = &mut l.normal {
                        f(s);
                    }
                    if let Some(s) = &mut l.tangent {
                        f(s);
                    }
                    l.bsdf.for_each_slot(&mut f);
                }
                Closure::Mix { mix, .. } => f(mix),
                Closure::Multiply { weight, .. } => f(weight),
                Closure::Layer { .. } | Closure::Add { .. } | Closure::Empty => {}
            }
        }
        for e in &mut self.emission {
            f(&mut e.color);
            f(&mut e.weight);
            if let Some(fo) = &mut e.falloff {
                f(&mut fo.color0);
                f(&mut fo.color90);
                f(&mut fo.exponent);
            }
        }
        if let Some(v) = &mut self.volume {
            f(&mut v.absorption);
            f(&mut v.scattering);
            f(&mut v.anisotropy);
        }
        if let Some(t) = &mut self.thin_walled {
            f(t);
        }
    }

    /// Applies a thin film to every specular leaf under `id` that has none.
    fn apply_thin_film(&mut self, id: NodeId, film: ThinFilm) {
        match self.nodes[id as usize].clone() {
            Closure::Leaf(_) => {
                if let Closure::Leaf(l) = &mut self.nodes[id as usize]
                    && let Some(tf) = l.bsdf.thin_film_mut()
                    && tf.is_none()
                {
                    *tf = Some(film);
                }
            }
            Closure::Layer { top: a, base: b }
            | Closure::Mix { fg: a, bg: b, .. }
            | Closure::Add { a, b } => {
                self.apply_thin_film(a, film);
                self.apply_thin_film(b, film);
            }
            Closure::Multiply { input, .. } => self.apply_thin_film(input, film),
            Closure::Empty => {}
        }
    }
}

/// Which closure tree is being walked.
///
/// Not cosmetic: [`closure_input`] gates a branch on its declared type, and
/// an EDF-typed `mix` declares `type="EDF"` on its `fg`/`bg`. Walking the
/// emission tree while still asking "is this a BSDF?" resolves every branch
/// to `None` and the emission vanishes — silently.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClosureType {
    Bsdf,
    Edf,
    Vdf,
}

/// How far a closure tree may nest before reading gives up.
///
/// MaterialX has no depth limit, but a hand-edited document can describe a
/// cycle through `layer`/`mix` that the pattern compiler's own cycle guard
/// does not see (it guards *pattern* recursion, and closure inputs are walked
/// separately). The bound also caps the walk: a cycle through both branches of
/// a `mix` doubles it per level.
const MAX_DEPTH: usize = 16;

/// Whether `depth` is past [`MAX_DEPTH`], reporting it when it is: what lies
/// deeper is dropped, and a truncated tree still shades plausibly.
fn too_deep(depth: usize, out: &mut Closures) -> bool {
    let deep = depth > MAX_DEPTH;
    if deep {
        out.reported.insert(format!(
            "closure graph nested past {MAX_DEPTH} levels (a cycle?): deeper closures dropped"
        ));
    }
    deep
}

/// Reads the closure graph under `node` — a `surfacematerial`, a `surface`,
/// a surface-shader node or a bare BSDF — into `out`.
pub fn flatten(c: &mut Compiler<'_>, node: &Node, out: &mut Closures) {
    shader(c, node, 0, out);
}

fn shader(c: &mut Compiler<'_>, node: &Node, depth: usize, out: &mut Closures) {
    if too_deep(depth, out) {
        return;
    }
    match node.category.as_str() {
        "surfacematerial" => {
            // `surfaceshader`-typed and the node's only connection — there is
            // nothing to disambiguate, so this follows whatever it points at.
            if let Some(n) = connected_node(c, node, "surfaceshader") {
                shader(c, &n, depth + 1, out);
            }
        }
        "surface" => {
            if let Some(n) = connected_node(c, node, "bsdf") {
                out.root = bsdf_tree(c, &n, depth + 1, out);
            }
            if let Some(n) = connected_node(c, node, "edf") {
                let one = c.constant(Val::ONE);
                edf_walk(c, &n, one, None, depth + 1, out);
            }
            if let Some(t) = c.optional_input(node, "thin_walled") {
                out.thin_walled = Some(t);
            }
            if authored_away(c, node, "opacity", Val::ONE) {
                out.reported.insert("opacity (no cutout)".into());
            }
        }
        "open_pbr_surface" | "standard_surface" | "gltf_pbr" => {
            crate::surface::build(c, node, out);
        }
        _ => out.root = bsdf_tree(c, node, depth, out),
    }
}

/// Whether `input` is authored away from `default`: connected, or a literal
/// that differs.
pub(crate) fn authored_away(c: &Compiler<'_>, node: &Node, input: &str, default: Val) -> bool {
    let _ = c;
    match node.input(input).map(|i| &i.source) {
        None => false,
        Some(Source::Value(v)) => {
            let lanes = (v.arity as usize).clamp(1, 4);
            let d = default.with_arity(v.arity.max(1));
            v.v[..lanes] != d.v[..lanes]
        }
        Some(_) => true,
    }
}

/// True when `input` is authored as a *literal* whose every meaningful lane is
/// zero.
///
/// Only a literal counts. A value arriving through a connection is a runtime
/// quantity even when it happens to evaluate to zero, and pruning on it would
/// make the tree depend on the shading point. Every lane, not just lane 0: a
/// `multiply` weight may be a `color3`, and `(0, 0.4, 0.4)` is not a pruned
/// branch.
fn literal_zero(node: &Node, input: &str) -> bool {
    node.input(input).is_some_and(|i| match &i.source {
        Source::Value(v) => {
            let lanes = (v.arity as usize).clamp(1, 4);
            v.v[..lanes].iter().all(|&c| c == 0.0)
        }
        _ => false,
    })
}

/// The literal scalar `input` is authored as, if it is one.
fn literal_scalar(node: &Node, input: &str) -> Option<f32> {
    match &node.input(input)?.source {
        Source::Value(v) => Some(v.x()),
        _ => None,
    }
}

/// Builds the BSDF tree under `node`, `None` for a branch that contributes
/// nothing (pruned or unsupported).
fn bsdf_tree(
    c: &mut Compiler<'_>,
    node: &Node,
    depth: usize,
    out: &mut Closures,
) -> Option<NodeId> {
    if too_deep(depth, out) {
        return None;
    }
    match node.category.as_str() {
        "layer" => {
            // `layer_vdf`: a BSDF over a volume. The volume is the surface's
            // interior; the layer is its top.
            if let Some(v) = closure_input(c, node, "base", ClosureType::Vdf) {
                vdf(c, &v, out);
                return closure_input(c, node, "top", ClosureType::Bsdf)
                    .and_then(|t| bsdf_tree(c, &t, depth + 1, out));
            }
            let top = closure_input(c, node, "top", ClosureType::Bsdf);
            // MaterialX 1.38 authored a film as `layer(thin_film_bsdf, x)`; in
            // 1.39 it is an input of the specular leaves. The old form is a
            // modifier on the interfaces beneath it, not a layer of its own.
            if let Some(t) = &top
                && t.category == "thin_film_bsdf"
            {
                let base = closure_input(c, node, "base", ClosureType::Bsdf)
                    .and_then(|b| bsdf_tree(c, &b, depth + 1, out))?;
                let film = ThinFilm {
                    thickness: c.input_or(t, "thickness", Val::float(550.0)),
                    ior: c.input_or(t, "ior", Val::float(1.5)),
                };
                out.apply_thin_film(base, film);
                return Some(base);
            }
            let base = closure_input(c, node, "base", ClosureType::Bsdf)
                .and_then(|b| bsdf_tree(c, &b, depth + 1, out));
            let top = top.and_then(|t| bsdf_tree(c, &t, depth + 1, out));
            match (top, base) {
                (Some(top), Some(base)) => Some(out.push(Closure::Layer { top, base })),
                (t, b) => t.or(b),
            }
        }
        "mix" => {
            let m = literal_scalar(node, "mix").or(if node.input("mix").is_none() {
                Some(0.0)
            } else {
                None
            });
            let branch = |c: &mut Compiler<'_>, out: &mut Closures, name: &str| {
                closure_input(c, node, name, ClosureType::Bsdf)
                    .and_then(|n| bsdf_tree(c, &n, depth + 1, out))
            };
            // A literal endpoint selects one branch outright: the other's
            // share of the response and of the throughput is 0 alike.
            match m {
                Some(m) if m <= 0.0 => branch(c, out, "bg"),
                Some(m) if m >= 1.0 => branch(c, out, "fg"),
                _ => {
                    let fg = branch(c, out, "fg");
                    let bg = branch(c, out, "bg");
                    let mix = c.input_or(node, "mix", Val::ZERO);
                    out.mix(fg, bg, mix)
                }
            }
        }
        "add" => {
            let a = closure_input(c, node, "in1", ClosureType::Bsdf)
                .and_then(|n| bsdf_tree(c, &n, depth + 1, out));
            let b = closure_input(c, node, "in2", ClosureType::Bsdf)
                .and_then(|n| bsdf_tree(c, &n, depth + 1, out));
            match (a, b) {
                (Some(a), Some(b)) => Some(out.push(Closure::Add { a, b })),
                (a, b) => a.or(b),
            }
        }
        "multiply" => {
            // `multiply(BSDF, float|color)`. Which side holds the BSDF is
            // decided by the declared type, never by position: the scalar
            // side is routinely a connected node (a texture, a mask chain).
            let (bsdf, scalar) = match closure_input(c, node, "in1", ClosureType::Bsdf) {
                Some(n) => (Some(n), "in2"),
                None => (closure_input(c, node, "in2", ClosureType::Bsdf), "in1"),
            };
            let n = bsdf?;
            if literal_zero(node, scalar) {
                return None;
            }
            let input = bsdf_tree(c, &n, depth + 1, out)?;
            let weight = c.input_or(node, scalar, Val::ONE);
            Some(out.push(Closure::Multiply { input, weight }))
        }
        _ => leaf(c, node, out).map(|l| out.push(Closure::Leaf(l))),
    }
}

/// Builds a leaf, or `None` for a BSDF node there is no leaf for (reported)
/// or one that can never contribute (a literal `weight = 0`).
fn leaf(c: &mut Compiler<'_>, node: &Node, out: &mut Closures) -> Option<Leaf> {
    const KNOWN: [&str; 9] = [
        "oren_nayar_diffuse_bsdf",
        "diffuse_bsdf",
        "burley_diffuse_bsdf",
        "dielectric_bsdf",
        "conductor_bsdf",
        "generalized_schlick_bsdf",
        "sheen_bsdf",
        "subsurface_bsdf",
        "translucent_bsdf",
    ];
    if !KNOWN.contains(&node.category.as_str()) {
        c.unsupported.insert(node.category.clone());
        return None;
    }
    if literal_zero(node, "weight") {
        return None;
    }
    let weight = c.input_or(node, "weight", Val::ONE);
    let normal = c.optional_input(node, "normal");
    let tangent = c.optional_input(node, "tangent");
    let alpha = Val::vec2(0.05, 0.05);
    let bsdf = match node.category.as_str() {
        "oren_nayar_diffuse_bsdf" | "diffuse_bsdf" => {
            let compensated = literal_scalar(node, "energy_compensation").is_some_and(|v| v != 0.0);
            Bsdf::Diffuse {
                model: if compensated {
                    DiffuseModel::Eon
                } else {
                    DiffuseModel::OrenNayar
                },
                color: c.input_or(node, "color", Val::vec3(0.18, 0.18, 0.18)),
                roughness: c.input_or(node, "roughness", Val::ZERO),
            }
        }
        "burley_diffuse_bsdf" => Bsdf::Diffuse {
            model: DiffuseModel::Burley,
            color: c.input_or(node, "color", Val::vec3(0.18, 0.18, 0.18)),
            roughness: c.input_or(node, "roughness", Val::ZERO),
        },
        "dielectric_bsdf" => Bsdf::Dielectric {
            tint: c.input_or(node, "tint", Val::ONE),
            ior: c.input_or(node, "ior", Val::float(1.5)),
            roughness: c.input_or(node, "roughness", alpha),
            mode: ScatterMode::parse(text(node, "scatter_mode")),
            thin_film: thin_film(c, node),
            abbe: None,
        },
        "conductor_bsdf" => Bsdf::Conductor {
            ior: c.input_or(node, "ior", Val::vec3(0.183, 0.421, 1.373)),
            extinction: c.input_or(node, "extinction", Val::vec3(3.424, 2.346, 1.770)),
            roughness: c.input_or(node, "roughness", alpha),
            thin_film: thin_film(c, node),
        },
        "generalized_schlick_bsdf" => Bsdf::Schlick {
            color0: c.input_or(node, "color0", Val::ONE),
            color82: c.input_or(node, "color82", Val::ONE),
            color90: c.input_or(node, "color90", Val::ONE),
            exponent: c.input_or(node, "exponent", Val::float(5.0)),
            roughness: c.input_or(node, "roughness", alpha),
            mode: ScatterMode::parse(text(node, "scatter_mode")),
            thin_film: thin_film(c, node),
        },
        "sheen_bsdf" => {
            let mode = match text(node, "mode").map(str::trim) {
                Some("zeltner") => SheenMode::Zeltner,
                _ => SheenMode::ContyKulla,
            };
            if mode == SheenMode::Zeltner {
                out.reported
                    .insert("sheen_bsdf mode zeltner (evaluated as conty_kulla)".into());
            }
            Bsdf::Sheen {
                color: c.input_or(node, "color", Val::ONE),
                roughness: c.input_or(node, "roughness", Val::float(0.3)),
                mode,
            }
        }
        "subsurface_bsdf" => {
            out.reported
                .insert("subsurface_bsdf (no random walk: shaded as diffuse)".into());
            Bsdf::Subsurface {
                color: c.input_or(node, "color", Val::vec3(0.18, 0.18, 0.18)),
                radius: c.input_or(node, "radius", Val::ONE),
                anisotropy: c.input_or(node, "anisotropy", Val::ZERO),
            }
        }
        _ => Bsdf::Translucent {
            color: c.input_or(node, "color", Val::ONE),
        },
    };
    Some(Leaf {
        bsdf,
        weight,
        normal,
        tangent,
    })
}

fn text<'n>(node: &'n Node, input: &str) -> Option<&'n str> {
    node.input(input).and_then(|i| i.text.as_deref())
}

/// A leaf's MaterialX 1.39 thin film, `None` when its thickness is a literal
/// zero (the default).
fn thin_film(c: &mut Compiler<'_>, node: &Node) -> Option<ThinFilm> {
    if node.input("thinfilm_thickness").is_none() || literal_zero(node, "thinfilm_thickness") {
        return None;
    }
    Some(ThinFilm {
        thickness: c.input_or(node, "thinfilm_thickness", Val::ZERO),
        ior: c.input_or(node, "thinfilm_ior", Val::float(1.5)),
    })
}

/// Records a VDF as the surface's interior medium.
fn vdf(c: &mut Compiler<'_>, node: &Node, out: &mut Closures) {
    match node.category.as_str() {
        "anisotropic_vdf" => {
            out.volume = Some(Volume {
                absorption: c.input_or(node, "absorption", Val::vec3(0.0, 0.0, 0.0)),
                scattering: c.input_or(node, "scattering", Val::vec3(0.0, 0.0, 0.0)),
                anisotropy: c.input_or(node, "anisotropy", Val::ZERO),
            });
        }
        "absorption_vdf" => {
            let zero = c.constant(Val::vec3(0.0, 0.0, 0.0));
            out.volume = Some(Volume {
                absorption: c.input_or(node, "absorption", Val::vec3(0.0, 0.0, 0.0)),
                scattering: zero,
                anisotropy: c.constant(Val::ZERO),
            });
        }
        other => {
            c.unsupported.insert(other.to_string());
        }
    }
}

/// Walks the EDF tree, accumulating the weight reaching each `uniform_edf` and
/// the `generalized_schlick_edf` falloff over it, if any.
pub(crate) fn edf_walk(
    c: &mut Compiler<'_>,
    node: &Node,
    weight: Slot,
    falloff: Option<EdfFalloff>,
    depth: usize,
    out: &mut Closures,
) {
    if too_deep(depth, out) {
        return;
    }
    match node.category.as_str() {
        "mix" => {
            let m = c.input_or(node, "mix", Val::ZERO);
            let one = c.constant(Val::ONE);
            let inv = c.emit(Op::Invert { a: m, amount: one });
            let w_fg = c.emit(Op::Binary {
                op: BinOp::Mul,
                a: weight,
                b: m,
            });
            let w_bg = c.emit(Op::Binary {
                op: BinOp::Mul,
                a: weight,
                b: inv,
            });
            if let Some(n) = closure_input(c, node, "bg", ClosureType::Edf) {
                edf_walk(c, &n, w_bg, falloff, depth + 1, out);
            }
            if let Some(n) = closure_input(c, node, "fg", ClosureType::Edf) {
                edf_walk(c, &n, w_fg, falloff, depth + 1, out);
            }
        }
        "add" => {
            for name in ["in1", "in2"] {
                if let Some(n) = closure_input(c, node, name, ClosureType::Edf) {
                    edf_walk(c, &n, weight, falloff, depth + 1, out);
                }
            }
        }
        "multiply" => {
            let (edf, scalar) = match closure_input(c, node, "in1", ClosureType::Edf) {
                Some(n) => (Some(n), "in2"),
                None => (closure_input(c, node, "in2", ClosureType::Edf), "in1"),
            };
            // A literal-zero scalar prunes the branch, which keeps the list
            // empty for a graph that only looks emissive. The `color3` case
            // reaches the weight slot too: `Mul` promotes arity, so the
            // scalar operand's three channels survive — read it as RGB.
            if let Some(n) = edf
                && !literal_zero(node, scalar)
            {
                let s = c.input_or(node, scalar, Val::ONE);
                let w = c.emit(Op::Binary {
                    op: BinOp::Mul,
                    a: weight,
                    b: s,
                });
                edf_walk(c, &n, w, falloff, depth + 1, out);
            }
        }
        // A view-dependent Fresnel-style falloff over its `base` EDF. One
        // level is represented; a falloff over a falloff is reported.
        "generalized_schlick_edf" => {
            if falloff.is_some() {
                c.unsupported
                    .insert("generalized_schlick_edf (nested)".into());
                return;
            }
            let fo = EdfFalloff {
                color0: c.input_or(node, "color0", Val::ONE),
                color90: c.input_or(node, "color90", Val::ONE),
                exponent: c.input_or(node, "exponent", Val::float(5.0)),
            };
            if let Some(n) = closure_input(c, node, "base", ClosureType::Edf) {
                edf_walk(c, &n, weight, Some(fo), depth + 1, out);
            }
        }
        // Only `uniform_edf` is mapped, and the refusal of the others is the
        // point rather than an omission: crust's emitter is uniform, and a
        // cone, an IES profile or a Schlick falloff would be a plausible glow
        // at the wrong intensity. They land in `unsupported` and are warned.
        "uniform_edf" => {
            // A literal black EDF is a dummy branch; dropping it keeps the
            // emission list *empty* for a graph that only looks emissive —
            // what the consumer's "evaluate nothing" fast path keys on.
            if literal_zero(node, "color") {
                return;
            }
            let color = c.input_or(node, "color", Val::ONE);
            out.emission.push(Emission {
                color,
                weight,
                falloff,
            });
        }
        other => {
            c.unsupported.insert(other.to_string());
        }
    }
}

/// Follows an input to the node feeding it, whatever that node's type.
///
/// For inputs that cannot be confused with an operand of another type — a
/// `surfacematerial`'s one shader, a `surface`'s one bsdf.
pub(crate) fn connected_node(c: &Compiler<'_>, node: &Node, name: &str) -> Option<Node> {
    let input = node.input(name)?;
    let scope = node.graph.clone().unwrap_or_default();
    match &input.source {
        Source::Node { name, .. } => c.doc.find(&scope, name).cloned(),
        Source::Graph { graph, output } => {
            c.doc.graph_output(graph, output).map(|g| g.node.clone())
        }
        Source::Value(_) => None,
    }
}

/// Follows a **closure-typed** input to the node feeding it, and `None` for
/// one carrying anything else.
///
/// Either declaration counts — the input's own `type`, or the target node's.
/// A conformant document states both, but a `type` attribute omitted on an
/// input parses as `float` (its schema default), and a node declared
/// `type="BSDF"` is a BSDF whatever the edge to it says.
fn closure_input(c: &Compiler<'_>, node: &Node, name: &str, closure: ClosureType) -> Option<Node> {
    let declared = node
        .input(name)
        .is_some_and(|i| is_closure_type(&i.type_name, closure));
    let target = connected_node(c, node, name)?;
    (declared || is_closure_type(&target.type_name, closure)).then_some(target)
}

/// MaterialX's type name for a closure, as authored.
fn is_closure_type(type_name: &str, closure: ClosureType) -> bool {
    match closure {
        ClosureType::Bsdf => type_name.eq_ignore_ascii_case("bsdf"),
        ClosureType::Edf => type_name.eq_ignore_ascii_case("edf"),
        ClosureType::Vdf => type_name.eq_ignore_ascii_case("vdf"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::ShadeCtx;
    use crate::parse::Doc;

    fn build(text: &str, root: &str) -> (Vec<Val>, Closures) {
        let doc = Doc::parse(text).unwrap();
        let loader = |_: &str, _: Option<&str>| None;
        let mut c = Compiler::new(&doc, &loader);
        let node = doc.find("", root).unwrap().clone();
        let mut out = Closures::default();
        flatten(&mut c, &node, &mut out);
        let mut slots = Vec::new();
        c.program.eval(
            &ShadeCtx {
                uv: (0.0, 0.0),
                normal: Vec3A::Z,
                tangent: Vec3A::X,
                view: -Vec3A::Z,
                position: Vec3A::ZERO,
                uv_width: 0.0,
            },
            &mut slots,
        );
        (slots, out)
    }

    /// `(category, structural weight)` per reachable leaf.
    fn weights(text: &str, root: &str) -> Vec<(&'static str, f32)> {
        let (slots, cl) = build(text, root);
        cl.structural_weights(&slots)
            .into_iter()
            .map(|(id, w)| match &cl.nodes[id as usize] {
                Closure::Leaf(l) => (l.bsdf.category(), w.x),
                _ => unreachable!(),
            })
            .collect()
    }

    const MIXED: &str = r#"<materialx>
      <oren_nayar_diffuse_bsdf name="d" type="BSDF">
        <input name="color" type="color3" value="0.8, 0.2, 0.2" />
      </oren_nayar_diffuse_bsdf>
      <conductor_bsdf name="c" type="BSDF">
        <input name="roughness" type="vector2" value="0.1, 0.1" />
      </conductor_bsdf>
      <mix name="m" type="BSDF">
        <input name="bg" type="BSDF" nodename="c" />
        <input name="fg" type="BSDF" nodename="d" />
        <input name="mix" type="float" value="0.25" />
      </mix>
    </materialx>"#;

    #[test]
    fn a_mix_partitions_weight_between_its_branches() {
        let w = weights(MIXED, "m");
        assert_eq!(
            w,
            vec![("oren_nayar_diffuse_bsdf", 0.25), ("conductor_bsdf", 0.75)]
        );
    }

    #[test]
    fn a_mix_keeps_both_leaves_apart() {
        let (_, cl) = build(MIXED, "m");
        assert_eq!(cl.leaf_count(), 2);
        assert!(matches!(
            cl.nodes[cl.root.unwrap() as usize],
            Closure::Mix { .. }
        ));
    }

    #[test]
    fn a_layer_keeps_its_top_and_base() {
        let doc = r#"<materialx>
          <dielectric_bsdf name="t" type="BSDF" />
          <oren_nayar_diffuse_bsdf name="b" type="BSDF" />
          <layer name="l" type="BSDF">
            <input name="top" type="BSDF" nodename="t" />
            <input name="base" type="BSDF" nodename="b" />
          </layer>
        </materialx>"#;
        let (_, cl) = build(doc, "l");
        let Closure::Layer { top, base } = cl.nodes[cl.root.unwrap() as usize] else {
            panic!("root is a layer");
        };
        let cat = |id: NodeId| match &cl.nodes[id as usize] {
            Closure::Leaf(l) => l.bsdf.category(),
            _ => "?",
        };
        assert_eq!(cat(top), "dielectric_bsdf");
        assert_eq!(cat(base), "oren_nayar_diffuse_bsdf");
    }

    #[test]
    fn a_literal_zero_weight_leaf_is_pruned() {
        let doc = r#"<materialx>
          <dielectric_bsdf name="t" type="BSDF">
            <input name="weight" type="float" value="0" />
          </dielectric_bsdf>
          <oren_nayar_diffuse_bsdf name="b" type="BSDF" />
          <layer name="l" type="BSDF">
            <input name="top" type="BSDF" nodename="t" />
            <input name="base" type="BSDF" nodename="b" />
          </layer>
        </materialx>"#;
        let (_, cl) = build(doc, "l");
        assert_eq!(cl.leaf_count(), 1);
        assert!(matches!(
            cl.nodes[cl.root.unwrap() as usize],
            Closure::Leaf(_)
        ));
    }

    #[test]
    fn a_mix_against_a_pruned_branch_stays_a_mix() {
        // The zero-weight dielectric is pruned, but the mix keeps its place
        // with an empty closure there: `mix(T_bg, T_fg, m)` still needs the
        // pruned side's throughput of 1, which `multiply(fg, m)` would replace
        // with the diffuse's 0.
        let doc = r#"<materialx>
          <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
          <dielectric_bsdf name="t" type="BSDF">
            <input name="weight" type="float" value="0" />
          </dielectric_bsdf>
          <mix name="m" type="BSDF">
            <input name="fg" type="BSDF" nodename="d" />
            <input name="bg" type="BSDF" nodename="t" />
            <input name="mix" type="float" value="0.25" />
          </mix>
        </materialx>"#;
        let (_, cl) = build(doc, "m");
        let Closure::Mix { fg, bg, .. } = cl.nodes[cl.root.unwrap() as usize] else {
            panic!("the root stays a mix");
        };
        assert!(matches!(cl.nodes[fg as usize], Closure::Leaf(_)));
        assert!(matches!(cl.nodes[bg as usize], Closure::Empty));
        assert_eq!(cl.leaf_count(), 1);
        assert_eq!(weights(doc, "m"), vec![("oren_nayar_diffuse_bsdf", 0.25)]);
    }

    #[test]
    fn a_literal_mix_endpoint_prunes_the_other_branch() {
        let doc = MIXED.replace("value=\"0.25\"", "value=\"1\"");
        let w = weights(&doc, "m");
        assert_eq!(w, vec![("oren_nayar_diffuse_bsdf", 1.0)]);
    }

    #[test]
    fn a_thin_film_layer_becomes_a_film_on_the_interface_beneath() {
        let doc = r#"<materialx>
          <thin_film_bsdf name="f" type="BSDF">
            <input name="thickness" type="float" value="400" />
          </thin_film_bsdf>
          <dielectric_bsdf name="d" type="BSDF" />
          <layer name="l" type="BSDF">
            <input name="top" type="BSDF" nodename="f" />
            <input name="base" type="BSDF" nodename="d" />
          </layer>
        </materialx>"#;
        let (slots, cl) = build(doc, "l");
        let Closure::Leaf(l) = &cl.nodes[cl.root.unwrap() as usize] else {
            panic!("the film is not a layer of its own");
        };
        let Bsdf::Dielectric {
            thin_film: Some(tf),
            ..
        } = &l.bsdf
        else {
            panic!("the dielectric carries the film");
        };
        assert_eq!(slots[tf.thickness as usize].x(), 400.0);
    }

    #[test]
    fn a_zeltner_sheen_is_reported() {
        let doc = r#"<materialx>
          <sheen_bsdf name="s" type="BSDF">
            <input name="mode" type="string" value="zeltner" />
          </sheen_bsdf>
        </materialx>"#;
        let (_, cl) = build(doc, "s");
        assert!(cl.reported.iter().any(|r| r.contains("zeltner")));
    }

    #[test]
    fn a_closure_cycle_is_cut_and_reported() {
        // `l`'s base is `l` itself: nothing in MaterialX forbids writing it,
        // and the walk must end, keep the diffuse, and say what it dropped.
        let doc = r#"<materialx>
          <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
          <layer name="l" type="BSDF">
            <input name="top" type="BSDF" nodename="d" />
            <input name="base" type="BSDF" nodename="l" />
          </layer>
        </materialx>"#;
        let (_, cl) = build(doc, "l");
        assert!(cl.leaf_count() > 0);
        assert!(cl.reported.iter().any(|r| r.contains("nested past")));
        // A tree within the bound reports nothing.
        let (_, cl) = build(MIXED, "m");
        assert!(cl.reported.is_empty());
    }

    #[test]
    fn a_layered_vdf_becomes_the_interior_volume() {
        let doc = r#"<materialx>
          <dielectric_bsdf name="t" type="BSDF">
            <input name="scatter_mode" type="string" value="T" />
          </dielectric_bsdf>
          <anisotropic_vdf name="v" type="VDF">
            <input name="absorption" type="vector3" value="0.1, 0.2, 0.3" />
            <input name="scattering" type="vector3" value="1, 2, 3" />
            <input name="anisotropy" type="float" value="0.4" />
          </anisotropic_vdf>
          <layer name="l" type="BSDF">
            <input name="top" type="BSDF" nodename="t" />
            <input name="base" type="VDF" nodename="v" />
          </layer>
          <surface name="s" type="surfaceshader">
            <input name="bsdf" type="BSDF" nodename="l" />
            <input name="thin_walled" type="boolean" value="false" />
          </surface>
        </materialx>"#;
        let (slots, cl) = build(doc, "s");
        let v = cl.volume.expect("the VDF is the interior");
        assert_eq!(
            slots[v.absorption as usize].rgb(),
            Vec3A::new(0.1, 0.2, 0.3)
        );
        assert_eq!(
            slots[v.scattering as usize].rgb(),
            Vec3A::new(1.0, 2.0, 3.0)
        );
        assert_eq!(slots[v.anisotropy as usize].x(), 0.4);
        assert_eq!(slots[cl.thin_walled.unwrap() as usize].x(), 0.0);
        // The layer is its top: the dielectric alone.
        assert_eq!(cl.leaf_count(), 1);
        assert!(matches!(
            cl.nodes[cl.root.unwrap() as usize],
            Closure::Leaf(_)
        ));
    }

    #[test]
    fn scatter_mode_is_read() {
        let doc = r#"<materialx>
          <dielectric_bsdf name="d" type="BSDF">
            <input name="scatter_mode" type="string" value="RT" />
          </dielectric_bsdf>
        </materialx>"#;
        let (_, cl) = build(doc, "d");
        let Closure::Leaf(Leaf {
            bsdf: Bsdf::Dielectric { mode, .. },
            ..
        }) = &cl.nodes[0]
        else {
            panic!()
        };
        assert_eq!(*mode, ScatterMode::RT);
    }
}
