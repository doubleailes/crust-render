//! The program's instruction set.

use crate::texture::TextureRef;
use crate::value::Val;

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
    pub(super) fn is_pure(&self) -> bool {
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
