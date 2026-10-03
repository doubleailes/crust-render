//! MaterialX surface-shader nodes expanded into the closure tree of their
//! implementation nodegraphs.
//!
//! `open_pbr_surface`, `standard_surface` and `gltf_pbr` are, in MaterialX,
//! nodedefs whose implementations are nodegraphs over standalone BSDF nodes
//! (`libraries/bxdf/*.mtlx`, MaterialX 1.39). A document authoring one never
//! carries the implementation, so this module reproduces each graph **node for
//! node** — the same leaves, the same `layer` / `mix` / `multiply` in the same
//! order, and the same derived parameters, which are emitted as program ops so
//! they fold, optimise and JIT like any pattern graph. Each translator (one
//! submodule per node) is commented with the nodegraph node names it
//! reproduces, so it can be checked against the `.mtlx` line by line. NVIDIA
//! Typhoon builds its surfaces the same way (`MaterialXCpp/materials/*.cpp`);
//! where Typhoon departs from the graph (it omits OpenPBR's thin-walled
//! subsurface branch), the graph wins.
//!
//! Every input takes its connection, else its authored value, else its
//! nodedef default from the tables here — generated from the nodedefs, which
//! are vendored under `tests/nodedefs/` and checked against these tables.
//!
//! Two parts of each graph sit outside the closure tree and are carried beside
//! it: the `surface` node's `opacity` ([`Closures::opacity`]), and the
//! `rotate3d` a graph applies to its tangent, which becomes the angle a leaf's
//! frame is turned by ([`Leaf::rotation`]).
//!
//! What the tree cannot represent is reported rather than dropped silently:
//! glTF occlusion, and inputs the MaterialX graphs themselves ignore.

mod gltf_pbr;
mod open_pbr;
mod standard_surface;

use crate::bsdf::{
    Bsdf, Closure, Closures, EdfFalloff, Emission, Leaf, NodeId, SheenMode, Slot, ThinFilm,
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
        "open_pbr_surface" => open_pbr::open_pbr_surface(&mut b),
        "standard_surface" => standard_surface::standard_surface(&mut b),
        _ => gltf_pbr::gltf_pbr(&mut b),
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
    /// `m = max(sign(value1 − value2), 0)` is 1 exactly when `value1` is
    /// greater — MaterialX's `sign(0)` is 0, and `x − x` is `+0`, which is
    /// what makes equality pick `in2` — and `mix(fg = in1, bg = in2, m)` is
    /// exact at `m ∈ {0, 1}` for finite operands (every divide and log in the
    /// program is kept finite).
    fn gt(&mut self, v1: Slot, v2: Slot, in1: Slot, in2: Slot) -> Slot {
        let d = self.sub(v1, v2);
        let s = self.un(UnOp::Sign, d);
        let zero = self.k(0.0);
        let m = self.bin(BinOp::Max, s, zero);
        self.mix(in1, in2, m)
    }

    /// MaterialX's `ifgreatereq`: `in1` where `value1 ≥ value2`, else `in2` —
    /// [`B::gt`] with its operands swapped, which is exact for the same reason.
    fn ge(&mut self, v1: Slot, v2: Slot, in1: Slot, in2: Slot) -> Slot {
        self.gt(v2, v1, in2, in1)
    }

    /// MaterialX's `ifequal`: `in1` where `value1 = value2`, else `in2`,
    /// built as "neither is greater".
    fn eq(&mut self, v1: Slot, v2: Slot, in1: Slot, in2: Slot) -> Slot {
        let below = self.gt(v2, v1, in2, in1);
        self.gt(v1, v2, in2, below)
    }

    /// The right-handed angle, in radians, a graph's `rotate3d` of its tangent
    /// by `degrees` about the normal turns it by, `None` when `degrees` folds
    /// to 0.
    ///
    /// `mx_rotate_vector3` is `v·cos θ + (v × axis)·sin θ + axis·(axis·v)(1 −
    /// cos θ)`, and `v × axis = −(axis × v)`: Rodrigues' formula at `−θ`. So
    /// a `rotate3d` by `amount` degrees turns the tangent by `−amount` in the
    /// right-handed sense [`Leaf::rotation`] is stated in.
    fn tangent_rotation(&mut self, degrees: Slot) -> Option<Slot> {
        if self.is(degrees, 0.0) {
            return None;
        }
        let k = self.k(-std::f32::consts::PI / 180.0);
        Some(self.mul(degrees, k))
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
        if let Bsdf::Sheen {
            mode: SheenMode::Zeltner,
            ..
        } = &bsdf
        {
            self.out
                .reported
                .insert("sheen_bsdf mode zeltner (evaluated as conty_kulla)".into());
        }
        Some(self.push(Closure::Leaf(Leaf {
            bsdf,
            weight,
            normal,
            tangent,
            rotation: None,
        })))
    }

    /// Turns the tangent of the leaf `id` by `rotation` ([`Leaf::rotation`]).
    fn rotate(&mut self, id: Option<NodeId>, rotation: Option<Slot>) {
        if let (Some(id), Some(r)) = (id, rotation)
            && let Closure::Leaf(l) = &mut self.out.nodes[id as usize]
        {
            l.rotation = Some(r);
        }
    }

    /// The surface's presence, unless it folds to 1.
    fn set_opacity(&mut self, opacity: Slot) {
        if !self.is(opacity, 1.0) {
            self.out.opacity = Some(opacity);
        }
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
