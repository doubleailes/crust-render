//! Named `.mtlx` nodes compiled into a [`Program`].

use super::apply::apply;
use super::op::{BinOp, Op, UnOp};
use super::{Program, ShadeCtx};
use crate::parse::{Doc, Input, Node, Source};
use crate::texture::TextureRef;
use crate::value::{Val, arity_of};
use glam::Vec3A;

/// `luminance`'s default `lumacoeffs`: ACEScg's (AP1) weights.
const AP1_LUMA_COEFFS: Val = Val::vec3(0.2722287, 0.6740818, 0.0536895);

/// Turns named `.mtlx` nodes into a topologically ordered [`Program`].
pub struct Compiler<'a> {
    pub(crate) doc: &'a Doc,
    pub(crate) program: Program,
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
    pub(crate) unsupported: std::collections::BTreeSet<String>,
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

    /// The program compiled so far.
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// Node categories met that this compiler has no operator for.
    pub fn unsupported(&self) -> &std::collections::BTreeSet<String> {
        &self.unsupported
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

/// An input's authored literal, when it is one (not a connection).
fn literal_of(node: &Node, name: &str) -> Option<Val> {
    match node.input(name)?.source {
        Source::Value(v) => Some(v),
        _ => None,
    }
}
