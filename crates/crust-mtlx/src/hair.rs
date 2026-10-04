//! MaterialX's hair nodes: the `chiang_hair_bsdf` leaf's nodedef, and the
//! three helper nodes that turn artist parameters into its inputs.
//!
//! The helpers compile to the ordinary pattern program, from existing
//! operators only, so the JIT runs them like any other pattern node. Their
//! reference is MaterialX's genglsl (`mx_chiang_hair_bsdf.glsl`) rather than
//! the OSL oracle: MaterialX 1.39's genosl implementations of these three are
//! placeholders. `tests/hair_helpers.rs` pins them against
//! `scripts/hair_reference.py`, a float64 transcription of the GLSL.

use crate::eval::{BinOp, Compiler, Op, UnOp};
use crate::parse::Node;
use crate::surface::InputDef;
use crate::value::Val;

// Generated from `tests/nodedefs/ND_chiang_hair.mtlx` (MaterialX 1.39,
// Apache-2.0); `nodedef_tables_match_materialx` keeps them honest.
#[rustfmt::skip]
mod tables {
    use super::InputDef;
    use crate::value::Val;

    pub const CHIANG_HAIR_BSDF: &[InputDef] = &[
        InputDef { name: "tint_R", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "tint_TT", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "tint_TRT", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "ior", ty: "float", default: Some(Val::float(1.55)) },
        InputDef { name: "roughness_R", ty: "vector2", default: Some(Val::vec2(0.1, 0.1)) },
        InputDef { name: "roughness_TT", ty: "vector2", default: Some(Val::vec2(0.05, 0.05)) },
        InputDef { name: "roughness_TRT", ty: "vector2", default: Some(Val::vec2(0.2, 0.2)) },
        InputDef { name: "cuticle_angle", ty: "float", default: Some(Val::float(0.5)) },
        InputDef { name: "absorption_coefficient", ty: "vector3", default: Some(Val::vec3(0.0, 0.0, 0.0)) },
        InputDef { name: "normal", ty: "vector3", default: None },
        InputDef { name: "curve_direction", ty: "vector3", default: None },
    ];

    pub const DEON_HAIR_ABSORPTION_FROM_MELANIN: &[InputDef] = &[
        InputDef { name: "melanin_concentration", ty: "float", default: Some(Val::float(0.25)) },
        InputDef { name: "melanin_redness", ty: "float", default: Some(Val::float(0.5)) },
        InputDef { name: "eumelanin_color", ty: "color3", default: Some(Val::vec3(0.657704, 0.498077, 0.254107)) },
        InputDef { name: "pheomelanin_color", ty: "color3", default: Some(Val::vec3(0.829444, 0.67032, 0.349938)) },
    ];

    pub const CHIANG_HAIR_ABSORPTION_FROM_COLOR: &[InputDef] = &[
        InputDef { name: "color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "azimuthal_roughness", ty: "float", default: Some(Val::float(0.2)) },
    ];

    pub const CHIANG_HAIR_ROUGHNESS: &[InputDef] = &[
        InputDef { name: "longitudinal", ty: "float", default: Some(Val::float(0.1)) },
        InputDef { name: "azimuthal", ty: "float", default: Some(Val::float(0.2)) },
        InputDef { name: "scale_TT", ty: "float", default: Some(Val::float(0.5)) },
        InputDef { name: "scale_TRT", ty: "float", default: Some(Val::float(2.0)) },
    ];
}

pub use tables::*;

/// The default `table` gives input `name`, which must have one.
pub(crate) fn default_of(table: &[InputDef], name: &str) -> Val {
    table
        .iter()
        .find(|d| d.name == name)
        .and_then(|d| d.default)
        .unwrap_or_else(|| panic!("{name} has a default"))
}

impl Compiler<'_> {
    /// `input` of `node`, or its nodedef default from `table` at the
    /// default's own width (not widened to the node's output type, which for
    /// these nodes says nothing about their inputs).
    fn hair_input(&mut self, node: &Node, table: &[InputDef], name: &str) -> u32 {
        match node.input(name) {
            Some(_) => self.input_or(node, name, Val::ZERO),
            None => self.constant(default_of(table, name)),
        }
    }

    fn k(&mut self, x: f32) -> u32 {
        self.constant(Val::float(x))
    }

    fn bin(&mut self, op: BinOp, a: u32, b: u32) -> u32 {
        self.emit(Op::Binary { op, a, b })
    }

    fn un(&mut self, op: UnOp, a: u32) -> u32 {
        self.emit(Op::Unary { op, a })
    }

    /// `Σ c_i · x^i` over `(power, coefficient)` terms, summed left to right
    /// as the GLSL does, with `x^i` as repeated products (`x^20` and `x^22`
    /// through `pow`).
    fn polynomial(&mut self, terms: &[(u32, f32)], x: u32) -> u32 {
        let mut sum: Option<u32> = None;
        for &(power, coeff) in terms {
            let p = match power {
                1 => x,
                2 => self.bin(BinOp::Mul, x, x),
                _ => {
                    let e = self.k(power as f32);
                    self.bin(BinOp::Pow, x, e)
                }
            };
            let c = self.k(coeff);
            let term = self.bin(BinOp::Mul, c, p);
            sum = Some(match sum {
                Some(s) => self.bin(BinOp::Add, s, term),
                None => term,
            });
        }
        sum.expect("at least one term")
    }

    /// `chiang_hair_roughness`: one of its three `vector2` outputs,
    /// `(longitudinal variance, azimuthal scale)` — pbrt-v3's `v` and `s`
    /// without the √(π/8), which the BSDF applies.
    pub(crate) fn compile_chiang_hair_roughness(
        &mut self,
        node: &Node,
        output: Option<&str>,
    ) -> u32 {
        let t = CHIANG_HAIR_ROUGHNESS;
        let longitudinal = self.hair_input(node, t, "longitudinal");
        let azimuthal = self.hair_input(node, t, "azimuthal");
        let (lo, hi) = (self.k(0.001), self.k(1.0));
        let lr = self.emit(Op::Clamp {
            a: longitudinal,
            low: lo,
            high: hi,
        });
        let ar = self.emit(Op::Clamp {
            a: azimuthal,
            low: lo,
            high: hi,
        });
        let v = self.polynomial(&[(1, 0.726), (2, 0.812), (20, 3.7)], lr);
        let v = self.bin(BinOp::Mul, v, v);
        let s = self.polynomial(&[(1, 0.265), (2, 1.194), (22, 5.372)], ar);
        let scale = match output {
            Some("roughness_TT") => Some("scale_TT"),
            Some("roughness_TRT") => Some("scale_TRT"),
            _ => None,
        };
        let v = match scale {
            Some(name) => {
                let k = self.hair_input(node, t, name);
                let v = self.bin(BinOp::Mul, v, k);
                self.bin(BinOp::Mul, v, k)
            }
            None => v,
        };
        self.emit(Op::Combine2 { a: v, b: s })
    }

    /// `chiang_hair_absorption_from_color`: the absorption coefficient that
    /// gives a dense groom `color`, by Chiang et al. 2016's fit over the
    /// azimuthal roughness β_n.
    pub(crate) fn compile_chiang_hair_absorption_from_color(&mut self, node: &Node) -> u32 {
        let t = CHIANG_HAIR_ABSORPTION_FROM_COLOR;
        let color = self.hair_input(node, t, "color");
        let b = self.hair_input(node, t, "azimuthal_roughness");
        // 5.969 − 0.215β + 2.532β² − 10.73β³ + 5.574β⁴ + 0.245β⁵, with the
        // powers formed as the GLSL forms them (b2, b2·β, b4 = b2², b4·β).
        let b2 = self.bin(BinOp::Mul, b, b);
        let b3 = self.bin(BinOp::Mul, b2, b);
        let b4 = self.bin(BinOp::Mul, b2, b2);
        let b5 = self.bin(BinOp::Mul, b4, b);
        let mut fac = self.k(5.969);
        for (p, c) in [
            (b, -0.215),
            (b2, 2.532),
            (b3, -10.73),
            (b4, 5.574),
            (b5, 0.245),
        ] {
            let k = self.k(c);
            let term = self.bin(BinOp::Mul, k, p);
            fac = self.bin(BinOp::Add, fac, term);
        }
        let (lo, hi) = (self.k(0.001), self.k(1.0));
        let c = self.emit(Op::Clamp {
            a: color,
            low: lo,
            high: hi,
        });
        let ln = self.un(UnOp::Ln, c);
        let sigma = self.bin(BinOp::Div, ln, fac);
        self.bin(BinOp::Mul, sigma, sigma)
    }

    /// `deon_hair_absorption_from_melanin`: d'Eon et al. 2011's mixture of
    /// eumelanin and pheomelanin, with the concentration in [0, 1) mapped to
    /// an amount by `−ln(1 − c)`.
    pub(crate) fn compile_deon_hair_absorption_from_melanin(&mut self, node: &Node) -> u32 {
        let t = DEON_HAIR_ABSORPTION_FROM_MELANIN;
        let concentration = self.hair_input(node, t, "melanin_concentration");
        let redness = self.hair_input(node, t, "melanin_redness");
        let eu_color = self.hair_input(node, t, "eumelanin_color");
        let pheo_color = self.hair_input(node, t, "pheomelanin_color");
        let (zero, one) = (self.k(0.0), self.k(1.0));
        let floor = self.k(0.0001);
        let rest = self.bin(BinOp::Sub, one, concentration);
        let rest = self.bin(BinOp::Max, rest, floor);
        let ln = self.un(UnOp::Ln, rest);
        let melanin = self.bin(BinOp::Sub, zero, ln);
        let not_red = self.bin(BinOp::Sub, one, redness);
        let eu = self.bin(BinOp::Mul, melanin, not_red);
        let pheo = self.bin(BinOp::Mul, melanin, redness);
        let ln_eu = self.un(UnOp::Ln, eu_color);
        let sigma_eu = self.bin(BinOp::Sub, zero, ln_eu);
        let ln_pheo = self.un(UnOp::Ln, pheo_color);
        let sigma_pheo = self.bin(BinOp::Sub, zero, ln_pheo);
        let a = self.bin(BinOp::Mul, eu, sigma_eu);
        let b = self.bin(BinOp::Mul, pheo, sigma_pheo);
        let sum = self.bin(BinOp::Add, a, b);
        self.bin(BinOp::Max, sum, zero)
    }
}
