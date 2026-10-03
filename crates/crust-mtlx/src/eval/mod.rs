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
mod colorcorrect;
mod compiler;
mod op;
#[cfg(test)]
mod tests;

pub use apply::{perturb_normal, reflectivity_from_ior};
pub use compiler::Compiler;
pub use op::{BinOp, Op, UnOp};

use crate::value::Val;
use apply::apply;
use glam::Vec3A;

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
    ///   here — by the interpreter's own step ([`Program::apply_op`]), so the
    ///   value is the one every hit would have computed, bit for bit.
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
