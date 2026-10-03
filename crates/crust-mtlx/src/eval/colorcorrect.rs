//! `colorcorrect`, lowered to the stdlib nodegraph's chain of operators.

use super::compiler::Compiler;
use super::op::{BinOp, Op, UnOp};
use crate::parse::Node;
use crate::value::Val;

impl Compiler<'_> {
    /// MaterialX `colorcorrect` (`color3`), lowered to the stdlib's own
    /// `NG_colorcorrect_color3` chain: `hsvadjust` (hue) → `saturate` →
    /// `range` (gamma) → lift → gain → `contrast` → exposure.
    ///
    /// Expanded here rather than kept as one op so the stages reuse
    /// operators the JIT already inlines. A stage whose parameter folds to
    /// its identity (hue 0, saturation 1, …) is left out: the reference's
    /// arithmetic at those values is the identity up to rounding at most, and
    /// the playground's graphs author one or two of the eight inputs.
    pub(super) fn compile_colorcorrect(&mut self, node: &Node) -> u32 {
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
}
