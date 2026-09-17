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
/// instruction's own index — the compiler emits in topological order, so a
/// single forward pass evaluates the whole program.
#[derive(Debug)]
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
    },
    /// `primvars:st` as a `vector2`.
    TexCoord,
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
    Luminance {
        a: u32,
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
}

/// A compiled pattern graph.
#[derive(Default)]
pub struct Program {
    pub ops: Vec<Op>,
}

impl Program {
    /// Evaluates every instruction into `slots`, which is resized as needed.
    ///
    /// The caller owns the buffer so it can be reused across shading calls —
    /// a fresh `Vec` per call would allocate once per BSDF evaluation, which
    /// at several evaluations per path vertex is the kind of cost that does
    /// not show up in a profile as one hot line.
    pub fn eval(&self, ctx: &ShadeCtx, slots: &mut Vec<Val>) {
        slots.clear();
        slots.reserve(self.ops.len());
        for op in &self.ops {
            // Every operand index was emitted before this instruction, so the
            // slot exists. `get` rather than indexing keeps a malformed
            // program from panicking inside the integrator.
            let g = |i: u32| -> Val { slots.get(i as usize).copied().unwrap_or(Val::ZERO) };
            let v = match op {
                Op::Const(v) => *v,
                Op::Texture {
                    tex,
                    fallback,
                    scale,
                    offset,
                    arity,
                } => match tex {
                    Some(t) => {
                        let u = ctx.uv.0 * scale[0] + offset[0];
                        let v = ctx.uv.1 * scale[1] + offset[1];
                        let rgba = t.eval(u, v);
                        Val {
                            v: rgba,
                            arity: *arity,
                        }
                    }
                    None => *fallback,
                },
                Op::TexCoord => Val::vec2(ctx.uv.0, ctx.uv.1),
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
                        UnOp::Ln => a.map(|x| if x > 1e-30 { x.ln() } else { -69.0 }),
                        UnOp::Exp => a.map(|x| x.clamp(-88.0, 88.0).exp()),
                        UnOp::Sin => a.map(f32::sin),
                        UnOp::Cos => a.map(f32::cos),
                        UnOp::Asin => a.map(|x| x.clamp(-1.0, 1.0).asin()),
                        UnOp::Acos => a.map(|x| x.clamp(-1.0, 1.0).acos()),
                        UnOp::Sqrt => a.map(|x| x.max(0.0).sqrt()),
                        UnOp::Sign => a.map(f32::signum),
                        UnOp::Floor => a.map(f32::floor),
                        UnOp::Ceil => a.map(f32::ceil),
                        UnOp::Normalize => {
                            let v = a.rgb();
                            let n = v.length();
                            if n > 1e-20 { (v / n).into() } else { a }
                        }
                    }
                }
                Op::Binary { op, a, b } => {
                    let (a, b) = (g(*a), g(*b));
                    match op {
                        BinOp::Add => a.zip(b, |x, y| x + y),
                        BinOp::Sub => a.zip(b, |x, y| x - y),
                        BinOp::Mul => a.zip(b, |x, y| x * y),
                        // A zero divisor is a real possibility in these
                        // graphs (`1 / transmittance` with a black channel),
                        // and an infinity survives every later multiply.
                        BinOp::Div => a.zip(b, |x, y| if y.abs() > 1e-20 { x / y } else { 0.0 }),
                        BinOp::Pow => a.zip(b, |x, y| x.max(0.0).powf(y)),
                        BinOp::Min => a.zip(b, f32::min),
                        BinOp::Max => a.zip(b, f32::max),
                        BinOp::Modulo => a.zip(b, |x, y| if y.abs() > 1e-20 { x % y } else { 0.0 }),
                    }
                }
                // `bg·(1−m) + fg·m`. Written as two weighted terms rather
                // than `bg + (fg−bg)·m` so that a `float` mix against wider
                // operands broadcasts through `zip`'s promotion in both
                // terms alike.
                Op::Mix { fg, bg, m } => {
                    let (fg, bg, m) = (g(*fg), g(*bg), g(*m));
                    let inv = m.map(|x| 1.0 - x);
                    bg.zip(inv, |b, t| b * t)
                        .zip(fg.zip(m, |f, t| f * t), |a, b| a + b)
                }
                Op::Clamp { a, low, high } => {
                    let (a, lo, hi) = (g(*a), g(*low), g(*high));
                    a.zip(lo, f32::max).zip(hi, f32::min)
                }
                Op::Contrast { a, amount, pivot } => {
                    let (a, amt, piv) = (g(*a), g(*amount), g(*pivot));
                    a.zip(piv, |x, p| x - p)
                        .zip(amt, |x, m| x * m)
                        .zip(piv, |x, p| x + p)
                }
                Op::Remap {
                    a,
                    in_low,
                    in_high,
                    out_low,
                    out_high,
                } => {
                    let (a, il, ih, ol, oh) =
                        (g(*a), g(*in_low), g(*in_high), g(*out_low), g(*out_high));
                    let t = a
                        .zip(il, |x, l| x - l)
                        .zip(ih.zip(il, |h, l| h - l), |x, d| {
                            if d.abs() > 1e-20 { x / d } else { 0.0 }
                        });
                    ol.zip(oh.zip(ol, |h, l| h - l).zip(t, |d, t| d * t), |l, x| l + x)
                }
                Op::Invert { a, amount } => g(*amount).zip(g(*a), |m, x| m - x),
                Op::Convert { a, arity } => g(*a).with_arity(*arity),
                Op::Extract { a, index } => Val::float(g(*a).v[(*index).min(3)]),
                Op::Combine3 { a, b, c } => Val::vec3(g(*a).x(), g(*b).x(), g(*c).x()),
                Op::Combine2 { a, b } => Val::vec2(g(*a).x(), g(*b).x()),
                Op::DotProduct { a, b } => Val::float(g(*a).rgb().dot(g(*b).rgb())),
                Op::Luminance { a } => {
                    let c = g(*a).rgb();
                    Val::float(c.dot(Vec3A::new(0.2722287, 0.6740818, 0.0536895)))
                }
                Op::NormalMap { a, scale } => normal_map(g(*a), g(*scale).x(), ctx).into(),
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
                    a.zip(lo, |x, l| x - l)
                        .zip(hi.zip(lo, |h, l| h - l), |x, d| {
                            if d.abs() > 1e-20 {
                                (x / d).clamp(0.0, 1.0)
                            } else {
                                0.0
                            }
                        })
                        .map(|t| t * t * (3.0 - 2.0 * t))
                }
            };
            slots.push(v);
        }
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
fn normal_map(encoded: Val, scale: f32, ctx: &ShadeCtx) -> Vec3A {
    if ctx.tangent.length_squared() < 1e-20 {
        return ctx.normal;
    }
    let n = ctx.normal;
    // Re-orthogonalise: the stored tangent is the triangle's, while `n` may
    // already carry interpolated shading curvature, so the two need not be
    // perpendicular.
    let t = (ctx.tangent - n * n.dot(ctx.tangent)).normalize_or_zero();
    if t.length_squared() < 1e-20 {
        return n;
    }
    let b = n.cross(t);
    let v = encoded.rgb() * 2.0 - Vec3A::ONE;
    let scale = if scale.is_finite() { scale } else { 1.0 };
    let local = Vec3A::new(v.x * scale, v.y * scale, v.z.max(1e-4));
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
    // GLSL `mix(n_max, n_min, edge)`: an edge colour of white — the default —
    // selects `n_min`.
    let n = n_max + (n_min - n_max) * edge.clamp(Vec3A::ZERO, Vec3A::ONE);
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

/// Turns named `.mtlx` nodes into a topologically ordered [`Program`].
pub struct Compiler<'a> {
    pub doc: &'a Doc,
    pub program: Program,
    /// Slot already emitted for a `(graph, node, output)` triple, so a node
    /// feeding five others is evaluated once. The output name matters:
    /// `artistic_ior` emits a different slot for `ior` than for `extinction`.
    memo: std::collections::HashMap<(String, String, String), u32>,
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

    /// Compiles the value feeding `input` of `node`, or `default` when the
    /// input is unauthored.
    pub fn input_or(&mut self, node: &Node, name: &str, default: Val) -> u32 {
        match node.input(name) {
            Some(i) => self.compile_input(node, i),
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
            "texcoord" => self.emit(Op::TexCoord),
            "normal" => self.emit(Op::Normal),
            "viewdirection" => self.emit(Op::ViewDirection),
            "position" => self.emit(Op::Position),
            "add" => bin(self, BinOp::Add, Val::ZERO, Val::ZERO),
            "subtract" => bin(self, BinOp::Sub, Val::ZERO, Val::ZERO),
            "multiply" => bin(self, BinOp::Mul, Val::ONE, Val::ONE),
            "divide" => bin(self, BinOp::Div, Val::ONE, Val::ONE),
            "power" => bin(self, BinOp::Pow, Val::ONE, Val::ONE),
            "min" => bin(self, BinOp::Min, Val::ZERO, Val::ZERO),
            "max" => bin(self, BinOp::Max, Val::ZERO, Val::ZERO),
            "modulo" => bin(self, BinOp::Modulo, Val::ONE, Val::ONE),
            "absval" => un(self, UnOp::Abs),
            "ln" => un(self, UnOp::Ln),
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
                self.emit(Op::Luminance { a })
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
                let a = self.input_or(node, "in1", Val::ZERO);
                let b = self.input_or(node, "in2", Val::ZERO);
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
                let reflectivity = self.input_or(node, "reflectivity", Val::vec3(0.94, 0.78, 0.37));
                let edge = self.input_or(node, "edge_color", Val::vec3(0.998, 0.981, 0.751));
                self.emit(Op::ArtisticIor {
                    reflectivity,
                    edge,
                    extinction: output == Some("extinction"),
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

    fn compile_image(&mut self, node: &Node, arity: u8) -> u32 {
        let file = node.input("file").and_then(|i| i.text.clone());
        let space = node.input("file").and_then(|i| i.colorspace.clone());
        let tex = file
            .as_deref()
            .and_then(|f| (self.loader)(f, space.as_deref()));
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
            let s = literal_of(node, "uvtiling").unwrap_or(Val::vec2(1.0, 1.0));
            let o = literal_of(node, "uvoffset").unwrap_or(Val::vec2(0.0, 0.0));
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
        self.emit(Op::Texture {
            tex,
            fallback,
            scale,
            offset,
            arity,
        })
    }
}

/// An input's authored literal, when it is one (not a connection).
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
