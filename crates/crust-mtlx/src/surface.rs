//! MaterialX surface-shader nodes expanded into the closure tree of their
//! implementation nodegraphs.
//!
//! `open_pbr_surface`, `standard_surface` and `gltf_pbr` are, in MaterialX,
//! nodedefs whose implementations are nodegraphs over standalone BSDF nodes
//! (`libraries/bxdf/*.mtlx`, MaterialX 1.39). A document authoring one never
//! carries the implementation, so this module reproduces each graph **node for
//! node** — the same leaves, the same `layer` / `mix` / `multiply` in the same
//! order, and the same derived parameters, which are emitted as program ops so
//! they fold, optimise and JIT like any pattern graph. Each block below is
//! commented with the nodegraph node names it reproduces, so it can be checked
//! against the `.mtlx` line by line. NVIDIA Typhoon builds its surfaces the
//! same way (`MaterialXCpp/materials/*.cpp`); where Typhoon departs from the
//! graph (it omits OpenPBR's thin-walled subsurface branch), the graph wins.
//!
//! Every input takes its connection, else its authored value, else its
//! nodedef default from the tables here — generated from the nodedefs, which
//! are vendored under `tests/nodedefs/` and checked against these tables.
//!
//! What the tree cannot represent is reported rather than dropped silently:
//! opacity (no cutout), anisotropy rotation, glTF occlusion, and inputs the
//! MaterialX graphs themselves ignore.

use crate::bsdf::{
    Bsdf, Closure, Closures, DiffuseModel, EdfFalloff, Emission, Leaf, NodeId, ScatterMode,
    SheenMode, Slot, ThinFilm, Volume,
};
use crate::eval::{BinOp, Compiler, Op, UnOp};
use crate::parse::Node;
use crate::value::Val;
use std::collections::HashMap;

/// One nodedef input: its name, MaterialX type and default. `None` for an
/// input whose default is a geometric property (`Nworld`, `Tworld`) or that
/// declares no value.
#[derive(Clone, Copy, Debug)]
pub struct InputDef {
    pub name: &'static str,
    pub ty: &'static str,
    pub default: Option<Val>,
}

// Generated from `tests/nodedefs/*.mtlx` (MaterialX 1.39, Apache-2.0); the
// `nodedef_tables_match_materialx` test keeps them honest. `standard_surface`
// is version 1.0.1, which inherits 1.0.0 and overrides `base` and
// `base_color`.
#[rustfmt::skip]
mod tables {
    use super::InputDef;
    use crate::value::Val;
    /// `ND_open_pbr_surface_surfaceshader`'s inputs, in nodedef order.
    pub const OPEN_PBR_SURFACE: &[InputDef] = &[
        InputDef { name: "base_weight", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "base_color", ty: "color3", default: Some(Val::vec3(0.8, 0.8, 0.8)) },
        InputDef { name: "base_diffuse_roughness", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "base_metalness", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "specular_weight", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "specular_color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "specular_roughness", ty: "float", default: Some(Val::float(0.3)) },
        InputDef { name: "specular_ior", ty: "float", default: Some(Val::float(1.5)) },
        InputDef { name: "specular_roughness_anisotropy", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "transmission_weight", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "transmission_color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "transmission_depth", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "transmission_scatter", ty: "color3", default: Some(Val::vec3(0.0, 0.0, 0.0)) },
        InputDef { name: "transmission_scatter_anisotropy", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "transmission_dispersion_scale", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "transmission_dispersion_abbe_number", ty: "float", default: Some(Val::float(20.0)) },
        InputDef { name: "subsurface_weight", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "subsurface_color", ty: "color3", default: Some(Val::vec3(0.8, 0.8, 0.8)) },
        InputDef { name: "subsurface_radius", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "subsurface_radius_scale", ty: "color3", default: Some(Val::vec3(1.0, 0.5, 0.25)) },
        InputDef { name: "subsurface_scatter_anisotropy", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "fuzz_weight", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "fuzz_color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "fuzz_roughness", ty: "float", default: Some(Val::float(0.5)) },
        InputDef { name: "coat_weight", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "coat_color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "coat_roughness", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "coat_roughness_anisotropy", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "coat_ior", ty: "float", default: Some(Val::float(1.6)) },
        InputDef { name: "coat_darkening", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "thin_film_weight", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "thin_film_thickness", ty: "float", default: Some(Val::float(0.5)) },
        InputDef { name: "thin_film_ior", ty: "float", default: Some(Val::float(1.4)) },
        InputDef { name: "emission_luminance", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "emission_color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "geometry_opacity", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "geometry_thin_walled", ty: "boolean", default: Some(Val::float(0.0)) },
        InputDef { name: "geometry_normal", ty: "vector3", default: None },
        InputDef { name: "geometry_coat_normal", ty: "vector3", default: None },
        InputDef { name: "geometry_tangent", ty: "vector3", default: None },
        InputDef { name: "geometry_coat_tangent", ty: "vector3", default: None },
    ];
    
    /// `ND_standard_surface_surfaceshader`'s inputs, in nodedef order.
    pub const STANDARD_SURFACE: &[InputDef] = &[
        InputDef { name: "base", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "base_color", ty: "color3", default: Some(Val::vec3(0.8, 0.8, 0.8)) },
        InputDef { name: "diffuse_roughness", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "metalness", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "specular", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "specular_color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "specular_roughness", ty: "float", default: Some(Val::float(0.2)) },
        InputDef { name: "specular_IOR", ty: "float", default: Some(Val::float(1.5)) },
        InputDef { name: "specular_anisotropy", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "specular_rotation", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "transmission", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "transmission_color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "transmission_depth", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "transmission_scatter", ty: "color3", default: Some(Val::vec3(0.0, 0.0, 0.0)) },
        InputDef { name: "transmission_scatter_anisotropy", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "transmission_dispersion", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "transmission_extra_roughness", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "subsurface", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "subsurface_color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "subsurface_radius", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "subsurface_scale", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "subsurface_anisotropy", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "sheen", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "sheen_color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "sheen_roughness", ty: "float", default: Some(Val::float(0.3)) },
        InputDef { name: "coat", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "coat_color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "coat_roughness", ty: "float", default: Some(Val::float(0.1)) },
        InputDef { name: "coat_anisotropy", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "coat_rotation", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "coat_IOR", ty: "float", default: Some(Val::float(1.5)) },
        InputDef { name: "coat_normal", ty: "vector3", default: None },
        InputDef { name: "coat_affect_color", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "coat_affect_roughness", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "thin_film_thickness", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "thin_film_IOR", ty: "float", default: Some(Val::float(1.5)) },
        InputDef { name: "emission", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "emission_color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "opacity", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "thin_walled", ty: "boolean", default: Some(Val::float(0.0)) },
        InputDef { name: "normal", ty: "vector3", default: None },
        InputDef { name: "tangent", ty: "vector3", default: None },
    ];
    
    /// `ND_gltf_pbr_surfaceshader`'s inputs, in nodedef order.
    pub const GLTF_PBR: &[InputDef] = &[
        InputDef { name: "base_color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "metallic", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "roughness", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "normal", ty: "vector3", default: None },
        InputDef { name: "tangent", ty: "vector3", default: None },
        InputDef { name: "occlusion", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "transmission", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "specular", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "specular_color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "ior", ty: "float", default: Some(Val::float(1.5)) },
        InputDef { name: "alpha", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "alpha_mode", ty: "integer", default: Some(Val::float(0.0)) },
        InputDef { name: "alpha_cutoff", ty: "float", default: Some(Val::float(0.5)) },
        InputDef { name: "iridescence", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "iridescence_ior", ty: "float", default: Some(Val::float(1.3)) },
        InputDef { name: "iridescence_thickness", ty: "float", default: Some(Val::float(100.0)) },
        InputDef { name: "sheen_color", ty: "color3", default: Some(Val::vec3(0.0, 0.0, 0.0)) },
        InputDef { name: "sheen_roughness", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "clearcoat", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "clearcoat_roughness", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "clearcoat_normal", ty: "vector3", default: None },
        InputDef { name: "emissive", ty: "color3", default: Some(Val::vec3(0.0, 0.0, 0.0)) },
        InputDef { name: "emissive_strength", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "thickness", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "attenuation_distance", ty: "float", default: None },
        InputDef { name: "attenuation_color", ty: "color3", default: Some(Val::vec3(1.0, 1.0, 1.0)) },
        InputDef { name: "anisotropy_strength", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "anisotropy_rotation", ty: "float", default: Some(Val::float(0.0)) },
        InputDef { name: "dispersion", ty: "float", default: Some(Val::float(0.0)) },
    ];
    
}
pub use tables::{GLTF_PBR, OPEN_PBR_SURFACE, STANDARD_SURFACE};

/// Expands `node` — an `open_pbr_surface`, `standard_surface` or `gltf_pbr` —
/// into `out`.
pub fn build(c: &mut Compiler<'_>, node: &Node, out: &mut Closures) {
    let (defs, default_version): (&'static [InputDef], &str) = match node.category.as_str() {
        "open_pbr_surface" => (OPEN_PBR_SURFACE, "1.1"),
        "standard_surface" => (STANDARD_SURFACE, "1.0.1"),
        "gltf_pbr" => (GLTF_PBR, "2.0.1"),
        other => {
            c.unsupported.insert(other.to_string());
            return;
        }
    };
    let mut b = B {
        c,
        node,
        defs,
        memo: HashMap::new(),
        out,
    };
    if let Some(v) = b.version()
        && v != default_version
    {
        b.out.reported.insert(format!(
            "{} version {v} (built as the default version {default_version})",
            node.category
        ));
    }
    match node.category.as_str() {
        "open_pbr_surface" => open_pbr_surface(&mut b),
        "standard_surface" => standard_surface(&mut b),
        _ => gltf_pbr(&mut b),
    }
}

/// A builder over one surface node: its inputs by name, program ops, and the
/// tree under construction.
struct B<'x, 'a> {
    c: &'x mut Compiler<'a>,
    node: &'x Node,
    defs: &'static [InputDef],
    memo: HashMap<&'static str, Slot>,
    out: &'x mut Closures,
}

impl B<'_, '_> {
    fn version(&self) -> Option<&str> {
        self.node.version.as_deref()
    }

    fn def(&self, name: &str) -> Option<&'static InputDef> {
        self.defs.iter().find(|d| d.name == name)
    }

    /// An input: its connection, its authored value, or its nodedef default.
    fn get(&mut self, name: &'static str) -> Slot {
        if let Some(&s) = self.memo.get(name) {
            return s;
        }
        let default = self.def(name).and_then(|d| d.default).unwrap_or(Val::ZERO);
        let s = self.c.input_or(self.node, name, default);
        self.memo.insert(name, s);
        s
    }

    /// An input whose default is geometric (`Nworld` / `Tworld`): `None`
    /// unless authored.
    fn geom(&mut self, name: &'static str) -> Option<Slot> {
        self.c.optional_input(self.node, name)
    }

    /// Reports `name` when it is authored away from its nodedef default.
    fn report(&mut self, name: &'static str, why: &str) {
        let default = self.def(name).and_then(|d| d.default).unwrap_or(Val::ZERO);
        if crate::bsdf::authored_away(self.c, self.node, name, default) {
            self.out.reported.insert(format!("{name} ({why})"));
        }
    }

    fn k(&mut self, x: f32) -> Slot {
        self.c.constant(Val::float(x))
    }

    fn k3(&mut self, x: f32, y: f32, z: f32) -> Slot {
        self.c.constant(Val::vec3(x, y, z))
    }

    fn bin(&mut self, op: BinOp, a: Slot, b: Slot) -> Slot {
        self.c.emit(Op::Binary { op, a, b })
    }

    fn mul(&mut self, a: Slot, b: Slot) -> Slot {
        self.bin(BinOp::Mul, a, b)
    }

    fn add(&mut self, a: Slot, b: Slot) -> Slot {
        self.bin(BinOp::Add, a, b)
    }

    fn sub(&mut self, a: Slot, b: Slot) -> Slot {
        self.bin(BinOp::Sub, a, b)
    }

    fn div(&mut self, a: Slot, b: Slot) -> Slot {
        self.bin(BinOp::Div, a, b)
    }

    fn un(&mut self, op: UnOp, a: Slot) -> Slot {
        self.c.emit(Op::Unary { op, a })
    }

    fn mix(&mut self, fg: Slot, bg: Slot, m: Slot) -> Slot {
        self.c.emit(Op::Mix { fg, bg, m })
    }

    fn clamp(&mut self, a: Slot, lo: f32, hi: f32) -> Slot {
        let (low, high) = (self.k(lo), self.k(hi));
        self.c.emit(Op::Clamp { a, low, high })
    }

    fn convert(&mut self, a: Slot, arity: u8) -> Slot {
        self.c.emit(Op::Convert { a, arity })
    }

    fn extract(&mut self, a: Slot, index: usize) -> Slot {
        self.c.emit(Op::Extract { a, index })
    }

    /// MaterialX's `ifgreater`: `in1` where `value1 > value2`, else `in2`.
    ///
    /// Built from existing operators so the JIT needs nothing new:
    /// `m = max(sign(value2 − value1), 0)` is 1 exactly when `value1` is *not*
    /// greater — Rust's `signum(+0.0)` is 1, which is what makes equality pick
    /// `in2` — and `mix(fg = in2, bg = in1, m)` is exact at `m ∈ {0, 1}` for
    /// finite operands (every divide and log in the program is kept finite).
    fn gt(&mut self, v1: Slot, v2: Slot, in1: Slot, in2: Slot) -> Slot {
        let d = self.sub(v2, v1);
        let s = self.un(UnOp::Sign, d);
        let zero = self.k(0.0);
        let m = self.bin(BinOp::Max, s, zero);
        self.mix(in2, in1, m)
    }

    /// `1 − a`.
    fn one_minus(&mut self, a: Slot) -> Slot {
        let one = self.k(1.0);
        self.c.emit(Op::Invert { a, amount: one })
    }

    /// Whether `s` folds to a constant whose every lane is `x`.
    fn is(&self, s: Slot, x: f32) -> bool {
        self.c.fold(s).is_some_and(|v| {
            let lanes = (v.arity as usize).clamp(1, 4);
            v.v[..lanes].iter().all(|&c| c == x)
        })
    }

    // ---- the tree ----------------------------------------------------------

    /// A leaf, pruned when its weight folds to zero.
    fn leaf(
        &mut self,
        bsdf: Bsdf,
        weight: Slot,
        normal: Option<Slot>,
        tangent: Option<Slot>,
    ) -> Option<NodeId> {
        if self.is(weight, 0.0) {
            return None;
        }
        match &bsdf {
            Bsdf::Sheen {
                mode: SheenMode::Zeltner,
                ..
            } => {
                self.out
                    .reported
                    .insert("sheen_bsdf mode zeltner (evaluated as conty_kulla)".into());
            }
            Bsdf::Subsurface { .. } => {
                self.out
                    .reported
                    .insert("subsurface_bsdf (no random walk: shaded as diffuse)".into());
            }
            _ => {}
        }
        Some(self.push(Closure::Leaf(Leaf {
            bsdf,
            weight,
            normal,
            tangent,
        })))
    }

    fn push(&mut self, c: Closure) -> NodeId {
        self.out.nodes.push(c);
        (self.out.nodes.len() - 1) as NodeId
    }

    fn layer(&mut self, top: Option<NodeId>, base: Option<NodeId>) -> Option<NodeId> {
        match (top, base) {
            (Some(top), Some(base)) => Some(self.push(Closure::Layer { top, base })),
            (t, b) => t.or(b),
        }
    }

    /// `mix(fg, bg, m)`, pruned at a constant `m` of 0 or 1. A pruned branch
    /// keeps its share of the throughput ([`Closures::mix`]).
    fn mixc(&mut self, fg: Option<NodeId>, bg: Option<NodeId>, m: Slot) -> Option<NodeId> {
        if self.is(m, 0.0) {
            return bg;
        }
        if self.is(m, 1.0) {
            return fg;
        }
        self.out.mix(fg, bg, m)
    }

    /// `multiply(x, w)`, pruned at a constant `w` of 0 and elided at 1.
    fn multiply(&mut self, x: Option<NodeId>, w: Slot) -> Option<NodeId> {
        let x = x?;
        if self.is(w, 0.0) {
            return None;
        }
        if self.is(w, 1.0) {
            return Some(x);
        }
        Some(self.push(Closure::Multiply {
            input: x,
            weight: w,
        }))
    }

    /// A thin film, `None` when its thickness folds to zero.
    fn film(&mut self, thickness_nm: Slot, ior: Slot) -> Option<ThinFilm> {
        (!self.is(thickness_nm, 0.0)).then_some(ThinFilm {
            thickness: thickness_nm,
            ior,
        })
    }

    /// An EDF term, pruned when its weight folds to zero.
    fn emit_edf(&mut self, color: Slot, weight: Slot, falloff: Option<EdfFalloff>) {
        if self.is(weight, 0.0) || self.is(color, 0.0) {
            return;
        }
        self.out.emission.push(Emission {
            color,
            weight,
            falloff,
        });
    }

    // ---- shared helper graphs ----------------------------------------------

    /// `NG_open_pbr_anisotropy`: `α = r²`, `αx = α·√(2 / (1 + (1 − a)²))`,
    /// `αy = (1 − a)·αx`.
    fn open_pbr_anisotropy(&mut self, roughness: Slot, anisotropy: Slot) -> Slot {
        let inv = self.one_minus(anisotropy); // aniso_invert
        let inv_sq = self.mul(inv, inv); // aniso_invert_sq
        let one = self.k(1.0);
        let denom = self.add(inv_sq, one); // denom
        let two = self.k(2.0);
        let fraction = self.div(two, denom); // fraction
        let sqrt = self.un(UnOp::Sqrt, fraction); // sqrt
        let rough_sq = self.mul(roughness, roughness); // rough_sq
        let ax = self.mul(rough_sq, sqrt); // alpha_x
        let ay = self.mul(inv, ax); // alpha_y
        self.c.emit(Op::Combine2 { a: ax, b: ay }) // result
    }

    /// `ND_roughness_anisotropy` (`mx_roughness_anisotropy.glsl`):
    /// `α = clamp(r², ε, 1)`; with `aspect = √(1 − clamp(a, 0, 0.98))`,
    /// `(min(α / aspect, 1), α · aspect)` — which is `(α, α)` at `a ≤ 0`,
    /// so the GLSL's branch needs no select here.
    fn roughness_anisotropy(&mut self, roughness: Slot, anisotropy: Slot) -> Slot {
        let r2 = self.mul(roughness, roughness);
        let alpha = self.clamp(r2, 1e-8, 1.0);
        let a = self.clamp(anisotropy, 0.0, 0.98);
        let one_minus = self.one_minus(a);
        let aspect = self.un(UnOp::Sqrt, one_minus);
        let x0 = self.div(alpha, aspect);
        let one = self.k(1.0);
        let x = self.bin(BinOp::Min, x0, one);
        let y = self.mul(alpha, aspect);
        self.c.emit(Op::Combine2 { a: x, b: y })
    }

    /// `((ior − 1) / (ior + 1))²`.
    fn ior_to_f0(&mut self, ior: Slot) -> Slot {
        let one = self.k(1.0);
        let m = self.sub(ior, one);
        let p = self.add(one, ior);
        let r = self.div(m, p);
        self.mul(r, r)
    }
}

// ---------------------------------------------------------------------------
// open_pbr_surface — NG_open_pbr_surface_surfaceshader (OpenPBR 1.1)
// ---------------------------------------------------------------------------

fn open_pbr_surface(b: &mut B<'_, '_>) {
    b.report("geometry_opacity", "no cutout");
    b.report(
        "transmission_dispersion_scale",
        "ignored, as MaterialX's own graph does",
    );

    let normal = b.geom("geometry_normal");
    let tangent = b.geom("geometry_tangent");
    let coat_normal = b.geom("geometry_coat_normal");
    let coat_tangent = b.geom("geometry_coat_tangent");

    // Roughness: the coat broadens the base specular.
    let coat_roughness = b.get("coat_roughness");
    let specular_roughness = b.get("specular_roughness");
    let coat_weight = b.get("coat_weight");
    let four = b.k(4.0);
    let cr4 = b.bin(BinOp::Pow, coat_roughness, four); // coat_roughness_to_power_4
    let two = b.k(2.0);
    let two_cr4 = b.mul(cr4, two); // two_times_coat_roughness_to_power_4
    let sr4 = b.bin(BinOp::Pow, specular_roughness, four); // specular_roughness_to_power_4
    let sum = b.add(two_cr4, sr4); // add_coat_and_spec_roughnesses_to_power_4
    let one = b.k(1.0);
    let min1 = b.bin(BinOp::Min, one, sum); // min_1_add_coat_and_spec_roughnesses_to_power_4
    let quarter = b.k(0.25);
    let coat_affected = b.bin(BinOp::Pow, min1, quarter); // coat_affected_specular_roughness
    let effective = b.mix(coat_affected, specular_roughness, coat_weight); // effective_specular_roughness
    let aniso = b.get("specular_roughness_anisotropy");
    let main_roughness = b.open_pbr_anisotropy(effective, aniso); // main_roughness

    // Subsurface, thin-walled and not, built only when it is live: a
    // literal-zero `subsurface_weight` (the default) makes `opaque_base` the
    // diffuse alone.
    let subsurface_color = b.get("subsurface_color");
    let zero = b.k(0.0);
    let diffuse_roughness = b.get("base_diffuse_roughness");
    let one_w = b.k(1.0);
    let base_color = b.get("base_color");
    let bcn = b.bin(BinOp::Max, base_color, zero); // base_color_nonnegative
    let base_weight = b.get("base_weight");
    let diffuse = b.leaf(
        Bsdf::Diffuse {
            model: DiffuseModel::Eon,
            color: bcn,
            roughness: diffuse_roughness,
        },
        base_weight,
        normal,
        None,
    ); // diffuse_bsdf
    let thin_walled = b.get("geometry_thin_walled");
    let subsurface_weight = b.get("subsurface_weight");
    let opaque_base = if b.is(subsurface_weight, 0.0) {
        diffuse
    } else {
        let ssc = b.bin(BinOp::Max, subsurface_color, zero); // subsurface_color_nonnegative
        let sss_refl_bsdf = b.leaf(
            Bsdf::Diffuse {
                model: DiffuseModel::OrenNayar,
                color: ssc,
                roughness: diffuse_roughness,
            },
            one_w,
            normal,
            None,
        ); // subsurface_thin_walled_reflection_bsdf
        let ss_aniso = b.get("subsurface_scatter_anisotropy");
        let one_minus_aniso = b.one_minus(ss_aniso); // one_minus_subsurface_scatter_anisotropy
        let brdf_factor = b.mul(subsurface_color, one_minus_aniso); // subsurface_thin_walled_brdf_factor
        let sss_refl = b.multiply(sss_refl_bsdf, brdf_factor); // subsurface_thin_walled_reflection
        let sss_trans_bsdf = b.leaf(Bsdf::Translucent { color: ssc }, one_w, normal, None); // subsurface_thin_walled_transmission_bsdf
        let one = b.k(1.0);
        let one_plus_aniso = b.add(one, ss_aniso); // one_plus_subsurface_scatter_anisotropy
        let btdf_factor = b.mul(subsurface_color, one_plus_aniso); // subsurface_thin_walled_btdf_factor
        let sss_trans = b.multiply(sss_trans_bsdf, btdf_factor); // subsurface_thin_walled_transmission
        let half = b.k(0.5);
        let sss_thin = b.mixc(sss_refl, sss_trans, half); // subsurface_thin_walled
        let radius_scale = b.get("subsurface_radius_scale");
        let radius = b.get("subsurface_radius");
        let radius_scaled = b.mul(radius_scale, radius); // subsurface_radius_scaled
        let sss_bsdf = b.leaf(
            Bsdf::Subsurface {
                color: ssc,
                radius: radius_scaled,
                anisotropy: ss_aniso,
            },
            one_w,
            normal,
            None,
        ); // subsurface_bsdf
        let selector = b.convert(thin_walled, 1); // subsurface_selector
        let selected = b.mixc(sss_thin, sss_bsdf, selector); // selected_subsurface
        b.mixc(selected, diffuse, subsurface_weight) // opaque_base
    };

    // The transmission volume.
    let transmission_color = b.get("transmission_color");
    let tcv = b.convert(transmission_color, 3); // transmission_color_vector
    let tcl = b.un(UnOp::Ln, tcv); // transmission_color_ln
    let minus_one = b.k(-1.0);
    let ext_den = b.mul(tcl, minus_one); // extinction_coeff_denom
    let depth = b.get("transmission_depth");
    let depth_v = b.convert(depth, 3); // transmission_depth_vector
    let extinction = b.div(ext_den, depth_v); // extinction_coeff
    let scatter = b.get("transmission_scatter");
    let scatter_v = b.convert(scatter, 3); // transmission_scatter_vector
    let scattering = b.div(scatter_v, depth_v); // scattering_coeff
    let absorption = b.sub(extinction, scattering); // absorption_coeff
    let ax = b.extract(absorption, 0);
    let ay = b.extract(absorption, 1);
    let az = b.extract(absorption, 2);
    let min_xy = b.bin(BinOp::Min, ax, ay);
    let amin = b.bin(BinOp::Min, min_xy, az); // absorption_coeff_min
    let amin_v = b.convert(amin, 3);
    let shifted = b.sub(absorption, amin_v); // absorption_coeff_shifted
    let if_shifted = b.gt(zero, amin, shifted, absorption); // if_absorption_coeff_shifted
    let zero3 = b.k3(0.0, 0.0, 0.0);
    let vol_abs = b.gt(depth, zero, if_shifted, zero3); // if_volume_absorption
    let vol_scat = b.gt(depth, zero, scattering, zero3); // if_volume_scattering
    let vol_aniso = b.get("transmission_scatter_anisotropy");
    let transmission_weight = b.get("transmission_weight");
    let one = b.k(1.0);
    if !b.is(transmission_weight, 0.0) {
        b.out.volume = Some(Volume {
            absorption: vol_abs,
            scattering: vol_scat,
            anisotropy: vol_aniso,
        }); // dielectric_volume
    }

    // The dielectric interface's IOR: relative to the coat, and modulated by
    // specular_weight through F0.
    let tf_thickness = b.get("thin_film_thickness");
    let thousand = b.k(1000.0);
    let tf_nm = b.mul(tf_thickness, thousand); // thin_film_thickness_nm
    let specular_ior = b.get("specular_ior");
    let coat_ior = b.get("coat_ior");
    let s2c = b.div(specular_ior, coat_ior); // specular_to_coat_ior_ratio
    let c2s = b.div(coat_ior, specular_ior); // coat_to_specular_ior_ratio
    let tir_fix = b.gt(s2c, one, s2c, c2s); // specular_to_coat_ior_ratio_tir_fix
    let eta_s = b.mix(tir_fix, specular_ior, coat_weight); // eta_s
    let em1 = b.sub(eta_s, one); // eta_s_minus_one
    let ep1 = b.add(eta_s, one); // eta_s_plus_one
    let f0_sqrt = b.div(em1, ep1); // specular_F0_sqrt
    let f0 = b.mul(f0_sqrt, f0_sqrt); // specular_F0
    let specular_weight = b.get("specular_weight");
    let scaled_f0 = b.mul(specular_weight, f0); // scaled_specular_F0
    let scaled_f0c = b.clamp(scaled_f0, 0.0, 0.99999); // scaled_specular_F0_clamped
    let sqrt_f0 = b.un(UnOp::Sqrt, scaled_f0c); // sqrt_scaled_specular_F0
    let sign = b.un(UnOp::Sign, em1); // sign_eta_s_minus_one
    let eps = b.mul(sign, sqrt_f0); // modulated_eta_s_epsilon
    let one_minus_eps = b.one_minus(eps); // one_minus_modulated_eta_s_epsilon
    let one_plus_eps = b.add(one, eps); // one_plus_modulated_eta_s_epsilon
    let modulated_eta = b.div(one_plus_eps, one_minus_eps); // modulated_eta_s

    // Transmission over the opaque base, then the reflection over that.
    let white = b.k3(1.0, 1.0, 1.0);
    let substrate = if b.is(transmission_weight, 0.0) {
        opaque_base
    } else {
        let t_tint = b.gt(depth, zero, white, transmission_color); // if_transmission_tint
        let d_trans = b.leaf(
            Bsdf::Dielectric {
                tint: t_tint,
                ior: modulated_eta,
                roughness: main_roughness,
                mode: ScatterMode::T,
                thin_film: None,
                abbe: None,
            },
            one_w,
            normal,
            tangent,
        ); // dielectric_transmission (+ dielectric_volume_transmission)
        b.mixc(d_trans, opaque_base, transmission_weight) // dielectric_substrate
    };
    let specular_color = b.get("specular_color");
    let d_refl = b.leaf(
        Bsdf::Dielectric {
            tint: specular_color,
            ior: modulated_eta,
            roughness: main_roughness,
            mode: ScatterMode::R,
            thin_film: None,
            abbe: None,
        },
        one_w,
        normal,
        tangent,
    ); // dielectric_reflection
    let tf_ior = b.get("thin_film_ior");
    let thin_film_weight = b.get("thin_film_weight");
    let d_refl_mix = if b.is(thin_film_weight, 0.0) {
        d_refl
    } else {
        let film = b.film(tf_nm, tf_ior);
        let d_refl_tf = b.leaf(
            Bsdf::Dielectric {
                tint: specular_color,
                ior: modulated_eta,
                roughness: main_roughness,
                mode: ScatterMode::R,
                thin_film: film,
                abbe: None,
            },
            one_w,
            normal,
            tangent,
        ); // dielectric_reflection_tf
        b.mixc(d_refl_tf, d_refl, thin_film_weight) // dielectric_reflection_tf_mix
    };
    let dielectric_base = b.layer(d_refl_mix, substrate); // dielectric_base

    // The metal.
    let metal_refl = b.mul(base_color, base_weight); // metal_reflectivity
    let metal_edge = b.mul(specular_color, specular_weight); // metal_edgecolor
    let five = b.k(5.0);
    let metalness = b.get("base_metalness");
    let metal_mix = if b.is(metalness, 0.0) {
        None
    } else {
        let metal = b.leaf(
            Bsdf::Schlick {
                color0: metal_refl,
                color82: metal_edge,
                color90: white,
                exponent: five,
                roughness: main_roughness,
                mode: ScatterMode::R,
                thin_film: None,
            },
            specular_weight,
            normal,
            tangent,
        ); // metal_bsdf
        if b.is(thin_film_weight, 0.0) {
            metal
        } else {
            let film = b.film(tf_nm, tf_ior);
            let metal_tf = b.leaf(
                Bsdf::Schlick {
                    color0: metal_refl,
                    color82: metal_edge,
                    color90: white,
                    exponent: five,
                    roughness: main_roughness,
                    mode: ScatterMode::R,
                    thin_film: film,
                },
                specular_weight,
                normal,
                tangent,
            ); // metal_bsdf_tf
            b.mixc(metal_tf, metal, thin_film_weight) // metal_bsdf_tf_mix
        }
    };
    let base_substrate = b.mixc(metal_mix, dielectric_base, metalness); // base_substrate

    // Coat darkening and tint.
    let coat_f0 = b.ior_to_f0(coat_ior); // coat_ior_to_F0
    let one_minus_coat_f0 = b.one_minus(coat_f0); // one_minus_coat_F0
    let coat_ior_sq = b.mul(coat_ior, coat_ior); // coat_ior_sqr
    let omf0_eta2 = b.div(one_minus_coat_f0, coat_ior_sq); // one_minus_coat_F0_over_eta2
    let k_coat = b.one_minus(omf0_eta2); // Kcoat
    let e_metal = b.mul(base_color, specular_weight); // Emetal
    let e_diel = b.mix(subsurface_color, base_color, subsurface_weight); // Edielectric
    let e_base = b.mix(e_metal, e_diel, metalness); // Ebase
    let ebk = b.mul(e_base, k_coat); // Ebase_Kcoat
    let one_minus_k = b.one_minus(k_coat); // one_minus_Kcoat
    let one_minus_ebk = b.sub(white, ebk); // one_minus_Ebase_Kcoat
    let omk3 = b.convert(one_minus_k, 3); // one_minus_Kcoat_color
    let darkening = b.div(omk3, one_minus_ebk); // base_darkening
    let coat_darkening = b.get("coat_darkening");
    let cwd = b.mul(coat_weight, coat_darkening); // coat_weight_times_coat_darkening
    let mod_dark = b.mix(darkening, white, cwd); // modulated_base_darkening
    let darkened = b.multiply(base_substrate, mod_dark); // darkened_base_substrate
    let coat_color = b.get("coat_color");
    let coat_att = b.mix(coat_color, white, coat_weight); // coat_attenuation
    let attenuated = b.multiply(darkened, coat_att); // coat_substrate_attenuated
    let coat_aniso = b.get("coat_roughness_anisotropy");
    let coat_rough_v = b.open_pbr_anisotropy(coat_roughness, coat_aniso); // coat_roughness_vector
    let coat = b.leaf(
        Bsdf::Dielectric {
            tint: white,
            ior: coat_ior,
            roughness: coat_rough_v,
            mode: ScatterMode::R,
            thin_film: None,
            abbe: None,
        },
        coat_weight,
        coat_normal,
        coat_tangent,
    ); // coat_bsdf
    let coat_layer = b.layer(coat, attenuated); // coat_layer
    let fuzz_color = b.get("fuzz_color");
    let fuzz_roughness = b.get("fuzz_roughness");
    let fuzz_weight = b.get("fuzz_weight");
    let fuzz = b.leaf(
        Bsdf::Sheen {
            color: fuzz_color,
            roughness: fuzz_roughness,
            mode: SheenMode::Zeltner,
        },
        fuzz_weight,
        normal,
        None,
    ); // fuzz_bsdf
    b.out.root = b.layer(fuzz, coat_layer); // fuzz_layer

    // Emission: uncoated, and through the coat's Fresnel.
    let emission_color = b.get("emission_color");
    let luminance = b.get("emission_luminance");
    let ew = b.mul(emission_color, luminance); // emission_weight
    let uncoated_w = b.one_minus(coat_weight);
    b.emit_edf(ew, uncoated_w, None); // uncoated_emission_edf (bg of emission_edf)
    let coated_w = b.mul(coat_color, coat_weight); // coat_tinted_emission_edf · mix
    let c0 = b.convert(one_minus_coat_f0, 3); // one_minus_coat_F0_color
    let falloff = EdfFalloff {
        color0: c0,
        color90: zero3,
        exponent: five,
    };
    b.emit_edf(ew, coated_w, Some(falloff)); // coated_emission_edf (fg)

    b.out.thin_walled = Some(thin_walled);
}

// ---------------------------------------------------------------------------
// standard_surface — NG_standard_surface_surfaceshader_100
// ---------------------------------------------------------------------------

fn standard_surface(b: &mut B<'_, '_>) {
    b.report("opacity", "no cutout");
    b.report("specular_rotation", "anisotropy rotation");
    b.report("coat_rotation", "anisotropy rotation");
    for (name, why) in [
        (
            "transmission_depth",
            "ignored, as MaterialX's own graph does",
        ),
        (
            "transmission_scatter",
            "ignored, as MaterialX's own graph does",
        ),
        (
            "transmission_dispersion",
            "ignored, as MaterialX's own graph does",
        ),
    ] {
        b.report(name, why);
    }

    let normal = b.geom("normal");
    let coat_normal = b.geom("coat_normal");
    // `main_tangent` / `coat_tangent` rotate the tangent by
    // `specular_rotation` / `coat_rotation` when anisotropic; the rotation is
    // reported above and the authored tangent used as is.
    let tangent = b.geom("tangent");

    let coat_affect_roughness = b.get("coat_affect_roughness");
    let coat = b.get("coat");
    let coat_roughness = b.get("coat_roughness");
    let m1 = b.mul(coat_affect_roughness, coat); // coat_affect_roughness_multiply1
    let m2 = b.mul(m1, coat_roughness); // coat_affect_roughness_multiply2
    let specular_roughness = b.get("specular_roughness");
    let one = b.k(1.0);
    let coat_affected = b.mix(one, specular_roughness, m2); // coat_affected_roughness
    let specular_anisotropy = b.get("specular_anisotropy");
    let main_roughness = b.roughness_anisotropy(coat_affected, specular_anisotropy); // main_roughness
    let extra = b.get("transmission_extra_roughness");
    let tr_add = b.add(specular_roughness, extra); // transmission_roughness_add
    let tr_clamped = b.clamp(tr_add, 0.0, 1.0); // transmission_roughness_clamped
    let coat_affected_tr = b.mix(one, tr_clamped, m2); // coat_affected_transmission_roughness
    let transmission_roughness = b.roughness_anisotropy(coat_affected_tr, specular_anisotropy); // transmission_roughness

    let coat_clamped = b.clamp(coat, 0.0, 1.0); // coat_clamped
    let coat_affect_color = b.get("coat_affect_color");
    let cg_m = b.mul(coat_clamped, coat_affect_color); // coat_gamma_multiply
    let coat_gamma = b.add(cg_m, one); // coat_gamma
    let zero = b.k(0.0);
    let base_color = b.get("base_color");
    let bcn = b.bin(BinOp::Max, base_color, zero); // base_color_nonnegative
    let diffuse_color = b.bin(BinOp::Pow, bcn, coat_gamma); // coat_affected_diffuse_color
    let subsurface_color = b.get("subsurface_color");
    let scn = b.bin(BinOp::Max, subsurface_color, zero); // subsurface_color_nonnegative
    let sss_color = b.bin(BinOp::Pow, scn, coat_gamma); // coat_affected_subsurface_color

    let base = b.get("base");
    let diffuse_roughness = b.get("diffuse_roughness");
    let diffuse = b.leaf(
        Bsdf::Diffuse {
            model: DiffuseModel::OrenNayar,
            color: diffuse_color,
            roughness: diffuse_roughness,
        },
        base,
        normal,
        None,
    ); // diffuse_bsdf
    let subsurface = b.get("subsurface");
    let sss_mix = if b.is(subsurface, 0.0) {
        diffuse
    } else {
        let one_w = b.k(1.0);
        let translucent = b.leaf(Bsdf::Translucent { color: sss_color }, one_w, normal, None); // translucent_bsdf
        let radius = b.get("subsurface_radius");
        let scale = b.get("subsurface_scale");
        let radius_scaled = b.mul(radius, scale); // subsurface_radius_scaled
        let ss_aniso = b.get("subsurface_anisotropy");
        let sss = b.leaf(
            Bsdf::Subsurface {
                color: sss_color,
                radius: radius_scaled,
                anisotropy: ss_aniso,
            },
            one_w,
            normal,
            None,
        ); // subsurface_bsdf
        let thin_walled = b.get("thin_walled");
        let selector = b.convert(thin_walled, 1); // subsurface_selector
        let selected = b.mixc(translucent, sss, selector); // selected_subsurface_bsdf
        b.mixc(selected, diffuse, subsurface) // subsurface_mix
    };
    let sheen_w = b.get("sheen");
    let sheen_color = b.get("sheen_color");
    let sheen_roughness = b.get("sheen_roughness");
    let sheen = b.leaf(
        Bsdf::Sheen {
            color: sheen_color,
            roughness: sheen_roughness,
            mode: SheenMode::ContyKulla,
        },
        sheen_w,
        normal,
        None,
    ); // sheen_bsdf
    let sheen_layer = b.layer(sheen, sss_mix); // sheen_layer
    let transmission = b.get("transmission");
    let transmission_color = b.get("transmission_color");
    let specular_ior = b.get("specular_IOR");
    let one_w = b.k(1.0);
    let trans_mix = if b.is(transmission, 0.0) {
        sheen_layer
    } else {
        let trans = b.leaf(
            Bsdf::Dielectric {
                tint: transmission_color,
                ior: specular_ior,
                roughness: transmission_roughness,
                mode: ScatterMode::T,
                thin_film: None,
                abbe: None,
            },
            one_w,
            normal,
            tangent,
        ); // transmission_bsdf
        b.mixc(trans, sheen_layer, transmission) // transmission_mix
    };
    let specular = b.get("specular");
    let specular_color = b.get("specular_color");
    let tf_thickness = b.get("thin_film_thickness");
    let tf_ior = b.get("thin_film_IOR");
    let film = b.film(tf_thickness, tf_ior);
    let spec = b.leaf(
        Bsdf::Dielectric {
            tint: specular_color,
            ior: specular_ior,
            roughness: main_roughness,
            mode: ScatterMode::R,
            thin_film: film,
            abbe: None,
        },
        specular,
        normal,
        tangent,
    ); // specular_bsdf
    let spec_layer = b.layer(spec, trans_mix); // specular_layer
    let metalness = b.get("metalness");
    let metal_mix = if b.is(metalness, 0.0) {
        spec_layer
    } else {
        let metal_refl = b.mul(base_color, base); // metal_reflectivity
        let metal_edge = b.mul(specular_color, specular); // metal_edgecolor
        let n = b.c.emit(Op::ArtisticIor {
            reflectivity: metal_refl,
            edge: metal_edge,
            extinction: false,
        }); // artistic_ior.ior
        let k = b.c.emit(Op::ArtisticIor {
            reflectivity: metal_refl,
            edge: metal_edge,
            extinction: true,
        }); // artistic_ior.extinction
        let metal = b.leaf(
            Bsdf::Conductor {
                ior: n,
                extinction: k,
                roughness: main_roughness,
                thin_film: film,
            },
            one_w,
            normal,
            tangent,
        ); // metal_bsdf
        b.mixc(metal, spec_layer, metalness) // metalness_mix
    };
    let coat_color = b.get("coat_color");
    let white = b.k3(1.0, 1.0, 1.0);
    let coat_att = b.mix(coat_color, white, coat); // coat_attenuation
    let attenuated = b.multiply(metal_mix, coat_att); // thin_film_layer_attenuated
    let coat_anisotropy = b.get("coat_anisotropy");
    let coat_rough_v = b.roughness_anisotropy(coat_roughness, coat_anisotropy); // coat_roughness_vector
    let coat_ior = b.get("coat_IOR");
    let coat_bsdf = b.leaf(
        Bsdf::Dielectric {
            tint: white,
            ior: coat_ior,
            roughness: coat_rough_v,
            mode: ScatterMode::R,
            thin_film: None,
            abbe: None,
        },
        coat,
        coat_normal,
        tangent,
    ); // coat_bsdf
    b.out.root = b.layer(coat_bsdf, attenuated); // coat_layer

    // Emission, uncoated and through the coat's Fresnel.
    let coat_f0 = b.ior_to_f0(coat_ior); // coat_ior_to_F0
    let one_minus_f0 = b.one_minus(coat_f0); // one_minus_coat_ior_to_F0
    let emission_color = b.get("emission_color");
    let emission = b.get("emission");
    let ew = b.mul(emission_color, emission); // emission_weight
    let uncoated_w = b.one_minus(coat);
    b.emit_edf(ew, uncoated_w, None); // emission_edf (bg of blended_coat_emission_edf)
    let coated_w = b.mul(coat_color, coat); // coat_tinted_emission_edf · mix
    let c0 = b.convert(one_minus_f0, 3); // emission_color0
    let zero3 = b.k3(0.0, 0.0, 0.0);
    let five = b.k(5.0);
    b.emit_edf(
        ew,
        coated_w,
        Some(EdfFalloff {
            color0: c0,
            color90: zero3,
            exponent: five,
        }),
    ); // coat_emission_edf (fg)

    let thin_walled = b.get("thin_walled");
    b.out.thin_walled = Some(thin_walled);
}

// ---------------------------------------------------------------------------
// gltf_pbr — IMPL_gltf_pbr_surfaceshader (glTF PBR 2.0.1)
// ---------------------------------------------------------------------------

fn gltf_pbr(b: &mut B<'_, '_>) {
    b.report("alpha", "no cutout");
    b.report("alpha_mode", "no cutout");
    b.report("anisotropy_rotation", "anisotropy rotation");
    b.report("occlusion", "a path tracer computes its own");
    b.report("dispersion", "ignored, as MaterialX's own graph does");
    b.report("thickness", "ignored, as MaterialX's own graph does");

    let normal = b.geom("normal");
    // `selected_tangent` rotates the tangent by `anisotropy_rotation`; the
    // rotation is reported above.
    let tangent = b.geom("tangent");
    let clearcoat_normal = b.geom("clearcoat_normal");

    // The volume.
    let transmission = b.get("transmission");
    if !b.is(transmission, 0.0) {
        let ac = b.get("attenuation_color");
        let acv = b.convert(ac, 3); // attenuation_color_vec
        let ln = b.un(UnOp::Ln, acv); // ln_attenuation_color_vec
        let dist = b.get("attenuation_distance");
        let zero = b.k(0.0);
        let one = b.k(1.0);
        let safe = b.gt(dist, zero, dist, one); // safe_attenuation_distance
        let over = b.div(ln, safe); // ln_attenuation_color_vec_over_distance
        let minus_one = b.k(-1.0);
        let coeff = b.mul(over, minus_one); // attenuation_coeff
        let zero3 = b.k3(0.0, 0.0, 0.0);
        let g = b.k(0.0);
        b.out.volume = Some(Volume {
            absorption: coeff,
            scattering: zero3,
            anisotropy: g,
        }); // isotropic_volume
    }

    // The dielectric's Fresnel as a generalized Schlick.
    let ior = b.get("ior");
    let f0_ior = b.ior_to_f0(ior); // dielectric_f0_from_ior
    let specular_color = b.get("specular_color");
    let f0_sc = b.mul(specular_color, f0_ior); // dielectric_f0_from_ior_specular_color
    let one = b.k(1.0);
    let f0_cl = b.bin(BinOp::Min, f0_sc, one); // clamped_dielectric_f0_from_ior_specular_color
    let specular = b.get("specular");
    let f0 = b.mul(f0_cl, specular); // dielectric_f0
    let white = b.k3(1.0, 1.0, 1.0);
    let f90 = b.mul(white, specular); // dielectric_f90

    // Roughness.
    let roughness = b.get("roughness");
    let alpha = b.mul(roughness, roughness); // alpha_roughness
    let strength = b.get("anisotropy_strength");
    let s2 = b.mul(strength, strength); // strength_2
    let at = b.mix(one, alpha, s2); // at
    let at_c = b.clamp(at, 0.00001, 1.0); // clamped_at
    let ab_c = b.clamp(alpha, 0.00001, 1.0); // clamped_ab
    let ruv = b.c.emit(Op::Combine2 { a: at_c, b: ab_c }); // roughness_uv

    let base_color = b.get("base_color");
    let zero = b.k(0.0);
    let one_w = b.k(1.0);
    let diffuse = b.leaf(
        Bsdf::Diffuse {
            model: DiffuseModel::OrenNayar,
            color: base_color,
            roughness: zero,
        },
        one_w,
        normal,
        None,
    ); // diffuse_bsdf
    let trans_mix = if b.is(transmission, 0.0) {
        diffuse
    } else {
        let trans = b.leaf(
            Bsdf::Dielectric {
                tint: base_color,
                ior,
                roughness: ruv,
                mode: ScatterMode::T,
                thin_film: None,
                abbe: None,
            },
            one_w,
            normal,
            tangent,
        ); // transmission_bsdf (+ volume_transmission_bsdf)
        b.mixc(trans, diffuse, transmission) // transmission_mix
    };
    let five = b.k(5.0);
    let refl = b.leaf(
        Bsdf::Schlick {
            color0: f0,
            color82: white,
            color90: f90,
            exponent: five,
            roughness: ruv,
            mode: ScatterMode::R,
            thin_film: None,
        },
        one_w,
        normal,
        tangent,
    ); // reflection_bsdf
    let iridescence = b.get("iridescence");
    let irid_thickness = b.get("iridescence_thickness");
    let irid_ior = b.get("iridescence_ior");
    let mix_irid = if b.is(iridescence, 0.0) {
        refl
    } else {
        let film = b.film(irid_thickness, irid_ior);
        let tf_refl = b.leaf(
            Bsdf::Schlick {
                color0: f0,
                color82: white,
                color90: f90,
                exponent: five,
                roughness: ruv,
                mode: ScatterMode::R,
                thin_film: film,
            },
            one_w,
            normal,
            tangent,
        ); // tf_reflection_bsdf
        b.mixc(tf_refl, refl, iridescence) // mix_iridescent_dielectric_reflection
    };
    let irid_diel = b.layer(mix_irid, trans_mix); // iridescent_dielectric_bsdf
    let metallic = b.get("metallic");
    let metal_mix = if b.is(metallic, 0.0) {
        None
    } else {
        let metal = b.leaf(
            Bsdf::Schlick {
                color0: base_color,
                color82: white,
                color90: white,
                exponent: five,
                roughness: ruv,
                mode: ScatterMode::R,
                thin_film: None,
            },
            one_w,
            normal,
            tangent,
        ); // metal_bsdf
        if b.is(iridescence, 0.0) {
            metal
        } else {
            let film = b.film(irid_thickness, irid_ior);
            let tf_metal = b.leaf(
                Bsdf::Schlick {
                    color0: base_color,
                    color82: white,
                    color90: white,
                    exponent: five,
                    roughness: ruv,
                    mode: ScatterMode::R,
                    thin_film: film,
                },
                one_w,
                normal,
                tangent,
            ); // tf_metal_bsdf
            b.mixc(tf_metal, metal, iridescence) // mix_iridescent_metal_bsdf
        }
    };
    let base_mix = b.mixc(metal_mix, irid_diel, metallic); // base_mix

    let sheen_color = b.get("sheen_color");
    let r = b.extract(sheen_color, 0);
    let g = b.extract(sheen_color, 1);
    let bl = b.extract(sheen_color, 2);
    let max_rg = b.bin(BinOp::Max, r, g); // sheen_color_max_rg
    let intensity = b.bin(BinOp::Max, max_rg, bl); // sheen_intensity
    let sheen_roughness = b.get("sheen_roughness");
    let sheen_rough_sq = b.mul(sheen_roughness, sheen_roughness); // sheen_roughness_sq
    let sheen_norm = b.div(sheen_color, intensity); // sheen_color_normalized
    let sheen = b.leaf(
        Bsdf::Sheen {
            color: sheen_norm,
            roughness: sheen_rough_sq,
            mode: SheenMode::ContyKulla,
        },
        intensity,
        normal,
        None,
    ); // sheen_bsdf
    let sheen_layer = b.layer(sheen, base_mix); // sheen_layer

    let cc_roughness = b.get("clearcoat_roughness");
    let cc_rough_v = b.roughness_anisotropy(cc_roughness, zero); // clearcoat_roughness_uv
    let clearcoat = b.get("clearcoat");
    let cc_ior = b.k(1.5);
    let cc = b.leaf(
        Bsdf::Dielectric {
            tint: white,
            ior: cc_ior,
            roughness: cc_rough_v,
            mode: ScatterMode::R,
            thin_film: None,
            abbe: None,
        },
        clearcoat,
        clearcoat_normal,
        tangent,
    ); // clearcoat_bsdf
    b.out.root = b.layer(cc, sheen_layer); // clearcoat_layer

    let emissive = b.get("emissive");
    let strength_e = b.get("emissive_strength");
    let ec = b.mul(emissive, strength_e); // emission_color
    let one_e = b.k(1.0);
    b.emit_edf(ec, one_e, None); // emission
}
