//! The compiler: named `.mtlx` nodes turned into a topologically ordered
//! [`Program`].

use glam::Vec3A;

use super::*;
use crate::parse::{Doc, Input, Node, Source};
use crate::texture::TextureRef;
use crate::value::{Val, arity_of, convert_color, is_color_type};

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
    /// What the loader answered per `(file, colorspace)` — the space as
    /// handed to it — so the shifted copies of an `image`, and two images of
    /// one file in one effective space, share the one sampler rather than
    /// asking the host again.
    images: std::collections::HashMap<(String, Option<String>), Option<TextureRef>>,
    /// Nodes currently being compiled, so a cyclic document — which a
    /// hand-edited `.mtlx` can be — terminates as a constant rather than
    /// recursing until the stack runs out.
    active: Vec<String>,
    /// Resolves an `image` node's `file` input to a sampler, and converts
    /// authored colours into the working space.
    host: crate::Host<'a>,
    /// Node categories met that this compiler has no operator for, for one
    /// summary warning instead of one per occurrence.
    pub unsupported: std::collections::BTreeSet<String>,
}

impl<'a> Compiler<'a> {
    pub fn new(doc: &'a Doc, host: &crate::Host<'a>) -> Compiler<'a> {
        Compiler {
            doc,
            program: Program::default(),
            memo: std::collections::HashMap::new(),
            uv_shift: [0.0, 0.0],
            images: std::collections::HashMap::new(),
            active: Vec::new(),
            host: *host,
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
            curve: None,
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
                // A literal "0.5" on a color3 input broadcasts, which the
                // parser already arranged; a colour is then taken into the
                // working space.
                let v = self.managed(node, input, *v);
                self.constant(v)
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
            "chiang_hair_roughness" => self.compile_chiang_hair_roughness(node, output),
            "chiang_hair_absorption_from_color" => {
                self.compile_chiang_hair_absorption_from_color(node)
            }
            "deon_hair_absorption_from_melanin" => {
                self.compile_deon_hair_absorption_from_melanin(node)
            }
            "colorcorrect" => self.compile_colorcorrect(node),
            "rgbtohsv" => {
                let a = self.input_or(node, "in", Val::ZERO);
                self.emit(Op::RgbToHsv { a })
            }
            "hsvtorgb" => {
                let a = self.input_or(node, "in", Val::ZERO);
                self.emit(Op::HsvToRgb { a })
            }
            "geompropvalue" => self.compile_geompropvalue(node, arity),
            "ifgreater" => self.compile_conditional(node, CmpOp::Greater),
            "ifgreatereq" => self.compile_conditional(node, CmpOp::GreaterEq),
            "ifequal" => self.compile_conditional(node, CmpOp::Equal),
            "cellnoise2d" => {
                // The nodedef's `defaultgeomprop="UV0"`: unauthored, it reads
                // the shading point's chart.
                let a = match node.input("texcoord") {
                    Some(_) => self.input_or(node, "texcoord", Val::ZERO),
                    None => self.emit(Op::TexCoord {
                        shift: self.uv_shift,
                    }),
                };
                self.emit(Op::CellNoise2 { a })
            }
            "randomfloat" => {
                let min = self.input_or(node, "min", Val::ZERO);
                let max = self.input_or(node, "max", Val::ONE);
                // `NG_randomfloat_float` spreads a float signal over cells
                // 1/4096 wide; the `integer` variant uses the value itself.
                let scale = node.input("in").is_none_or(|i| i.type_name != "integer");
                self.random_float(node, scale, min, max, None)
            }
            "randomcolor" => self.compile_randomcolor(node),
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

    /// `geompropvalue`: the default chart for `st`, a curve hit's strand for
    /// the reserved `curve:v` / `curve:index` / `curve:random`, and any other
    /// name a primvar the host supplies per hit ([`Program::primvars`]) —
    /// each falling back to the node's `default` where the hit has none.
    fn compile_geompropvalue(&mut self, node: &Node, arity: u8) -> u32 {
        let name = node
            .input("geomprop")
            .and_then(|i| i.text.as_deref())
            .map(str::trim)
            .unwrap_or("");
        let name = name.strip_prefix("primvars:").unwrap_or(name);
        // The nodedef's `default` is a typed zero; an authored one is a
        // literal like any other (a colour converted into the working
        // space). A connected one cannot be folded into the op.
        let fallback = match self.literal_of(node, "default") {
            Some(v) => v.broadcast_to(arity),
            None => {
                if node.input("default").is_some() {
                    self.unsupported
                        .insert("geompropvalue (connected default)".into());
                }
                Val::ZERO.broadcast_to(arity)
            }
        };
        let prop = match name {
            "st" => {
                let chart = self.emit(Op::TexCoord {
                    shift: self.uv_shift,
                });
                return if arity == 2 {
                    chart
                } else {
                    self.emit(Op::Convert { a: chart, arity })
                };
            }
            "curve:v" => CurveProp::V,
            "curve:index" => CurveProp::Index,
            "curve:random" => CurveProp::Random,
            // An unknown property in the reserved namespace, or no name.
            other if other.is_empty() || other.starts_with("curve:") => {
                self.unsupported
                    .insert(format!("geompropvalue \"{other}\""));
                return self.constant(fallback);
            }
            other => {
                let channel = match self.program.primvars.iter().position(|p| p == other) {
                    Some(i) => i,
                    None => {
                        self.program.primvars.push(other.to_string());
                        self.program.primvars.len() - 1
                    }
                };
                return self.emit(Op::Primvar {
                    channel: channel as u32,
                    arity,
                    fallback,
                });
            }
        };
        self.emit(Op::Curve {
            prop,
            arity,
            fallback,
        })
    }

    /// `ifgreater` / `ifgreatereq` / `ifequal`: `in1` when `value1 cmp
    /// value2`, else `in2`. The `boolean` signatures have no branches and
    /// answer 1 or 0, as genosl's `mx_ternary(…, 1, 0)` does.
    ///
    /// The unauthored `value1` is 1 for `ifgreater` and `ifgreatereq`, and 0
    /// for `ifequal` — except `ND_ifequal_boolean`, whose float values default
    /// to 1 and 0 like the others. Nothing but the output type tells that
    /// signature from `ND_ifequal_booleanB` (boolean values, both false), so
    /// a boolean `ifequal` authoring a `boolean` value takes the latter's.
    fn compile_conditional(&mut self, node: &Node, cmp: CmpOp) -> u32 {
        let boolean_out = node.type_name == "boolean";
        let boolean_values = ["value1", "value2"]
            .iter()
            .any(|n| node.input(n).is_some_and(|i| i.type_name == "boolean"));
        let v1_default = match cmp {
            CmpOp::Equal if !boolean_out || boolean_values => 0.0,
            _ => 1.0,
        };
        let value1 = self.input_or(node, "value1", Val::float(v1_default));
        let value2 = self.input_or(node, "value2", Val::ZERO);
        let (in1, in2) = if boolean_out {
            (self.constant(Val::ONE), self.constant(Val::ZERO))
        } else {
            (
                self.input_or(node, "in1", Val::ZERO),
                self.input_or(node, "in2", Val::ZERO),
            )
        };
        self.emit(Op::Select {
            cmp,
            value1,
            value2,
            in1,
            in2,
        })
    }

    /// `NG_randomfloat_*`: `cellnoise2d` of `(in · 4096, seed)` (`in` as is
    /// when `scale` is false), `range`d from [0, 1] onto `[min, max]` and
    /// clamped there. `seed` is the node's own input unless a slot is given,
    /// as `randomcolor`'s expansion gives one per channel.
    ///
    /// `range`'s first remap and its gamma stage are left out: at the
    /// nodegraph's `inlow` 0, `inhigh` 1 and `gamma` 1 they are exact
    /// identities (`x / 1`, `|x|^1 · sign x`) on a noise value in [0, 1].
    fn random_float(
        &mut self,
        node: &Node,
        scale: bool,
        min: u32,
        max: u32,
        seed: Option<u32>,
    ) -> u32 {
        let mut signal = self.input_or(node, "in", Val::ZERO);
        if scale {
            let k = self.constant(Val::float(4096.0));
            signal = self.emit(Op::Binary {
                op: BinOp::Mul,
                a: signal,
                b: k,
            });
        }
        let seed = match seed {
            Some(s) => s,
            None => self.input_or(node, "seed", Val::ZERO),
        };
        // Both are scalars in the nodedef; one lane each, so `combine2`
        // makes a `vector2` whatever is wired in.
        let signal = self.emit(Op::Convert {
            a: signal,
            arity: 1,
        });
        let seed = self.emit(Op::Convert { a: seed, arity: 1 });
        let coord = self.emit(Op::Combine2 { a: signal, b: seed });
        let noise = self.emit(Op::CellNoise2 { a: coord });
        let (zero, one) = (self.constant(Val::ZERO), self.constant(Val::ONE));
        let ranged = self.emit(Op::Remap {
            a: noise,
            in_low: zero,
            in_high: one,
            out_low: min,
            out_high: max,
        });
        self.emit(Op::Clamp {
            a: ranged,
            low: min,
            high: max,
        })
    }

    /// `NG_randomcolor_float` (the `integer` variant only converts `in` and
    /// calls it): hue, saturation and brightness each a `randomfloat` of the
    /// same signal under its own seed, `ceil(seed + offset)`, `range`d onto
    /// the authored interval (unclamped), then `hsvtorgb`.
    fn compile_randomcolor(&mut self, node: &Node) -> u32 {
        let seed = self.input_or(node, "seed", Val::ZERO);
        let channels = [
            (413.3, "huelow", 0.0, "huehigh", 1.0),
            (1522.4, "saturationlow", 0.825, "saturationhigh", 1.0),
            (1813.8, "brightnesslow", 1.0, "brightnesshigh", 1.0),
        ];
        let mut hsv = [0u32; 3];
        for (slot, (offset, lo, lo_default, hi, hi_default)) in hsv.iter_mut().zip(channels) {
            let k = self.constant(Val::float(offset));
            let shifted = self.emit(Op::Binary {
                op: BinOp::Add,
                a: seed,
                b: k,
            });
            let channel_seed = self.emit(Op::Unary {
                op: UnOp::Ceil,
                a: shifted,
            });
            let (zero, one) = (self.constant(Val::ZERO), self.constant(Val::ONE));
            let r = self.random_float(node, true, zero, one, Some(channel_seed));
            let out_low = self.input_or(node, lo, Val::float(lo_default));
            let out_high = self.input_or(node, hi, Val::float(hi_default));
            *slot = self.emit(Op::Remap {
                a: r,
                in_low: zero,
                in_high: one,
                out_low,
                out_high,
            });
        }
        let [a, b, c] = hsv;
        let combined = self.emit(Op::Combine3 { a, b, c });
        self.emit(Op::HsvToRgb { a: combined })
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
        let file = node.input("file");
        // The texels' colour space: the `file` input's effective one, for a
        // colour image only. A `float` or `vector3` image is data — a mask,
        // a height, a normal map — whatever the document's default says.
        let space = file
            .filter(|_| is_color_type(&node.type_name))
            .and_then(|f| self.doc.colorspace_of(node, f))
            .map(str::to_string);
        let tex = file.and_then(|i| i.text.clone()).and_then(|f| {
            let loader = self.host.load_texture;
            self.images
                .entry((f, space))
                .or_insert_with_key(|(f, s)| loader(f, s.as_deref()))
                .clone()
        });
        // The authored `default` is a literal like any other: a colour one is
        // converted from its own effective space.
        let fallback = self.literal_of(node, "default").unwrap_or(Val::float(0.5));
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
    /// inline texture path. So does a `geompropvalue` naming another UV set:
    /// the chart is the one the host chose for the geometry, which is what
    /// such an image read before `geompropvalue` compiled at all. A
    /// connection whose subgraph meets an operator this compiler lacks
    /// (`place2d`) also takes `None`: the chart is a better stand-in for an
    /// unknown coordinate than the constant the unknown node would compile
    /// to, and that node is already reported.
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
    /// An authored literal `v` of `input` as the program holds it: a
    /// `color3` / `color4` with an effective colour space converted into the
    /// working space by the host, anything else as authored.
    fn managed(&self, node: &Node, input: &Input, v: Val) -> Val {
        if !is_color_type(&input.type_name) {
            return v;
        }
        match self.doc.colorspace_of(node, input) {
            Some(space) => convert_color(v, &input.type_name, |rgb| {
                (self.host.convert_color)(space, rgb)
            }),
            None => v,
        }
    }

    /// `node`'s input `name` when it is an authored literal (not a
    /// connection), with the value the program holds for it — converted, for
    /// a colour. What literal-driven pruning reads, so that it tests exactly
    /// the value every shading point would have computed.
    pub(crate) fn literal_of(&self, node: &Node, name: &str) -> Option<Val> {
        let input = node.input(name)?;
        match input.source {
            Source::Value(v) => Some(self.managed(node, input, v)),
            _ => None,
        }
    }

    /// An input the lookup needs as a compile-time constant: the literal, or
    /// a connection that folds to one (`convert(8.0)` is how documents feed a
    /// `tiledimage`'s `uvtiling`). One that varies over the surface cannot be
    /// baked into the texture op, so it takes `default` and is reported —
    /// never silently.
    fn static_input(&mut self, node: &Node, name: &str, default: Val) -> Val {
        if let Some(v) = self.literal_of(node, name) {
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

/// Whether an image reads `node` as the shading point's chart, what
/// `ShadeCtx::uv` holds: `texcoord` with index 0, or a `geompropvalue` of
/// `st` or of any other primvar but the `curve:` properties (see
/// `Compiler::image_coord`).
fn is_default_chart(node: &Node) -> bool {
    let text = |name: &str| {
        node.input(name)
            .and_then(|i| i.text.as_deref())
            .map(str::trim)
    };
    match node.category.as_str() {
        "texcoord" => text("index").is_none_or(|i| i == "0"),
        "geompropvalue" => text("geomprop").is_some_and(|g| !g.starts_with("curve:")),
        _ => false,
    }
}
