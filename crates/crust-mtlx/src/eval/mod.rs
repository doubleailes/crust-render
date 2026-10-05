//! The MaterialX pattern graph, compiled to a flat program and run per hit.
//!
//! A `.mtlx` look-dev graph is evaluated *per shading point* — its textures,
//! masks and blends are the whole point — so this cannot be folded to
//! constants at import. But it must also not be walked as a name-addressed DOM
//! inside the integrator: every lookup would be a string hash, and the teapot's
//! ceramic graph alone is ~50 nodes consulted several times per path vertex
//! (once to sample, again for each NEE and guide evaluation).
//!
//! So the graph is **compiled once** into a [`Program`]: a topologically
//! ordered `Vec<Op>` whose operands are slot *indices* into the values
//! computed so far. Evaluation is then a linear scan with no branching on
//! names, no allocation (the value stack is a thread-local scratch buffer
//! reused across calls), and no hash lookups.
//!
//! The compiler is also where cycles and unknown operators are dealt with:
//! a node that cannot be compiled becomes a constant, so an unsupported
//! MaterialX node degrades that one input to its default rather than failing
//! the material.

mod apply;
mod compile;
#[cfg(test)]
mod tests;

use crate::texture::TextureRef;
use crate::value::Val;
use glam::Vec3A;

use apply::apply;
pub use apply::{perturb_normal, reflectivity_from_ior};
pub use compile::Compiler;

/// The shading point a [`Program`] is evaluated at.
#[derive(Clone, Copy)]
pub struct ShadeCtx {
    /// `primvars:st`, unwrapped — the integer part selects a UDIM tile.
    pub uv: (f32, f32),
    /// Geometric (ray-facing) world-space normal.
    pub normal: Vec3A,
    /// World-space tangent along increasing `u`; `ZERO` when unknown, which
    /// makes `normalmap` pass the geometric normal straight through rather
    /// than build a frame out of noise.
    pub tangent: Vec3A,
    /// Direction from the viewer *to* the surface — MaterialX's
    /// `viewdirection`, which is the incoming ray's direction, not its
    /// negation.
    pub view: Vec3A,
    pub position: Vec3A,
    /// Diameter of the shading point's texture footprint, in the same UV
    /// units as `uv` — what [`crate::Texture::eval`] filters over. `0.0` asks
    /// every texture in the graph to point-sample, which is what a host that
    /// tracks no footprint should leave it at.
    pub uv_width: f32,
}

/// A componentwise binary operator.
#[derive(Clone, Copy, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Min,
    Max,
    /// `a` with `b` lanes, for `combine`-style plumbing.
    Modulo,
}

/// A componentwise unary operator.
#[derive(Clone, Copy, Debug)]
pub enum UnOp {
    Abs,
    Ln,
    Exp,
    Sin,
    Cos,
    Asin,
    Acos,
    Sqrt,
    Sign,
    Floor,
    Ceil,
    Normalize,
}

/// One instruction. Operands are slot indices, always strictly less than the
/// instruction's own slot — the compiler emits in topological order, so a
/// single forward pass evaluates the whole program.
#[derive(Clone, Debug)]
pub enum Op {
    Const(Val),
    /// A UV texture lookup. `tex` is `None` when the host declined the file,
    /// in which case `fallback` — the node's `default` input, or mid-grey —
    /// stands in, which is what keeps an undecodable texture from blackening
    /// a surface.
    Texture {
        tex: Option<TextureRef>,
        fallback: Val,
        /// `uvtiling` / `uvoffset` from a `tiledimage`; identity for `image`.
        scale: [f32; 2],
        offset: [f32; 2],
        arity: u8,
        /// Where to look up relative to the shading point, in footprint
        /// widths (see `shifted_uv`). Zero everywhere except the copies of
        /// a subgraph `heighttonormal` differentiates.
        shift: [f32; 2],
        /// The slot holding an authored `texcoord` connection, when the
        /// image has one other than the default chart; `None` reads the
        /// shading point's `uv` (displaced by `shift`). A connected
        /// coordinate carries any shift in its own `TexCoord` ops, so a
        /// coordinate that does not depend on the chart is not displaced.
        coord: Option<u32>,
    },
    /// `primvars:st` as a `vector2`, displaced by `shift` footprint widths
    /// exactly as [`Op::Texture`] is.
    TexCoord {
        shift: [f32; 2],
    },
    /// World-space geometric normal.
    Normal,
    ViewDirection,
    Position,
    Unary {
        op: UnOp,
        a: u32,
    },
    Binary {
        op: BinOp,
        a: u32,
        b: u32,
    },
    /// `bg·(1−m) + fg·m`, MaterialX's `mix`.
    Mix {
        fg: u32,
        bg: u32,
        m: u32,
    },
    Clamp {
        a: u32,
        low: u32,
        high: u32,
    },
    /// `(in − pivot)·amount + pivot`.
    Contrast {
        a: u32,
        amount: u32,
        pivot: u32,
    },
    /// Linear rescale from `[inlow, inhigh]` to `[outlow, outhigh]`.
    Remap {
        a: u32,
        in_low: u32,
        in_high: u32,
        out_low: u32,
        out_high: u32,
    },
    /// `amount − in`.
    Invert {
        a: u32,
        amount: u32,
    },
    /// Reinterpret at a different lane count, broadcasting a scalar.
    Convert {
        a: u32,
        arity: u8,
    },
    /// One lane of a wider value.
    Extract {
        a: u32,
        index: usize,
    },
    Combine3 {
        a: u32,
        b: u32,
        c: u32,
    },
    Combine2 {
        a: u32,
        b: u32,
    },
    DotProduct {
        a: u32,
        b: u32,
    },
    /// `dot(in.rgb, coeffs)`, keeping a `color4`'s alpha.
    Luminance {
        a: u32,
        coeffs: u32,
    },
    /// Decodes a tangent-space normal map into a world-space normal.
    NormalMap {
        a: u32,
        scale: u32,
    },
    /// Gulbrandsen's artist-friendly metal parameterisation. `extinction`
    /// selects which of the node's two outputs this slot holds.
    ArtisticIor {
        reflectivity: u32,
        edge: u32,
        extinction: bool,
    },
    /// Hermite interpolation between two edges.
    Smoothstep {
        a: u32,
        low: u32,
        high: u32,
    },
    /// MaterialX `hsvadjust`: to HSV, add `amount.x` to the hue and scale
    /// saturation and value by `amount.y` / `amount.z`, and back.
    HsvAdjust {
        a: u32,
        amount: u32,
    },
    /// MaterialX `heighttonormal`, from the height at half a footprint either
    /// side of the shading point along `u` (`xp`, `xm`) and `v` (`yp`, `ym`).
    /// The result is encoded in `[0, 1]`, like a normal-map texel.
    HeightToNormal {
        xp: u32,
        xm: u32,
        yp: u32,
        ym: u32,
        scale: u32,
    },
}

impl Op {
    /// Calls `f` on every operand slot index, in a fixed order.
    pub fn for_each_operand(&mut self, mut f: impl FnMut(&mut u32)) {
        match self {
            Op::Texture { coord, .. } => {
                if let Some(c) = coord {
                    f(c);
                }
            }
            Op::Const(_) | Op::TexCoord { .. } | Op::Normal | Op::ViewDirection | Op::Position => {}
            Op::Unary { a, .. } | Op::Convert { a, .. } | Op::Extract { a, .. } => f(a),
            Op::Binary { a, b, .. }
            | Op::Invert { a, amount: b }
            | Op::Combine2 { a, b }
            | Op::Luminance { a, coeffs: b }
            | Op::DotProduct { a, b }
            | Op::NormalMap { a, scale: b }
            | Op::HsvAdjust { a, amount: b }
            | Op::ArtisticIor {
                reflectivity: a,
                edge: b,
                ..
            } => {
                f(a);
                f(b);
            }
            Op::Mix { fg, bg, m } => {
                f(fg);
                f(bg);
                f(m);
            }
            Op::Clamp { a, low, high } | Op::Smoothstep { a, low, high } => {
                f(a);
                f(low);
                f(high);
            }
            Op::Contrast { a, amount, pivot } => {
                f(a);
                f(amount);
                f(pivot);
            }
            Op::Combine3 { a, b, c } => {
                f(a);
                f(b);
                f(c);
            }
            Op::Remap {
                a,
                in_low,
                in_high,
                out_low,
                out_high,
            } => {
                f(a);
                f(in_low);
                f(in_high);
                f(out_low);
                f(out_high);
            }
            Op::HeightToNormal {
                xp,
                xm,
                yp,
                ym,
                scale,
            } => {
                f(xp);
                f(xm);
                f(yp);
                f(ym);
                f(scale);
            }
        }
    }

    /// Whether the result depends on nothing but the operands — no shading
    /// point, no texture. Such an op over constant operands is a constant.
    fn is_pure(&self) -> bool {
        match self {
            Op::Texture { tex, .. } => tex.is_none(),
            Op::TexCoord { .. }
            | Op::Normal
            | Op::ViewDirection
            | Op::Position
            | Op::NormalMap { .. } => false,
            _ => true,
        }
    }
}

/// A compiled pattern graph.
///
/// Slots `0..consts.len()` hold `consts`, copied in once per evaluation; slot
/// `consts.len() + i` holds the value of `ops[i]`. The compiler leaves
/// `consts` empty and emits every literal as an [`Op::Const`];
/// [`Program::optimize`] moves them (and everything computable from them)
/// into `consts`.
#[derive(Clone, Default)]
pub struct Program {
    pub consts: Vec<Val>,
    pub ops: Vec<Op>,
}

impl Program {
    /// Number of slots an evaluation fills.
    pub fn len(&self) -> usize {
        self.consts.len() + self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether every operand refers to a slot strictly before its user's —
    /// what the compiler always emits, and what anything that runs a program
    /// by other means than [`Program::eval`]'s bounds-checked reads relies on.
    pub fn is_well_formed(&self) -> bool {
        let nc = self.consts.len();
        self.ops.iter().enumerate().all(|(i, op)| {
            let mut ok = true;
            op.clone()
                .for_each_operand(|o| ok &= (*o as usize) < nc + i);
            ok
        })
    }

    /// One instruction's value, given the slots computed before it — the
    /// interpreter's own step, for an evaluator that runs some ops another
    /// way and hands the rest back here (crust-jit), so that both produce the
    /// same bits by construction.
    pub fn apply_op(op: &Op, slots: &[Val], ctx: &ShadeCtx) -> Val {
        apply(op, slots, ctx)
    }

    /// Evaluates every instruction into `slots`, which is resized as needed.
    ///
    /// The caller owns the buffer so it can be reused across shading calls —
    /// a fresh `Vec` per call would allocate once per BSDF evaluation, which
    /// at several evaluations per path vertex is the kind of cost that does
    /// not show up in a profile as one hot line.
    pub fn eval(&self, ctx: &ShadeCtx, slots: &mut Vec<Val>) {
        slots.clear();
        slots.reserve(self.len());
        slots.extend_from_slice(&self.consts);
        for op in &self.ops {
            let v = apply(op, slots, ctx);
            slots.push(v);
        }
    }

    /// Rewrites the program so it computes the same values at `roots` with
    /// less work per evaluation, and returns where each old slot went
    /// (`None` for a slot that no longer exists).
    ///
    /// Three passes, each exact rather than approximate:
    ///
    /// - **Constant folding.** An op whose operands are all constant, and
    ///   which reads neither the shading point nor a texture, is evaluated
    ///   here — by `apply`, the function the interpreter runs, so the value
    ///   is the one every hit would have computed, bit for bit.
    /// - **Constant hoisting and deduplication.** Constants move into
    ///   [`Program::consts`], copied in with one `memcpy` per evaluation
    ///   instead of one dispatched instruction each, and bitwise-equal ones
    ///   share a slot (the compiler emits a fresh constant for every literal
    ///   and every unauthored input's default).
    /// - **Dead-code elimination.** Ops nothing at `roots` depends on are
    ///   dropped.
    ///
    /// The surviving ops keep their relative order, so operands still precede
    /// their users.
    ///
    /// A malformed program — an operand that does not precede its user, or a
    /// root out of range — is returned unchanged with the identity remap: the
    /// interpreter already reads such an operand as zero, and rewriting it
    /// would have to invent a meaning for it.
    pub fn optimize(&self, roots: &[u32]) -> (Program, Vec<Option<u32>>) {
        let n = self.len();
        let nc = self.consts.len();
        if !self.is_well_formed() || roots.iter().any(|&r| r as usize >= n) {
            return (self.clone(), (0..n as u32).map(Some).collect());
        }
        // Every slot's value, where it is a compile-time constant.
        let mut known: Vec<Option<Val>> = self.consts.iter().copied().map(Some).collect();
        known.resize(n, None);
        let ctx = ShadeCtx {
            uv: (0.0, 0.0),
            normal: Vec3A::Z,
            tangent: Vec3A::ZERO,
            view: -Vec3A::Z,
            position: Vec3A::ZERO,
            uv_width: 0.0,
        };
        let mut scratch: Vec<Val> = vec![Val::ZERO; n];
        for (i, op) in self.ops.iter().enumerate() {
            let slot = nc + i;
            let mut all_known = op.is_pure();
            op.clone()
                .for_each_operand(|o| match known.get(*o as usize).copied().flatten() {
                    Some(v) => scratch[*o as usize] = v,
                    None => all_known = false,
                });
            if all_known {
                // `apply` reads operands by slot, so hand it the known values
                // at their own indices.
                known[slot] = Some(apply(op, &scratch[..slot], &ctx));
            }
        }

        // Liveness, from the roots back.
        let mut live = vec![false; n];
        for &r in roots {
            if let Some(l) = live.get_mut(r as usize) {
                *l = true;
            }
        }
        for i in (0..self.ops.len()).rev() {
            let slot = nc + i;
            if live[slot] && known[slot].is_none() {
                self.ops[i].clone().for_each_operand(|o| {
                    if let Some(l) = live.get_mut(*o as usize) {
                        *l = true;
                    }
                });
            }
        }

        // Constants first, deduplicated bitwise, then the surviving ops.
        let mut out = Program::default();
        let mut remap: Vec<Option<u32>> = vec![None; n];
        let key = |v: &Val| (v.v.map(f32::to_bits), v.arity);
        let mut seen: std::collections::HashMap<([u32; 4], u8), u32> = Default::default();
        for slot in 0..n {
            if let (true, Some(v)) = (live[slot], known[slot]) {
                let idx = *seen.entry(key(&v)).or_insert_with(|| {
                    out.consts.push(v);
                    (out.consts.len() - 1) as u32
                });
                remap[slot] = Some(idx);
            }
        }
        let base = out.consts.len();
        for (i, op) in self.ops.iter().enumerate() {
            let slot = nc + i;
            if live[slot] && known[slot].is_none() {
                let mut op = op.clone();
                op.for_each_operand(|o| {
                    // A live op's operands are live, so they were placed.
                    *o = remap[*o as usize].unwrap_or(0);
                });
                remap[slot] = Some((base + out.ops.len()) as u32);
                out.ops.push(op);
            }
        }
        (out, remap)
    }
}
