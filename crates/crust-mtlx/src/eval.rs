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

use super::parse::{Doc, Input, Node, Source};
use super::value::{Val, arity_of};
use crate::texture::TextureRef;
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
        /// widths (see [`shifted_uv`]). Zero everywhere except the copies of
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
    ///   here — by [`apply`], the function the interpreter runs, so the value
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

/// One instruction's value, given the slots computed before it.
///
/// Shared by [`Program::eval`] and the constant folder in
/// [`Program::optimize`], which is what makes a folded constant exactly the
/// value the interpreter would have produced.
#[inline(always)]
fn apply(op: &Op, slots: &[Val], ctx: &ShadeCtx) -> Val {
    // Every operand index was emitted before this instruction, so the slot
    // exists. `get` rather than indexing keeps a malformed program from
    // panicking inside the integrator.
    let g = |i: u32| -> Val { slots.get(i as usize).copied().unwrap_or(Val::ZERO) };
    match op {
        Op::Const(v) => *v,
        Op::Texture {
            tex,
            fallback,
            scale,
            offset,
            arity,
            shift,
            coord,
        } => match tex {
            Some(t) => {
                let (u, v) = match coord {
                    Some(c) => {
                        let c = g(*c);
                        // A `float` coordinate is both axes, as MaterialX's
                        // implicit promotion to `vector2` makes it.
                        if c.arity == 1 {
                            (c.x(), c.x())
                        } else {
                            (c.v[0], c.v[1])
                        }
                    }
                    None => shifted_uv(ctx, *shift),
                };
                let u = u * scale[0] + offset[0];
                let v = v * scale[1] + offset[1];
                // `uvtiling` scales the coordinates, so it scales the
                // footprint with them: a texture tiled 10× is being
                // minified 10× and must read a coarser level to match.
                // The two axes are averaged because the width is one
                // isotropic number.
                let w = ctx.uv_width * 0.5 * (scale[0].abs() + scale[1].abs());
                let rgba = t.eval(u, v, w);
                Val {
                    v: rgba,
                    arity: *arity,
                }
            }
            None => *fallback,
        },
        Op::TexCoord { shift } => {
            let (u, v) = shifted_uv(ctx, *shift);
            Val::vec2(u, v)
        }
        Op::Normal => ctx.normal.into(),
        Op::ViewDirection => ctx.view.into(),
        Op::Position => ctx.position.into(),
        Op::Unary { op, a } => {
            let a = g(*a);
            match op {
                UnOp::Abs => a.map(f32::abs),
                // Guarded so a zero or negative operand — which the
                // teapot's Beer-Lambert chain can produce from a
                // black texel — yields a finite value instead of an
                // infinity that then poisons every downstream lane.
                // The guard is OSL's `safe_log`, which the MaterialX
                // reference runs: the operand is raised to the smallest
                // normal float, so `ln(0)` is `ln(f32::MIN_POSITIVE)`.
                UnOp::Ln => a.map(|x| x.max(f32::MIN_POSITIVE).ln()),
                UnOp::Exp => a.map(|x| x.clamp(-88.0, 88.0).exp()),
                UnOp::Sin => a.map(f32::sin),
                UnOp::Cos => a.map(f32::cos),
                UnOp::Asin => a.map(|x| x.clamp(-1.0, 1.0).asin()),
                UnOp::Acos => a.map(|x| x.clamp(-1.0, 1.0).acos()),
                UnOp::Sqrt => a.map(|x| x.max(0.0).sqrt()),
                // Not `f32::signum`, which answers ±1 for ±0: MaterialX's
                // `sign` of zero is zero.
                UnOp::Sign => a.map(|x| {
                    if x > 0.0 {
                        1.0
                    } else if x < 0.0 {
                        -1.0
                    } else {
                        x
                    }
                }),
                UnOp::Floor => a.map(f32::floor),
                UnOp::Ceil => a.map(f32::ceil),
                UnOp::Normalize => normalize(a),
            }
        }
        Op::Binary { op, a, b } => {
            let (a, b) = (g(*a), g(*b));
            match op {
                BinOp::Add => a + b,
                BinOp::Sub => a - b,
                BinOp::Mul => a * b,
                // A zero divisor is a real possibility in these
                // graphs (`1 / transmittance` with a black channel),
                // and an infinity survives every later multiply.
                BinOp::Div => a.zip(b, |x, y| if y.abs() > 1e-20 { x / y } else { 0.0 }),
                BinOp::Pow => a.zip(b, safe_pow),
                BinOp::Min => a.zip(b, f32::min),
                BinOp::Max => a.zip(b, f32::max),
                BinOp::Modulo => a.zip(b, floored_mod),
            }
        }
        // `bg·(1−m) + fg·m`. Written as two weighted terms rather
        // than `bg + (fg−bg)·m` so that a `float` mix against wider
        // operands broadcasts through `zip`'s promotion in both
        // terms alike.
        Op::Mix { fg, bg, m } => {
            let (fg, bg, m) = (g(*fg), g(*bg), g(*m));
            let inv = m.map(|x| 1.0 - x);
            bg * inv + fg * m
        }
        // `max(min(in, high), low)`, OSL's order: it only shows when
        // `low > high`, where `low` wins.
        Op::Clamp { a, low, high } => {
            let (a, lo, hi) = (g(*a), g(*low), g(*high));
            a.zip(hi, f32::min).zip(lo, f32::max)
        }
        Op::Contrast { a, amount, pivot } => {
            let (a, amt, piv) = (g(*a), g(*amount), g(*pivot));
            (a - piv) * amt + piv
        }
        Op::Remap {
            a,
            in_low,
            in_high,
            out_low,
            out_high,
        } => {
            let (a, il, ih, ol, oh) = (g(*a), g(*in_low), g(*in_high), g(*out_low), g(*out_high));
            let t = (a - il).zip(ih - il, |x, d| if d.abs() > 1e-20 { x / d } else { 0.0 });
            ol + (oh - ol) * t
        }
        Op::Invert { a, amount } => g(*amount) - g(*a),
        Op::Convert { a, arity } => convert(g(*a), *arity),
        Op::Extract { a, index } => Val::float(g(*a).v[(*index).min(3)]),
        Op::Combine3 { a, b, c } => Val::vec3(g(*a).x(), g(*b).x(), g(*c).x()),
        Op::Combine2 { a, b } => combine2(g(*a), g(*b)),
        Op::DotProduct { a, b } => dot(g(*a), g(*b)),
        Op::Luminance { a, coeffs } => {
            let a = g(*a);
            let l = a.rgb().dot(g(*coeffs).rgb());
            // The input's width: `color3` is the grey `(l, l, l)`, and
            // `color4` keeps its alpha.
            if a.arity == 4 {
                Val::vec4(l, l, l, a.v[3])
            } else {
                Val::float(l).broadcast_to(a.arity)
            }
        }
        Op::NormalMap { a, scale } => normal_map(g(*a), g(*scale), ctx).into(),
        Op::ArtisticIor {
            reflectivity,
            edge,
            extinction,
        } => {
            let (n, k) = artistic_ior(g(*reflectivity).rgb(), g(*edge).rgb());
            if *extinction { k.into() } else { n.into() }
        }
        Op::Smoothstep { a, low, high } => {
            let (a, lo, hi) = (g(*a), g(*low), g(*high));
            let n = a.arity.max(lo.arity).max(hi.arity);
            let (a, lo, hi) = (a.broadcast_to(n), lo.broadcast_to(n), hi.broadcast_to(n));
            Val {
                v: [0, 1, 2, 3].map(|i| smoothstep(a.v[i], lo.v[i], hi.v[i])),
                arity: n,
            }
        }
        Op::HsvAdjust { a, amount } => {
            let hsv = rgb_to_hsv(g(*a).rgb());
            let m = g(*amount).rgb();
            hsv_to_rgb(Vec3A::new(hsv.x + m.x, hsv.y * m.y, hsv.z * m.z)).into()
        }
        Op::HeightToNormal {
            xp,
            xm,
            yp,
            ym,
            scale,
        } => height_to_normal(
            g(*xp).x() - g(*xm).x(),
            g(*yp).x() - g(*ym).x(),
            g(*scale).x(),
        )
        .into(),
    }
}

/// The chart coordinates `shift` footprint widths away from the shading
/// point.
///
/// A zero shift returns `ctx.uv` untouched rather than adding `0 · width`,
/// which keeps every ordinary lookup bit-identical to what it was before
/// shifts existed (and to the JIT's inline texture path, which never sees a
/// shifted op). A shift over a zero footprint — no ray cone — lands back on
/// the shading point, so a derivative taken from shifted copies reads zero.
#[inline]
fn shifted_uv(ctx: &ShadeCtx, shift: [f32; 2]) -> (f32, f32) {
    if shift == [0.0, 0.0] {
        ctx.uv
    } else {
        (
            ctx.uv.0 + shift[0] * ctx.uv_width,
            ctx.uv.1 + shift[1] * ctx.uv_width,
        )
    }
}

/// MaterialX's OSL `mx_heighttonormal_vector3`, given the height's change
/// across one footprint along `u` (`du`) and `v` (`dv`).
///
/// The reference reads `dx = -Dx(in)`, `dy = Dy(in)`: screen-space
/// derivatives, i.e. the height's change across one pixel. The footprint is
/// this renderer's pixel (a ray cone's width in chart units), so the change
/// across it is the same quantity, with `Dx` running along `u`. Raster `y`
/// runs *down* while `v` runs up, so `Dy(in) = -dv` and both lateral
/// components come out as `-dh`: the normal of a surface raised by `in`.
/// That also makes the result resolution-dependent exactly as the
/// reference's is — a bump reads steeper the coarser the footprint.
fn height_to_normal(du: f32, dv: f32, scale: f32) -> Vec3A {
    let (dx, dy) = (-du, -dv);
    let dz = scale.max(1.0e-5) * (1.0 - dx * dx - dy * dy).max(1.0e-5).sqrt();
    Vec3A::new(dx, dy, dz).normalize_or(Vec3A::Z) * 0.5 + Vec3A::splat(0.5)
}

/// MaterialX's `mx_rgbtohsv` (Foley & van Dam, via OSL), transcribed.
fn rgb_to_hsv(c: Vec3A) -> Vec3A {
    let (r, g, b) = (c.x, c.y, c.z);
    let min = r.min(g.min(b));
    let max = r.max(g.max(b));
    let delta = max - min;
    let s = if max > 0.0 { delta / max } else { 0.0 };
    let h = if s <= 0.0 {
        0.0
    } else {
        let h = if r >= max {
            (g - b) / delta
        } else if g >= max {
            2.0 + (b - r) / delta
        } else {
            4.0 + (r - g) / delta
        } * (1.0 / 6.0);
        if h < 0.0 { h + 1.0 } else { h }
    };
    Vec3A::new(h, s, max)
}

/// MaterialX's `mx_hsvtorgb`, transcribed. The hue wraps, so a hue shift
/// past 1 comes round again.
fn hsv_to_rgb(hsv: Vec3A) -> Vec3A {
    let (h, s, v) = (hsv.x, hsv.y, hsv.z);
    if s < 0.0001 {
        return Vec3A::splat(v);
    }
    let h = 6.0 * (h - h.floor());
    // `h` is in [0, 6) up to rounding; a non-finite hue lands in the last
    // sextant rather than anywhere undefined.
    let hi = h.trunc();
    let f = h - hi;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match hi as i32 {
        0 => Vec3A::new(v, t, p),
        1 => Vec3A::new(q, v, p),
        2 => Vec3A::new(p, v, t),
        3 => Vec3A::new(p, q, v),
        4 => Vec3A::new(t, p, v),
        _ => Vec3A::new(v, p, q),
    }
}

/// MaterialX `normalize`, over the value's own lanes. A zero-length input is
/// returned unchanged rather than divided into NaNs. A `float` broadcasts to a
/// `vector3`, as it always did here (MaterialX has no `float` variant).
fn normalize(a: Val) -> Val {
    match a.arity {
        4 => {
            let v = glam::Vec4::from_array(a.v);
            let n = v.length();
            if n > 1e-20 {
                let [x, y, z, w] = (v / n).to_array();
                Val::vec4(x, y, z, w)
            } else {
                a
            }
        }
        arity => {
            // Lanes past a `vector2`'s are whatever the op that made it left
            // there, so they are zeroed before they can enter the length.
            let v = if arity == 2 {
                Vec3A::new(a.v[0], a.v[1], 0.0)
            } else {
                a.rgb()
            };
            let n = v.length();
            if n > 1e-20 {
                let r = v / n;
                if arity == 2 {
                    Val::vec2(r.x, r.y)
                } else {
                    r.into()
                }
            } else {
                a
            }
        }
    }
}

/// MaterialX `dotproduct` over the operands' lanes. The `vector3` sum is
/// glam's, as it always was; the others extend it. (A `float` operand's
/// lanes all hold its value, so reading them broadcasts it.)
fn dot(a: Val, b: Val) -> Val {
    let n = a.arity.max(b.arity);
    let lanes = |v: Val| match n {
        // A `vector2`'s third lane is not its own; see `normalize`.
        2 => Vec3A::new(v.v[0], v.v[1], 0.0),
        _ => v.rgb(),
    };
    let d = lanes(a).dot(lanes(b));
    Val::float(if n == 4 { d + a.v[3] * b.v[3] } else { d })
}

/// MaterialX `convert`. A `float` broadcasts. Widening a wider value fills
/// the new lanes the way the nodedefs do — zero, except that a `color4` /
/// `vector4` made from fewer lanes gets `1` in its last (an opaque alpha).
/// Narrowing keeps the leading lanes; to a `float`, the first.
fn convert(a: Val, arity: u8) -> Val {
    let arity = arity.clamp(1, 4);
    if a.arity == 1 {
        return a.with_arity(arity);
    }
    if arity == 1 {
        // Every lane of a `float` holds its value; see `Val::float`.
        return Val::float(a.v[0]);
    }
    let mut v = a.v;
    for (i, lane) in v.iter_mut().enumerate().skip(a.arity as usize) {
        *lane = if i == 3 { 1.0 } else { 0.0 };
    }
    Val { v, arity }
}

/// MaterialX `combine2`: the lanes of `a`, then of `b`. Covers every
/// signature — `(float, float)` → `vector2`, `(color3, float)` → `color4`,
/// `(vector3, float)` and `(vector2, vector2)` → `vector4`.
fn combine2(a: Val, b: Val) -> Val {
    let mut v = [0.0; 4];
    let (na, nb) = (a.arity as usize, b.arity as usize);
    v[..na].copy_from_slice(&a.v[..na]);
    let nb = nb.min(4 - na.min(4));
    v[na..na + nb].copy_from_slice(&b.v[..nb]);
    Val {
        v,
        arity: (na + nb) as u8,
    }
}

/// MaterialX's `modulo`, OSL's `mod`: floored, so the result takes the
/// divisor's sign (`-0.2 mod 1` is `0.8`) where Rust's `%` truncates, and a
/// zero divisor returns the dividend, as OSL's does.
///
/// OSL's `x − y·floor(x / y)` is kept wherever its quotient is finite, so
/// the rounding matches the reference (at `-1 mod -0.2` the quotient rounds
/// to 5 and the result to 0, a period away from the exact −0.19999999). Its
/// quotient overflows for some finite operands, though (`1 mod 1e-40` would
/// be `−inf`), and there the exact remainder `%` takes over, moved by one `y`
/// when its sign is the dividend's rather than the divisor's.
fn floored_mod(x: f32, y: f32) -> f32 {
    if y == 0.0 {
        return x;
    }
    let q = (x / y).floor();
    if q.is_finite() {
        return x - y * q;
    }
    let r = x % y;
    if r != 0.0 && (r < 0.0) != (y < 0.0) {
        r + y
    } else {
        r
    }
}

/// OSL's `pow` (OIIO `safe_pow`), which MaterialX's `power` is: `x^0` is one,
/// `0^y` zero, a negative base takes only integer exponents (zero otherwise),
/// and the result is clamped finite.
fn safe_pow(x: f32, y: f32) -> f32 {
    if y == 0.0 {
        return 1.0;
    }
    if x == 0.0 {
        return 0.0;
    }
    if x < 0.0 && y != y.floor() {
        return 0.0;
    }
    x.powf(y).clamp(-f32::MAX, f32::MAX)
}

/// OSL's `smoothstep(low, high, x)`, which MaterialX's is: zero below `low`,
/// one from `high` up, the Hermite ramp between. With `low >= high` the first
/// two tests decide every `x`, so no division by the empty interval happens.
fn smoothstep(x: f32, low: f32, high: f32) -> f32 {
    if x < low {
        0.0
    } else if x >= high {
        1.0
    } else {
        let t = (x - low) / (high - low);
        t * t * (3.0 - 2.0 * t)
    }
}

/// MaterialX `normalmap`: decode `[0,1]`-encoded tangent-space vector, scale
/// its lateral components, and rotate it into world space.
///
/// Falls back to the geometric normal when there is no tangent — the host
/// says when that happens (in crust, only baked single-placement geometry
/// carries one; see crust-core's `UvMap::tangents`). Returning the
/// geometric normal is the right degradation: a normal map's *mean* is the
/// surface normal, so the flat surface is the map's own zero.
fn normal_map(encoded: Val, scale: Val, ctx: &ShadeCtx) -> Vec3A {
    let v = encoded.rgb() * 2.0 - Vec3A::ONE;
    // `scale` is a `float` or, per axis, a `vector2`.
    let (sx, sy) = if scale.arity >= 2 {
        (scale.v[0], scale.v[1])
    } else {
        (scale.x(), scale.x())
    };
    let finite = |s: f32| if s.is_finite() { s } else { 1.0 };
    let local = Vec3A::new(v.x * finite(sx), v.y * finite(sy), v.z.max(1e-4));
    perturb_normal(local, ctx.normal, ctx.tangent)
}

/// Rotates a **decoded** tangent-space normal (`z` along `normal`) into world
/// space, against `tangent` re-orthogonalised to `normal`.
///
/// The half of [`normal_map`] that knows nothing about MaterialX's `[0,1]`
/// encoding, public so a host with its own decode — UsdPreviewSurface's
/// `normal` input arrives already in `[-1,1]`, its UsdUVTexture's
/// `scale`/`bias` having done the decode — rotates it identically. Returns
/// `normal` unchanged when `tangent` is zero (no chart frame) or parallel to
/// it, for the reason [`normal_map`] gives.
pub fn perturb_normal(local: Vec3A, normal: Vec3A, tangent: Vec3A) -> Vec3A {
    if tangent.length_squared() < 1e-20 {
        return normal;
    }
    let n = normal;
    // Re-orthogonalise: the stored tangent is the triangle's, while `n` may
    // already carry interpolated shading curvature, so the two need not be
    // perpendicular.
    let t = (tangent - n * n.dot(tangent)).normalize_or_zero();
    if t.length_squared() < 1e-20 {
        return n;
    }
    let b = n.cross(t);
    let world = t * local.x + b * local.y + n * local.z;
    if world.length_squared() > 1e-20 {
        world.normalize()
    } else {
        n
    }
}

/// Gulbrandsen's "Artist Friendly Metallic Fresnel": normal-incidence
/// reflectivity and grazing edge tint → complex IOR.
///
/// Implemented rather than short-circuited (the conductor lobe wants a
/// reflectivity back, which is what went in) because a graph may author `ior`
/// and `extinction` directly, and the round trip through
/// [`reflectivity_from_ior`] then handles both authorings with one path.
fn artistic_ior(reflectivity: Vec3A, edge: Vec3A) -> (Vec3A, Vec3A) {
    let r = reflectivity.clamp(Vec3A::ZERO, Vec3A::splat(0.99));
    let rs = Vec3A::new(r.x.sqrt(), r.y.sqrt(), r.z.sqrt());
    let n_min = (Vec3A::ONE - r) / (Vec3A::ONE + r);
    let n_max = (Vec3A::ONE + rs) / (Vec3A::ONE - rs).max(Vec3A::splat(1e-6));
    // OSL's `mix(n_max, n_min, edge)`, as `x·(1 − t) + y·t`: an edge colour
    // of white — the default — selects `n_min` exactly, where `x + (y − x)·t`
    // would leave `n_max`'s rounding in it (n_max is ~70 for a bright metal).
    // Clamped, unlike the reference: an edge tint outside [0, 1] extrapolates
    // the IOR to nonsense, down to negative values.
    let e = edge.clamp(Vec3A::ZERO, Vec3A::ONE);
    let n = n_max * (Vec3A::ONE - e) + n_min * e;
    let np1 = n + Vec3A::ONE;
    let nm1 = n - Vec3A::ONE;
    let k2 =
        ((np1 * np1 * r - nm1 * nm1) / (Vec3A::ONE - r).max(Vec3A::splat(1e-6))).max(Vec3A::ZERO);
    (n, Vec3A::new(k2.x.sqrt(), k2.y.sqrt(), k2.z.sqrt()))
}

/// Normal-incidence reflectivity of a conductor with complex IOR `n + ik`.
///
/// The exact inverse of `artistic_ior` (the node's implementation above), which
/// is what lets a conductor lobe
/// be reduced to the one colour OpenPBR's metal lobe takes, whichever way the
/// graph authored it.
pub fn reflectivity_from_ior(n: Vec3A, k: Vec3A) -> Vec3A {
    let num = (n - Vec3A::ONE) * (n - Vec3A::ONE) + k * k;
    let den = ((n + Vec3A::ONE) * (n + Vec3A::ONE) + k * k).max(Vec3A::splat(1e-6));
    (num / den).clamp(Vec3A::ZERO, Vec3A::ONE)
}

// ---------------------------------------------------------------------------
// Compilation
// ---------------------------------------------------------------------------

/// `luminance`'s default `lumacoeffs`: ACEScg's (AP1) weights.
const AP1_LUMA_COEFFS: Val = Val::vec3(0.2722287, 0.6740818, 0.0536895);

/// Turns named `.mtlx` nodes into a topologically ordered [`Program`].
pub struct Compiler<'a> {
    pub doc: &'a Doc,
    pub program: Program,
    /// Slot already emitted for a `(graph, node, output, shift)` key, so a
    /// node feeding five others is evaluated once. The output name matters:
    /// `artistic_ior` emits a different slot for `ior` than for `extinction`.
    /// So does the shift: a node under `heighttonormal` is compiled once per
    /// offset it is differentiated at.
    memo: std::collections::HashMap<(String, String, String, [u32; 2]), u32>,
    /// The offset, in footprint widths, every texture lookup and `texcoord`
    /// compiled now is displaced by — zero except while `heighttonormal`
    /// compiles the shifted copies of its input.
    uv_shift: [f32; 2],
    /// What the loader answered per `(file, colorspace)`, so the shifted
    /// copies of an `image` share the one sampler rather than asking the host
    /// again.
    images: std::collections::HashMap<(String, Option<String>), Option<TextureRef>>,
    /// Nodes currently being compiled, so a cyclic document — which a
    /// hand-edited `.mtlx` can be — terminates as a constant rather than
    /// recursing until the stack runs out.
    active: Vec<String>,
    /// Resolves an `image` node's `file` input to a sampler.
    loader: crate::TextureLoader<'a>,
    /// Node categories met that this compiler has no operator for, for one
    /// summary warning instead of one per occurrence.
    pub unsupported: std::collections::BTreeSet<String>,
}

impl<'a> Compiler<'a> {
    pub fn new(doc: &'a Doc, loader: crate::TextureLoader<'a>) -> Compiler<'a> {
        Compiler {
            doc,
            program: Program::default(),
            memo: std::collections::HashMap::new(),
            uv_shift: [0.0, 0.0],
            images: std::collections::HashMap::new(),
            active: Vec::new(),
            loader,
            unsupported: Default::default(),
        }
    }

    pub fn emit(&mut self, op: Op) -> u32 {
        self.program.ops.push(op);
        (self.program.ops.len() - 1) as u32
    }

    pub fn constant(&mut self, v: Val) -> u32 {
        self.emit(Op::Const(v))
    }

    /// `luminance` at the nodedef's default `lumacoeffs`, ACEScg's (AP1) —
    /// what the stdlib nodegraphs' unauthored `luminance` / `saturate` read.
    pub fn luminance(&mut self, a: u32) -> u32 {
        let coeffs = self.constant(AP1_LUMA_COEFFS);
        self.emit(Op::Luminance { a, coeffs })
    }

    /// The value of `slot` when it is a compile-time constant — a literal, or
    /// a pure operator over constants — and `None` when it depends on the
    /// shading point or a texture.
    ///
    /// What the closure builders prune on: a branch whose weight folds to a
    /// literal zero, or a mix whose factor folds to exactly 0 or 1, is left
    /// out of the tree altogether rather than evaluated at every vertex to be
    /// told it contributes nothing.
    pub fn fold(&self, slot: u32) -> Option<Val> {
        let ops = &self.program.ops;
        let op = ops.get(slot as usize)?;
        if let Op::Const(v) = op {
            return Some(*v);
        }
        if !op.is_pure() {
            return None;
        }
        let mut operands = Vec::new();
        op.clone().for_each_operand(|o| operands.push(*o));
        let mut slots = vec![Val::ZERO; slot as usize];
        for o in operands {
            slots[o as usize] = self.fold(o)?;
        }
        let ctx = ShadeCtx {
            uv: (0.0, 0.0),
            normal: Vec3A::Z,
            tangent: Vec3A::X,
            view: -Vec3A::Z,
            position: Vec3A::ZERO,
            uv_width: 0.0,
        };
        Some(apply(op, &slots, &ctx))
    }

    /// Compiles the value feeding `input` of `node`, or `default` when the
    /// input is unauthored.
    pub fn input_or(&mut self, node: &Node, name: &str, default: Val) -> u32 {
        match node.input(name) {
            Some(i) => self.compile_input(node, i),
            // The nodedef's default is typed: an unauthored `in1` of an
            // `add_color3` is a colour3 zero, not a `float` one, and an op over
            // nothing but defaults must come out at the node's width. A
            // `float` default's lanes already hold its value, so widening it
            // changes no lane — only what a width-reading consumer
            // (`luminance`, `normalize`, `dotproduct`, `combine2`) sees. Not
            // `convert`, whose input width is what decides the alpha.
            None if default.arity == 1 && node.category != "convert" => {
                self.constant(default.broadcast_to(arity_of(&node.type_name)))
            }
            None => self.constant(default),
        }
    }

    /// Like [`Compiler::input_or`] but reports whether the input was authored,
    /// which the BSDF reducer needs to tell "no normal map" from "a normal map
    /// that happens to be flat".
    pub fn optional_input(&mut self, node: &Node, name: &str) -> Option<u32> {
        let i = node.input(name)?;
        Some(self.compile_input(node, i))
    }

    /// The width an authored input carries: its producer's declared type
    /// when it is connected, since the parser reads an input with no `type`
    /// attribute as a `float` whatever feeds it, and the input's own type for
    /// a literal. A multioutput producer's outputs are not typed in the
    /// document, so there the input's declaration is all there is.
    fn input_arity(&self, node: &Node, input: &Input) -> u8 {
        let scope = node.graph.clone().unwrap_or_default();
        let producer = match &input.source {
            Source::Node { name, .. } => self.doc.find(&scope, name),
            Source::Graph { graph, output } => self.doc.graph_output(graph, output).map(|c| c.node),
            Source::Value(_) => None,
        };
        match producer {
            Some(p) if p.type_name != "multioutput" => arity_of(&p.type_name),
            _ => arity_of(&input.type_name),
        }
    }

    fn compile_input(&mut self, node: &Node, input: &Input) -> u32 {
        let scope = node.graph.clone().unwrap_or_default();
        match &input.source {
            Source::Value(v) => {
                // Re-arity against the *input's* declared type: a literal
                // "0.5" on a color3 input must broadcast, which the parser
                // already arranged, but a literal parsed at another width
                // should not silently narrow the operator.
                let _ = arity_of(&input.type_name);
                self.constant(*v)
            }
            Source::Node { name, output } => self.compile_named(&scope, name, output.as_deref()),
            Source::Graph { graph, output } => match self.doc.graph_output(graph, output) {
                Some(conn) => {
                    // The graph's `<output>` may itself select one output of a
                    // multioutput node; carry it through, or `extinction`
                    // silently compiles as `ior`.
                    let (g, nm) = (
                        conn.node.graph.clone().unwrap_or_default(),
                        conn.node.name.clone(),
                    );
                    let sel = conn.output.map(str::to_string);
                    self.compile_named(&g, &nm, sel.as_deref())
                }
                None => self.constant(Val::ZERO),
            },
        }
    }

    /// Compiles the node called `name`, returning its slot.
    pub fn compile_named(&mut self, scope: &str, name: &str, output: Option<&str>) -> u32 {
        let key = (
            scope.to_string(),
            name.to_string(),
            output.unwrap_or("").to_string(),
            self.uv_shift.map(f32::to_bits),
        );
        if let Some(&slot) = self.memo.get(&key) {
            return slot;
        }
        if self.active.iter().any(|a| a == name) {
            // A cycle. Break it with a constant — the alternative is a stack
            // overflow at import on a malformed document.
            return self.constant(Val::ZERO);
        }
        let Some(node) = self.doc.find(scope, name) else {
            return self.constant(Val::ZERO);
        };
        let node = node.clone();
        self.active.push(name.to_string());
        let slot = self.compile_node(&node, output);
        self.active.pop();
        self.memo.insert(key, slot);
        slot
    }

    fn compile_node(&mut self, node: &Node, output: Option<&str>) -> u32 {
        let arity = arity_of(&node.type_name);
        let bin = |c: &mut Self, op: BinOp, d1: Val, d2: Val| {
            let a = c.input_or(node, "in1", d1);
            let b = c.input_or(node, "in2", d2);
            c.emit(Op::Binary { op, a, b })
        };
        let un = |c: &mut Self, op: UnOp| {
            let a = c.input_or(node, "in", Val::ZERO);
            c.emit(Op::Unary { op, a })
        };
        match node.category.as_str() {
            "constant" => self.input_or(node, "value", Val::ZERO),
            "image" | "tiledimage" => self.compile_image(node, arity),
            "texcoord" => self.emit(Op::TexCoord {
                shift: self.uv_shift,
            }),
            "normal" => self.emit(Op::Normal),
            "viewdirection" => self.emit(Op::ViewDirection),
            "position" => self.emit(Op::Position),
            "add" => bin(self, BinOp::Add, Val::ZERO, Val::ZERO),
            "subtract" => bin(self, BinOp::Sub, Val::ZERO, Val::ZERO),
            // The unauthored defaults are the nodedefs': `in1` is zero and
            // `in2` one for every operator whose identity is one.
            "multiply" => bin(self, BinOp::Mul, Val::ZERO, Val::ONE),
            "divide" => bin(self, BinOp::Div, Val::ZERO, Val::ONE),
            "power" => bin(self, BinOp::Pow, Val::ZERO, Val::ONE),
            "min" => bin(self, BinOp::Min, Val::ZERO, Val::ZERO),
            "max" => bin(self, BinOp::Max, Val::ZERO, Val::ZERO),
            "modulo" => bin(self, BinOp::Modulo, Val::ZERO, Val::ONE),
            "absval" => un(self, UnOp::Abs),
            "ln" => {
                let a = self.input_or(node, "in", Val::ONE);
                self.emit(Op::Unary { op: UnOp::Ln, a })
            }
            "exp" => un(self, UnOp::Exp),
            "sin" => un(self, UnOp::Sin),
            "cos" => un(self, UnOp::Cos),
            "asin" => un(self, UnOp::Asin),
            "acos" => un(self, UnOp::Acos),
            "sqrt" => un(self, UnOp::Sqrt),
            "sign" => un(self, UnOp::Sign),
            "floor" => un(self, UnOp::Floor),
            "ceil" => un(self, UnOp::Ceil),
            "normalize" => un(self, UnOp::Normalize),
            "luminance" => {
                let a = self.input_or(node, "in", Val::ZERO);
                // The nodedef's default: ACEScg's (AP1) coefficients.
                let coeffs = self.input_or(node, "lumacoeffs", AP1_LUMA_COEFFS);
                self.emit(Op::Luminance { a, coeffs })
            }
            "dotproduct" => {
                let a = self.input_or(node, "in1", Val::ZERO);
                let b = self.input_or(node, "in2", Val::ZERO);
                self.emit(Op::DotProduct { a, b })
            }
            "mix" => {
                let fg = self.input_or(node, "fg", Val::ZERO);
                let bg = self.input_or(node, "bg", Val::ZERO);
                let m = self.input_or(node, "mix", Val::ZERO);
                self.emit(Op::Mix { fg, bg, m })
            }
            "clamp" => {
                let a = self.input_or(node, "in", Val::ZERO);
                let low = self.input_or(node, "low", Val::ZERO);
                let high = self.input_or(node, "high", Val::ONE);
                self.emit(Op::Clamp { a, low, high })
            }
            "contrast" => {
                let a = self.input_or(node, "in", Val::ZERO);
                let amount = self.input_or(node, "amount", Val::ONE);
                let pivot = self.input_or(node, "pivot", Val::float(0.5));
                self.emit(Op::Contrast { a, amount, pivot })
            }
            "remap" => {
                let a = self.input_or(node, "in", Val::ZERO);
                let in_low = self.input_or(node, "inlow", Val::ZERO);
                let in_high = self.input_or(node, "inhigh", Val::ONE);
                let out_low = self.input_or(node, "outlow", Val::ZERO);
                let out_high = self.input_or(node, "outhigh", Val::ONE);
                self.emit(Op::Remap {
                    a,
                    in_low,
                    in_high,
                    out_low,
                    out_high,
                })
            }
            "invert" => {
                let a = self.input_or(node, "in", Val::ZERO);
                let amount = self.input_or(node, "amount", Val::ONE);
                self.emit(Op::Invert { a, amount })
            }
            "smoothstep" => {
                let a = self.input_or(node, "in", Val::ZERO);
                let low = self.input_or(node, "low", Val::ZERO);
                let high = self.input_or(node, "high", Val::ONE);
                self.emit(Op::Smoothstep { a, low, high })
            }
            "convert" => {
                let a = self.input_or(node, "in", Val::ZERO);
                self.emit(Op::Convert { a, arity })
            }
            "extract" => {
                let a = self.input_or(node, "in", Val::ZERO);
                // `index` is an integer literal, so it is read off the raw
                // text rather than through the float lanes.
                let index = node
                    .input("index")
                    .and_then(|i| i.text.as_deref())
                    .and_then(|t| t.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                self.emit(Op::Extract { a, index })
            }
            "combine2" => {
                // The signature fixes each operand's width — `(float, float)`
                // → `vector2`, `(color3, float)` → `color4`, and for a
                // `vector4` `(vector3, float)` or `(vector2, vector2)`, told
                // apart by either input's width (see `Compiler::input_arity`).
                // Each operand is converted to its width first, so the
                // concatenation is exact whatever produced it (an unauthored
                // `in1` is a zero, which must still fill three lanes of a
                // `color4`).
                let declared = |n: &str| node.input(n).map(|i| self.input_arity(node, i));
                let (na, nb) = match arity {
                    4 if declared("in1") == Some(2) || declared("in2") == Some(2) => (2, 2),
                    4 => (3, 1),
                    _ => (1, 1),
                };
                let a = self.input_or(node, "in1", Val::ZERO);
                let a = self.emit(Op::Convert { a, arity: na });
                let b = self.input_or(node, "in2", Val::ZERO);
                let b = self.emit(Op::Convert { a: b, arity: nb });
                self.emit(Op::Combine2 { a, b })
            }
            "combine3" => {
                let a = self.input_or(node, "in1", Val::ZERO);
                let b = self.input_or(node, "in2", Val::ZERO);
                let c = self.input_or(node, "in3", Val::ZERO);
                self.emit(Op::Combine3 { a, b, c })
            }
            "normalmap" => {
                let a = self.input_or(node, "in", Val::vec3(0.5, 0.5, 1.0));
                let scale = self.input_or(node, "scale", Val::ONE);
                self.emit(Op::NormalMap { a, scale })
            }
            "artistic_ior" => {
                let reflectivity =
                    self.input_or(node, "reflectivity", Val::vec3(0.944, 0.776, 0.373));
                let edge = self.input_or(node, "edge_color", Val::vec3(0.998, 0.981, 0.751));
                self.emit(Op::ArtisticIor {
                    reflectivity,
                    edge,
                    extinction: output == Some("extinction"),
                })
            }
            "colorcorrect" => self.compile_colorcorrect(node),
            "heighttonormal" => {
                let scale = self.input_or(node, "scale", Val::ONE);
                // Half a footprint either side, so the difference spans one.
                let xp = self.shifted_input(node, "in", [0.5, 0.0]);
                let xm = self.shifted_input(node, "in", [-0.5, 0.0]);
                let yp = self.shifted_input(node, "in", [0.0, 0.5]);
                let ym = self.shifted_input(node, "in", [0.0, -0.5]);
                self.emit(Op::HeightToNormal {
                    xp,
                    xm,
                    yp,
                    ym,
                    scale,
                })
            }
            other => {
                self.unsupported.insert(other.to_string());
                // Degrade this input to mid-grey rather than to black: an
                // unsupported *pattern* node is usually a colour correction,
                // and zero would turn whatever it feeds into a hole.
                self.constant(Val::float(0.5))
            }
        }
    }

    /// `input` of `node` compiled with every lookup under it displaced by a
    /// further `shift` footprint widths.
    fn shifted_input(&mut self, node: &Node, input: &str, shift: [f32; 2]) -> u32 {
        let outer = self.uv_shift;
        self.uv_shift = [outer[0] + shift[0], outer[1] + shift[1]];
        let slot = self.input_or(node, input, Val::ZERO);
        self.uv_shift = outer;
        slot
    }

    /// MaterialX `colorcorrect` (`color3`), lowered to the stdlib's own
    /// `NG_colorcorrect_color3` chain: `hsvadjust` (hue) → `saturate` →
    /// `range` (gamma) → lift → gain → `contrast` → exposure.
    ///
    /// Expanded here rather than kept as one op so the stages reuse
    /// operators the JIT already inlines. A stage whose parameter folds to
    /// its identity (hue 0, saturation 1, …) is left out: the reference's
    /// arithmetic at those values is the identity up to rounding at most, and
    /// the playground's graphs author one or two of the eight inputs.
    fn compile_colorcorrect(&mut self, node: &Node) -> u32 {
        if node.type_name != "color3" {
            // `color4` routes alpha around the chain, and there is no
            // `combine4` to put it back with; say so rather than correct the
            // alpha too.
            self.unsupported
                .insert(format!("colorcorrect ({})", node.type_name));
            return self.constant(Val::float(0.5));
        }
        let input = self.input_or(node, "in", Val::vec3(1.0, 1.0, 1.0));
        // Promote a `float` input to the colour MaterialX makes of it — its
        // first lane, three times — before any stage runs. A one-lane texture
        // carries its file's other channels in the lanes above the first, and
        // the stages below work lane by lane, so without this they would
        // correct those hidden channels too and a later `convert` would
        // surface them as colour. `zip` broadcasts a one-lane operand from
        // lane 0 and multiplying by one is exact, so a `color3` input passes
        // through bit for bit.
        let ones = self.constant(Val::vec3(1.0, 1.0, 1.0));
        let mut c = self.emit(Op::Binary {
            op: BinOp::Mul,
            a: input,
            b: ones,
        });
        // Each parameter's slot, unless it folds to the stage's identity.
        let param = |cc: &mut Self, name: &str, default: f32| -> Option<u32> {
            let s = cc.input_or(node, name, Val::float(default));
            (cc.fold(s).map(|v| v.x()) != Some(default)).then_some(s)
        };
        if let Some(hue) = param(self, "hue", 0.0) {
            let one = self.constant(Val::ONE);
            let amount = self.emit(Op::Combine3 {
                a: hue,
                b: one,
                c: one,
            });
            c = self.emit(Op::HsvAdjust { a: c, amount });
        }
        if let Some(sat) = param(self, "saturation", 1.0) {
            // `saturate`: mix from the luminance grey toward the colour.
            let grey = self.luminance(c);
            c = self.emit(Op::Mix {
                fg: c,
                bg: grey,
                m: sat,
            });
        }
        if let Some(gamma) = param(self, "gamma", 1.0) {
            // `range` over [0, 1] → [0, 1] unclamped: both remaps are the
            // identity, leaving `sign(x)·|x|^(1/gamma)`.
            let one = self.constant(Val::ONE);
            let recip = self.emit(Op::Binary {
                op: BinOp::Div,
                a: one,
                b: gamma,
            });
            let abs = self.emit(Op::Unary {
                op: UnOp::Abs,
                a: c,
            });
            let pow = self.emit(Op::Binary {
                op: BinOp::Pow,
                a: abs,
                b: recip,
            });
            let sign = self.emit(Op::Unary {
                op: UnOp::Sign,
                a: c,
            });
            c = self.emit(Op::Binary {
                op: BinOp::Mul,
                a: pow,
                b: sign,
            });
        }
        if let Some(lift) = param(self, "lift", 0.0) {
            // `c·(1 − lift) + lift`: raises black to `lift`, keeps white.
            let one = self.constant(Val::ONE);
            let keep = self.emit(Op::Binary {
                op: BinOp::Sub,
                a: one,
                b: lift,
            });
            let scaled = self.emit(Op::Binary {
                op: BinOp::Mul,
                a: c,
                b: keep,
            });
            c = self.emit(Op::Binary {
                op: BinOp::Add,
                a: scaled,
                b: lift,
            });
        }
        if let Some(gain) = param(self, "gain", 1.0) {
            c = self.emit(Op::Binary {
                op: BinOp::Mul,
                a: c,
                b: gain,
            });
        }
        if let Some(amount) = param(self, "contrast", 1.0) {
            let pivot = self.input_or(node, "contrastpivot", Val::float(0.5));
            c = self.emit(Op::Contrast {
                a: c,
                amount,
                pivot,
            });
        }
        if let Some(exposure) = param(self, "exposure", 0.0) {
            let two = self.constant(Val::float(2.0));
            let k = self.emit(Op::Binary {
                op: BinOp::Pow,
                a: two,
                b: exposure,
            });
            c = self.emit(Op::Binary {
                op: BinOp::Mul,
                a: c,
                b: k,
            });
        }
        c
    }

    fn compile_image(&mut self, node: &Node, arity: u8) -> u32 {
        let file = node.input("file").and_then(|i| i.text.clone());
        let space = node.input("file").and_then(|i| i.colorspace.clone());
        let tex = file.and_then(|f| {
            let loader = self.loader;
            self.images
                .entry((f, space))
                .or_insert_with_key(|(f, s)| loader(f, s.as_deref()))
                .clone()
        });
        let fallback = node
            .input("default")
            .map(|i| match i.source {
                Source::Value(v) => v,
                _ => Val::float(0.5),
            })
            .unwrap_or(Val::float(0.5));
        // `tiledimage` scales and offsets the chart before the lookup;
        // `image` samples it as authored.
        let (scale, offset) = if node.category == "tiledimage" {
            let s = self.static_input(node, "uvtiling", Val::vec2(1.0, 1.0));
            let o = self.static_input(node, "uvoffset", Val::vec2(0.0, 0.0));
            let s = if s.arity == 1 {
                [s.x(), s.x()]
            } else {
                [s.v[0], s.v[1]]
            };
            let o = if o.arity == 1 {
                [o.x(), o.x()]
            } else {
                [o.v[0], o.v[1]]
            };
            (s, o)
        } else {
            ([1.0, 1.0], [0.0, 0.0])
        };
        let coord = self.image_coord(node);
        self.emit(Op::Texture {
            tex,
            fallback,
            scale,
            offset,
            arity,
            shift: self.uv_shift,
            coord,
        })
    }

    /// An image's authored `texcoord`, compiled, or `None` for the shading
    /// point's own chart.
    ///
    /// `None` covers the unconnected input and a connection to the default
    /// chart itself (`texcoord` index 0, or `geompropvalue` of `st`), which is
    /// how nearly every document spells it; those keep the op on the JIT's
    /// inline texture path. A connection whose subgraph meets an operator
    /// this compiler lacks (`place2d`, a second UV set) also takes `None`:
    /// the chart is a better stand-in for an unknown coordinate than the
    /// constant the unknown node would compile to, and that node is already
    /// reported.
    fn image_coord(&mut self, node: &Node) -> Option<u32> {
        let input = node.input("texcoord")?;
        if let Source::Node { name, .. } = &input.source {
            let scope = node.graph.clone().unwrap_or_default();
            if self.doc.find(&scope, name).is_some_and(is_default_chart) {
                return None;
            }
        }
        // A literal compiles to a constant: one fixed texel, as authored.
        let reported = self.unsupported.len();
        let slot = self.compile_input(node, input);
        (self.unsupported.len() == reported).then_some(slot)
    }
}

/// An input's authored literal, when it is one (not a connection).
impl Compiler<'_> {
    /// An input the lookup needs as a compile-time constant: the literal, or
    /// a connection that folds to one (`convert(8.0)` is how documents feed a
    /// `tiledimage`'s `uvtiling`). One that varies over the surface cannot be
    /// baked into the texture op, so it takes `default` and is reported —
    /// never silently.
    fn static_input(&mut self, node: &Node, name: &str, default: Val) -> Val {
        if let Some(v) = literal_of(node, name) {
            return v;
        }
        if node.input(name).is_none() {
            return default;
        }
        let slot = self.input_or(node, name, default);
        self.fold(slot).unwrap_or_else(|| {
            self.unsupported
                .insert(format!("{} (varying {name})", node.category));
            default
        })
    }
}

/// Whether `node` is the shading point's default chart: `texcoord` with
/// index 0, or `geompropvalue` reading `st` — what `ShadeCtx::uv` holds.
fn is_default_chart(node: &Node) -> bool {
    let text = |name: &str| {
        node.input(name)
            .and_then(|i| i.text.as_deref())
            .map(str::trim)
    };
    match node.category.as_str() {
        "texcoord" => text("index").is_none_or(|i| i == "0"),
        "geompropvalue" => text("geomprop") == Some("st"),
        _ => false,
    }
}

fn literal_of(node: &Node, name: &str) -> Option<Val> {
    match node.input(name)?.source {
        Source::Value(v) => Some(v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_textures(_: &str, _: Option<&str>) -> Option<TextureRef> {
        None
    }

    fn run(doc_text: &str, node: &str) -> Val {
        let doc = Doc::parse(doc_text).unwrap();
        let loader = no_textures;
        let mut c = Compiler::new(&doc, &loader);
        let slot = c.compile_named("", node, None);
        let mut slots = Vec::new();
        c.program.eval(
            &ShadeCtx {
                uv: (0.25, 0.75),
                normal: Vec3A::Z,
                tangent: Vec3A::X,
                view: -Vec3A::Z,
                position: Vec3A::ZERO,
                uv_width: 0.0,
            },
            &mut slots,
        );
        slots[slot as usize]
    }

    #[test]
    fn mix_blends_bg_toward_fg() {
        let v = run(
            r#"<materialx>
                 <constant name="a" type="float"><input name="value" type="float" value="0" /></constant>
                 <constant name="b" type="float"><input name="value" type="float" value="1" /></constant>
                 <mix name="m" type="float">
                   <input name="bg" type="float" nodename="a" />
                   <input name="fg" type="float" nodename="b" />
                   <input name="mix" type="float" value="0.25" />
                 </mix>
               </materialx>"#,
            "m",
        );
        assert!((v.x() - 0.25).abs() < 1e-6, "got {}", v.x());
    }

    #[test]
    fn artistic_ior_round_trips_to_its_reflectivity() {
        // The conductor lobe reduces (n, k) back to a reflectivity colour, so
        // the two conversions must be inverses or every metal shifts hue.
        for r in [0.05f32, 0.4, 0.94] {
            let (n, k) = artistic_ior(Vec3A::splat(r), Vec3A::ONE);
            let back = reflectivity_from_ior(n, k);
            assert!((back.x - r).abs() < 1e-4, "r={r} -> {}", back.x);
        }
    }

    #[test]
    fn a_graph_output_keeps_the_output_it_selected() {
        // A `<nodegraph>`'s `<output>` may select one output of a multioutput
        // node. Dropping that selection is silent: the reference falls back to
        // the node's first output, so a graph publishing `artistic_ior`'s
        // extinction hands its consumer the ior instead.
        let doc = r#"<materialx>
                 <nodegraph name="g">
                   <artistic_ior name="ai" type="multioutput">
                     <input name="reflectivity" type="color3" value="0.5, 0.5, 0.5" />
                     <input name="edge_color" type="color3" value="1, 1, 1" />
                   </artistic_ior>
                   <output name="n" type="color3" nodename="ai" output="ior" />
                   <output name="k" type="color3" nodename="ai" output="extinction" />
                 </nodegraph>
                 <multiply name="take_n" type="color3">
                   <input name="in1" type="color3" nodegraph="g" output="n" />
                   <input name="in2" type="color3" value="1, 1, 1" />
                 </multiply>
                 <multiply name="take_k" type="color3">
                   <input name="in1" type="color3" nodegraph="g" output="k" />
                   <input name="in2" type="color3" value="1, 1, 1" />
                 </multiply>
               </materialx>"#;
        let (n, k) = artistic_ior(Vec3A::splat(0.5), Vec3A::ONE);
        let got_n = run(doc, "take_n");
        let got_k = run(doc, "take_k");
        assert!((got_n.x() - n.x).abs() < 1e-5, "ior: got {}", got_n.x());
        assert!(
            (got_k.x() - k.x).abs() < 1e-5,
            "extinction: got {}",
            got_k.x()
        );
        // The two outputs are what the test is about; equal values would make
        // the assertions above pass for the wrong reason.
        assert!((n.x - k.x).abs() > 1e-3);
    }

    #[test]
    fn a_graph_output_without_a_selection_takes_the_first_output() {
        // The single-output majority authors no `output` attribute, and must
        // keep resolving as it did.
        let v = run(
            r#"<materialx>
                 <nodegraph name="g">
                   <artistic_ior name="ai" type="multioutput">
                     <input name="reflectivity" type="color3" value="0.5, 0.5, 0.5" />
                     <input name="edge_color" type="color3" value="1, 1, 1" />
                   </artistic_ior>
                   <output name="out" type="color3" nodename="ai" />
                 </nodegraph>
                 <multiply name="take" type="color3">
                   <input name="in1" type="color3" nodegraph="g" output="out" />
                   <input name="in2" type="color3" value="1, 1, 1" />
                 </multiply>
               </materialx>"#,
            "take",
        );
        let (n, _) = artistic_ior(Vec3A::splat(0.5), Vec3A::ONE);
        assert!((v.x() - n.x).abs() < 1e-5, "got {}", v.x());
    }

    #[test]
    fn a_cycle_terminates_instead_of_overflowing_the_stack() {
        let v = run(
            r#"<materialx>
                 <multiply name="a" type="float">
                   <input name="in1" type="float" nodename="b" />
                 </multiply>
                 <multiply name="b" type="float">
                   <input name="in1" type="float" nodename="a" />
                 </multiply>
               </materialx>"#,
            "a",
        );
        assert!(v.x().is_finite());
    }

    /// `colorcorrect` of a constant `in`, with `params` authored as floats.
    fn colorcorrect(input: [f32; 3], params: &[(&str, f32)]) -> Vec3A {
        let inputs: String = params
            .iter()
            .map(|(n, v)| format!(r#"<input name="{n}" type="float" value="{v}" />"#))
            .collect();
        let doc = format!(
            r#"<materialx>
                 <colorcorrect name="cc" type="color3">
                   <input name="in" type="color3" value="{}, {}, {}" />
                   {inputs}
                 </colorcorrect>
               </materialx>"#,
            input[0], input[1], input[2]
        );
        let v = run(&doc, "cc");
        assert_eq!(v.arity, 3);
        v.rgb()
    }

    fn close(got: Vec3A, want: Vec3A) {
        assert!(
            (got - want).abs().max_element() < 1e-5,
            "got {got}, want {want}"
        );
    }

    #[test]
    fn colorcorrect_defaults_are_the_identity() {
        // Bitwise: every stage folds to its identity and is left out.
        let c = [0.1, 0.7, 2.5];
        assert_eq!(colorcorrect(c, &[]), Vec3A::from(c));
        assert_eq!(
            colorcorrect(c, &[("hue", 0.0), ("gain", 1.0), ("exposure", 0.0)]),
            Vec3A::from(c)
        );
    }

    #[test]
    fn colorcorrect_applies_each_stage_as_the_stdlib_graph_does() {
        let c = [0.125, 0.5, 1.0];
        close(colorcorrect(c, &[("gain", 4.0)]), Vec3A::new(0.5, 2.0, 4.0));
        // `range` with gamma: x^(1/gamma), sign-preserving, unclamped.
        close(
            colorcorrect([0.125, 8.0, -0.125], &[("gamma", 3.0)]),
            Vec3A::new(0.5, 2.0, -0.5),
        );
        // Lift raises black to `lift` and leaves white alone.
        close(
            colorcorrect([0.0, 0.5, 1.0], &[("lift", 0.5)]),
            Vec3A::new(0.5, 0.75, 1.0),
        );
        close(
            colorcorrect(
                [0.25, 0.5, 1.0],
                &[("contrast", 2.0), ("contrastpivot", 0.5)],
            ),
            Vec3A::new(0.0, 0.5, 1.5),
        );
        close(
            colorcorrect(c, &[("exposure", -1.0)]),
            Vec3A::new(0.0625, 0.25, 0.5),
        );
        // Saturation 0 is the luminance grey (MaterialX's ACEScg default
        // `lumacoeffs`); 2 pushes away from it.
        let l = 0.2722287 * 0.125 + 0.6740818 * 0.5 + 0.0536895;
        close(colorcorrect(c, &[("saturation", 0.0)]), Vec3A::splat(l));
        close(
            colorcorrect(c, &[("saturation", 2.0)]),
            Vec3A::from(c) * 2.0 - Vec3A::splat(l),
        );
        // Hue rotates in turns and wraps: a third of a turn takes red to
        // green, and 1.5 turns is the same as a half.
        close(
            colorcorrect([1.0, 0.0, 0.0], &[("hue", 1.0 / 3.0)]),
            Vec3A::new(0.0, 1.0, 0.0),
        );
        close(
            colorcorrect([1.0, 0.0, 0.0], &[("hue", 1.5)]),
            Vec3A::new(0.0, 1.0, 1.0),
        );
    }

    #[test]
    fn colorcorrect_stages_run_in_the_stdlib_order() {
        // Gamma before lift before gain before contrast before exposure: any
        // two swapped gives a different answer on this input.
        let got = colorcorrect(
            [0.25; 3],
            &[
                ("gamma", 2.0),
                ("lift", 0.5),
                ("gain", 2.0),
                ("contrast", 0.5),
                ("exposure", 1.0),
            ],
        );
        let x = 0.25f32.sqrt(); // gamma → 0.5
        let x = x * (1.0 - 0.5) + 0.5; // lift → 0.75
        let x = x * 2.0; // gain → 1.5
        let x = (x - 0.5) * 0.5 + 0.5; // contrast → 1.0
        let x = x * 2.0; // exposure → 2.0
        close(got, Vec3A::splat(x));
    }

    #[test]
    fn hsv_round_trips() {
        for c in [
            Vec3A::new(0.9, 0.2, 0.1),
            Vec3A::new(0.1, 0.8, 0.3),
            Vec3A::new(0.2, 0.3, 0.95),
            Vec3A::new(0.7, 0.1, 0.6),
            Vec3A::splat(0.4),
            Vec3A::new(3.0, 1.0, 0.5),
        ] {
            close(hsv_to_rgb(rgb_to_hsv(c)), c);
        }
    }

    /// A height of `slope_u · u + slope_v · v`, sampled through a real
    /// `image` node so the shifted lookups are what is being differentiated.
    struct Ramp {
        slope_u: f32,
        slope_v: f32,
    }
    impl crate::Texture for Ramp {
        fn eval(&self, u: f32, v: f32, _: f32) -> [f32; 4] {
            [self.slope_u * u + self.slope_v * v; 4]
        }
    }

    const HEIGHT_DOC: &str = r#"<materialx>
             <image name="h" type="float">
               <input name="file" type="filename" value="height.tif" />
             </image>
             <heighttonormal name="n" type="vector3">
               <input name="in" type="float" nodename="h" />
               <input name="scale" type="float" value="1" />
             </heighttonormal>
             <normalmap name="world" type="vector3">
               <input name="in" type="vector3" nodename="n" />
             </normalmap>
           </materialx>"#;

    fn height_to_normal_at(slope_u: f32, slope_v: f32, uv_width: f32, node: &str) -> Vec3A {
        let doc = Doc::parse(HEIGHT_DOC).unwrap();
        let loader = move |_: &str, _: Option<&str>| {
            Some(TextureRef(std::sync::Arc::new(Ramp { slope_u, slope_v })))
        };
        let mut c = Compiler::new(&doc, &loader);
        let slot = c.compile_named("", node, None);
        let mut slots = Vec::new();
        let ctx = ShadeCtx {
            uv: (0.3, 0.6),
            normal: Vec3A::Z,
            tangent: Vec3A::X,
            view: -Vec3A::Z,
            position: Vec3A::ZERO,
            uv_width,
        };
        c.program.eval(&ctx, &mut slots);
        slots[slot as usize].rgb()
    }

    #[test]
    fn heighttonormal_is_the_osl_reference_over_one_footprint() {
        // A height rising 10 per UV unit, over a 0.01-wide footprint, changes
        // by 0.1 across it: the reference's `Dx(in)`.
        let (du, dv) = (0.1f32, 0.0f32);
        let dz = (1.0 - du * du - dv * dv).sqrt();
        let want = Vec3A::new(-du, -dv, dz).normalize() * 0.5 + Vec3A::splat(0.5);
        close(height_to_normal_at(10.0, 0.0, 0.01, "n"), want);
        let want = Vec3A::new(0.0, -0.1, dz).normalize() * 0.5 + Vec3A::splat(0.5);
        close(height_to_normal_at(0.0, 10.0, 0.01, "n"), want);
    }

    #[test]
    fn heighttonormal_tilts_away_from_rising_height() {
        // Through `normalmap` in a frame with the tangent along +u: a height
        // rising along +u (+v) is a slope facing −u (−v), which is where the
        // normal must lean.
        let n = height_to_normal_at(10.0, 0.0, 0.01, "world");
        assert!(n.x < -0.05 && n.y.abs() < 1e-6 && n.z > 0.9, "{n}");
        let n = height_to_normal_at(0.0, 10.0, 0.01, "world");
        assert!(n.y < -0.05 && n.x.abs() < 1e-6 && n.z > 0.9, "{n}");
    }

    #[test]
    fn heighttonormal_without_a_footprint_is_flat() {
        // No ray cone, no derivative — the nodedef's own default output.
        assert_eq!(
            height_to_normal_at(10.0, 5.0, 0.0, "n"),
            Vec3A::new(0.5, 0.5, 1.0)
        );
    }

    /// `node` of `doc`, every image in it served by `tex`, at uv (0.3, 0.6)
    /// with a footprint `uv_width` wide. Also returns the compiled program.
    fn eval_with(
        doc: &str,
        node: &str,
        tex: impl crate::Texture + Clone + 'static,
        uv_width: f32,
    ) -> (Val, Program) {
        let doc = Doc::parse(doc).unwrap();
        let loader =
            move |_: &str, _: Option<&str>| Some(TextureRef(std::sync::Arc::new(tex.clone())));
        let mut c = Compiler::new(&doc, &loader);
        let slot = c.compile_named("", node, None);
        let mut slots = Vec::new();
        let ctx = ShadeCtx {
            uv: (0.3, 0.6),
            normal: Vec3A::Z,
            tangent: Vec3A::X,
            view: -Vec3A::Z,
            position: Vec3A::ZERO,
            uv_width,
        };
        c.program.eval(&ctx, &mut slots);
        (slots[slot as usize], c.program)
    }

    #[derive(Clone)]
    struct RampTex(f32);
    impl crate::Texture for RampTex {
        fn eval(&self, u: f32, _: f32, _: f32) -> [f32; 4] {
            [self.0 * u; 4]
        }
    }

    /// `heighttonormal` over an image whose `texcoord` is `coord` — a
    /// fragment of nodes naming the connected one `c`, or empty for none.
    fn height_doc(coord: &str) -> String {
        let conn = if coord.is_empty() {
            String::new()
        } else {
            r#"<input name="texcoord" type="vector2" nodename="c" />"#.into()
        };
        format!(
            r#"<materialx>
                 {coord}
                 <image name="h" type="float">
                   <input name="file" type="filename" value="height.tif" />
                   {conn}
                 </image>
                 <heighttonormal name="n" type="vector3">
                   <input name="in" type="float" nodename="h" />
                 </heighttonormal>
               </materialx>"#
        )
    }

    fn texture_coords(p: &Program) -> Vec<Option<u32>> {
        p.ops
            .iter()
            .filter_map(|op| match op {
                Op::Texture { coord, .. } => Some(*coord),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn heighttonormal_of_a_constant_coordinate_is_flat() {
        // Every tap reads the same texel, whatever the footprint.
        let doc = height_doc(
            r#"<constant name="c" type="vector2"><input name="value" type="vector2" value="0.5, 0.5" /></constant>"#,
        );
        let (n, _) = eval_with(&doc, "n", RampTex(10.0), 0.01);
        assert_eq!(n.rgb(), Vec3A::new(0.5, 0.5, 1.0));
    }

    #[test]
    fn the_default_chart_spelled_out_is_the_implicit_one() {
        // `geompropvalue st` and `texcoord` are what `ctx.uv` already holds:
        // same answer as no connection, and still the JIT's inline lookup.
        let (want, p) = eval_with(&height_doc(""), "n", RampTex(10.0), 0.01);
        assert!(texture_coords(&p).iter().all(Option::is_none));
        for c in [
            r#"<geompropvalue name="c" type="vector2"><input name="geomprop" type="string" value="st" /></geompropvalue>"#,
            r#"<texcoord name="c" type="vector2" />"#,
        ] {
            let (got, p) = eval_with(&height_doc(c), "n", RampTex(10.0), 0.01);
            assert_eq!(bits_of(got), bits_of(want), "{c}");
            assert!(texture_coords(&p).iter().all(Option::is_none), "{c}");
        }
    }

    fn bits_of(v: Val) -> ([u32; 4], u8) {
        (v.v.map(f32::to_bits), v.arity)
    }

    #[test]
    fn an_authored_coordinate_is_sampled_and_differentiated_through() {
        // `texcoord · 2`: the image reads at twice the chart, and the height
        // changes twice as fast across the same footprint.
        let doc = height_doc(
            r#"<texcoord name="t" type="vector2" />
               <multiply name="c" type="vector2">
                 <input name="in1" type="vector2" nodename="t" />
                 <input name="in2" type="float" value="2" />
               </multiply>"#,
        );
        let (h, _) = eval_with(&doc, "h", RampTex(10.0), 0.01);
        assert!((h.x() - 10.0 * 0.6).abs() < 1e-5, "height {}", h.x());
        let (n, _) = eval_with(&doc, "n", RampTex(10.0), 0.01);
        let du = 0.2f32; // 10 per unit, 2x the chart, 0.01 across
        let want =
            Vec3A::new(-du, 0.0, (1.0 - du * du).sqrt()).normalize() * 0.5 + Vec3A::splat(0.5);
        close(n.rgb(), want);
    }

    #[test]
    fn an_uncompilable_coordinate_falls_back_to_the_chart() {
        // `place2d` has no operator here; its constant stand-in would pin the
        // lookup to one texel, so the chart is used instead (and reported).
        let doc = height_doc(r#"<place2d name="c" type="vector2" />"#);
        let (h, p) = eval_with(&doc, "h", RampTex(10.0), 0.01);
        assert!((h.x() - 3.0).abs() < 1e-5, "height {}", h.x());
        assert!(texture_coords(&p).iter().all(Option::is_none));
    }

    #[derive(Clone)]
    struct Channels;
    impl crate::Texture for Channels {
        fn eval(&self, _: f32, _: f32, _: f32) -> [f32; 4] {
            [0.25, 0.5, 0.75, 1.0]
        }
    }

    #[test]
    fn colorcorrect_of_a_float_image_is_grey() {
        // A one-lane lookup keeps its file's other channels in lanes 1..3; the
        // correction must promote lane 0, not correct and expose the rest.
        let doc = r#"<materialx>
                 <image name="m" type="float">
                   <input name="file" type="filename" value="mask.tif" />
                 </image>
                 <colorcorrect name="cc" type="color3">
                   <input name="in" type="float" nodename="m" />
                   <input name="gain" type="float" value="2" />
                 </colorcorrect>
                 <convert name="out" type="color3">
                   <input name="in" type="color3" nodename="cc" />
                 </convert>
               </materialx>"#;
        for node in ["cc", "out"] {
            let (v, _) = eval_with(doc, node, Channels, 0.0);
            assert_eq!(v.arity, 3, "{node}");
            assert_eq!(v.rgb(), Vec3A::splat(0.5), "{node}");
        }
    }

    #[test]
    fn heighttonormal_asks_the_host_for_its_image_once() {
        // Four shifted copies of the lookup, one sampler: the host's decode
        // and residency are per file, not per tap.
        let doc = Doc::parse(HEIGHT_DOC).unwrap();
        let calls = std::cell::Cell::new(0);
        let loader = |_: &str, _: Option<&str>| {
            calls.set(calls.get() + 1);
            Some(TextureRef(std::sync::Arc::new(Ramp {
                slope_u: 1.0,
                slope_v: 0.0,
            })))
        };
        let mut c = Compiler::new(&doc, &loader);
        c.compile_named("", "n", None);
        c.compile_named("", "h", None);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn division_by_zero_stays_finite() {
        // `1 / transmittance` with a black channel is authored in the teapot's
        // own ceramic graph; an infinity there survives every later multiply
        // and reaches the framebuffer as a NaN pixel.
        let v = run(
            r#"<materialx>
                 <divide name="d" type="float">
                   <input name="in1" type="float" value="1" />
                   <input name="in2" type="float" value="0" />
                 </divide>
               </materialx>"#,
            "d",
        );
        assert!(v.x().is_finite());
    }
}
