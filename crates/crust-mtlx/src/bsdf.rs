//! Flattening a MaterialX BSDF tree into weighted lobes.
//!
//! MaterialX lets a look be assembled from *standalone* BSDF nodes —
//! `oren_nayar_diffuse_bsdf`, `dielectric_bsdf`, `conductor_bsdf`,
//! `sheen_bsdf` — glued with `layer` and `mix`, which is exactly how the DPEL
//! Teapot and Lion are authored. A renderer with a fixed-lobe übershader
//! cannot execute that tree directly, so this module reduces it to something
//! it can: a flat list of [`Lobe`]s, each a BSDF leaf plus the weight that
//! reaches it.
//!
//! The flattening happens **at compile time**. A `mix(fg, bg, m)` sends `m`
//! down one branch and `1 − m` down the other; a `layer(top, base)` sends full
//! weight down both. Each leaf therefore arrives with a weight that is a
//! product of mask expressions — itself compiled into the same
//! [`crate::Program`], so it costs nothing extra to evaluate. The tree
//! structure is gone before any ray is traced.
//!
//! What a renderer then *does* with the lobes — pooling them onto its own
//! material model — is deliberately not decided here. crust's OpenPBR pooling
//! lives in crust-core (`material/materialx.rs`); another renderer would write
//! its own over the same `Vec<Lobe>`.

use crate::eval::{Compiler, Op};
use crate::parse::{Node, Source};
use crate::value::Val;

/// Which OpenPBR pool a MaterialX BSDF leaf feeds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LobeKind {
    /// `oren_nayar_diffuse_bsdf`, `diffuse_bsdf`, `burley_diffuse_bsdf`.
    Diffuse,
    /// `dielectric_bsdf`, `generalized_schlick_bsdf` — a specular coat over
    /// whatever sits beneath it.
    Dielectric,
    /// `conductor_bsdf` — OpenPBR's metal lobe.
    Conductor,
    /// `sheen_bsdf` — OpenPBR's fuzz.
    Sheen,
    /// `subsurface_bsdf` / `translucent_bsdf`.
    Subsurface,
}

/// One flattened BSDF leaf: a kind, the weight that reaches it, and the
/// program slots its parameters are computed in.
pub struct Lobe {
    pub kind: LobeKind,
    /// Slot holding the scalar weight reaching this leaf — every `mix` factor
    /// along its path multiplied together, times its own `weight` input.
    pub weight: u32,
    /// `color` / `tint` / `reflectivity`.
    pub color: u32,
    /// Roughness, as authored. A `vector2` anisotropic roughness is reduced to
    /// its first lane: crust's OpenPBR carries anisotropy as a separate
    /// `specular_roughness_anisotropy` and every lobe in these two assets is
    /// isotropic (`convert float→vector2`), so taking lane 0 loses nothing
    /// here and is the right *mean* if it ever does.
    pub roughness: u32,
    /// Real IOR — a dielectric's `ior`, or a conductor's `ior` output.
    pub ior: u32,
    /// Imaginary IOR (extinction); only read for [`LobeKind::Conductor`].
    pub extinction: u32,
    /// Slot holding this lobe's shading normal, when it authored one.
    pub normal: Option<u32>,
}

/// How far a BSDF tree may nest before flattening gives up.
///
/// MaterialX has no depth limit, but a hand-edited document can describe a
/// cycle through `layer`/`mix` that the pattern compiler's own cycle guard
/// does not see (it guards *pattern* recursion, and BSDF inputs are walked
/// separately). Six is comfortably past the four-deep stacks these assets use.
const MAX_BSDF_DEPTH: usize = 16;

/// Flattens the BSDF tree rooted at `node` into `out`.
///
/// `weight` is the slot holding the accumulated weight reaching this subtree;
/// `normal` the enclosing shading normal, which a leaf inherits unless it
/// authors its own.
pub fn flatten(c: &mut Compiler<'_>, node: &Node, weight: u32, depth: usize, out: &mut Vec<Lobe>) {
    if depth > MAX_BSDF_DEPTH {
        return;
    }
    match node.category.as_str() {
        "surfacematerial" => {
            // `surfaceshader`-typed, not BSDF-typed, and the node's only
            // connection — there is nothing to disambiguate, so this follows
            // whatever it points at.
            if let Some(n) = connected_node(c, node, "surfaceshader") {
                flatten(c, &n, weight, depth + 1, out);
            }
        }
        "surface" => {
            if let Some(n) = connected_node(c, node, "bsdf") {
                flatten(c, &n, weight, depth + 1, out);
            }
        }
        "layer" => {
            // A layer does not split energy the way a mix does: the top sits
            // *over* the base and both are present. OpenPBR's own stack is
            // layered the same way, so both branches keep the full weight and
            // the pools sort out which lobe each belongs to.
            if let Some(n) = bsdf_input(c, node, "base") {
                flatten(c, &n, weight, depth + 1, out);
            }
            if let Some(n) = bsdf_input(c, node, "top") {
                flatten(c, &n, weight, depth + 1, out);
            }
        }
        "mix" => {
            let m = c.input_or(node, "mix", Val::ZERO);
            let one = c.constant(Val::ONE);
            let inv = c.emit(Op::Invert { a: m, amount: one });
            let w_fg = c.emit(Op::Binary {
                op: super::eval::BinOp::Mul,
                a: weight,
                b: m,
            });
            let w_bg = c.emit(Op::Binary {
                op: super::eval::BinOp::Mul,
                a: weight,
                b: inv,
            });
            if let Some(n) = bsdf_input(c, node, "bg") {
                flatten(c, &n, w_bg, depth + 1, out);
            }
            if let Some(n) = bsdf_input(c, node, "fg") {
                flatten(c, &n, w_fg, depth + 1, out);
            }
        }
        "add" => {
            for name in ["in1", "in2"] {
                if let Some(n) = bsdf_input(c, node, name) {
                    flatten(c, &n, weight, depth + 1, out);
                }
            }
        }
        "multiply" => {
            // `multiply(BSDF, float|color)` attenuates a lobe. Which side
            // holds the BSDF is decided by the declared type, never by
            // position: the scalar side is routinely a connected node (a
            // texture, a mask chain) rather than a literal, so taking the
            // first *connected* input would make that scalar the BSDF. The
            // real branch would then be compiled as the attenuation value and
            // every lobe under it silently lost.
            let (bsdf, scalar) = match bsdf_input(c, node, "in1") {
                Some(n) => (Some(n), "in2"),
                None => (bsdf_input(c, node, "in2"), "in1"),
            };
            if let Some(n) = bsdf {
                let s = c.input_or(node, scalar, Val::ONE);
                let w = c.emit(Op::Binary {
                    op: super::eval::BinOp::Mul,
                    a: weight,
                    b: s,
                });
                flatten(c, &n, w, depth + 1, out);
            }
        }
        _ => {
            if let Some(lobe) = leaf(c, node, weight) {
                out.push(lobe);
            }
        }
    }
}

/// Builds a leaf lobe, or `None` for a BSDF node this reduction has no pool
/// for (a thin-film or layered BSDF crust cannot express).
fn leaf(c: &mut Compiler<'_>, node: &Node, weight: u32) -> Option<Lobe> {
    let kind = match node.category.as_str() {
        "oren_nayar_diffuse_bsdf" | "diffuse_bsdf" | "burley_diffuse_bsdf" => LobeKind::Diffuse,
        "dielectric_bsdf" | "generalized_schlick_bsdf" | "thin_film_bsdf" => LobeKind::Dielectric,
        "conductor_bsdf" => LobeKind::Conductor,
        "sheen_bsdf" => LobeKind::Sheen,
        "subsurface_bsdf" | "translucent_bsdf" => LobeKind::Subsurface,
        other => {
            c.unsupported.insert(other.to_string());
            return None;
        }
    };

    // A leaf's own `weight` input multiplies the weight that reached it. This
    // is what silences the `transmissiondummy` nodes both assets use as a
    // mix's null branch: they author `weight = 0`.
    let own = c.input_or(node, "weight", Val::ONE);
    let weight = c.emit(Op::Binary {
        op: super::eval::BinOp::Mul,
        a: weight,
        b: own,
    });

    let color_input = match kind {
        LobeKind::Conductor => "reflectivity",
        LobeKind::Dielectric => "tint",
        _ => "color",
    };
    let color = match node.input(color_input) {
        Some(_) => c.input_or(node, color_input, Val::ONE),
        // A conductor authored through `artistic_ior` has no `reflectivity`
        // of its own; its colour is recovered from (ior, extinction) at
        // shading time, so white here is the neutral multiplier.
        None => c.constant(Val::ONE),
    };
    let roughness = c.input_or(
        node,
        "roughness",
        // MaterialX's specular BSDFs default to a mirror; `oren_nayar`'s
        // `roughness` is its Oren-Nayar sigma, also 0.
        Val::ZERO,
    );
    let ior = c.input_or(node, "ior", Val::float(1.5));
    let extinction = c.input_or(node, "extinction", Val::ZERO);
    let normal = c.optional_input(node, "normal");

    Some(Lobe {
        kind,
        weight,
        color,
        roughness,
        ior,
        extinction,
        normal,
    })
}

/// Follows an input to the node feeding it, whatever that node's type.
///
/// For inputs that cannot be confused with an operand of another type — a
/// `surfacematerial`'s one shader, a `surface`'s one bsdf. Where two inputs
/// compete for the same role, use [`bsdf_input`] instead.
fn connected_node(c: &Compiler<'_>, node: &Node, name: &str) -> Option<Node> {
    let input = node.input(name)?;
    let scope = node.graph.clone().unwrap_or_default();
    match &input.source {
        Source::Node { name, .. } => c.doc.find(&scope, name).cloned(),
        Source::Graph { graph, output } => {
            c.doc.graph_output(graph, output).map(|g| g.node.clone())
        }
        Source::Value(_) => None,
    }
}

/// Follows a **BSDF-typed** input to the node feeding it, and `None` for one
/// carrying anything else.
///
/// The type is what separates a BSDF branch from an operand beside it, and it
/// has to be checked rather than inferred from position: `multiply`'s two
/// inputs are both ordinary connections, and only the declared type says
/// which of them is the lobe and which the attenuation.
///
/// Either declaration counts — the input's own `type`, or the target node's.
/// A conformant document states both, but a `type` attribute omitted on an
/// input parses as `float` (its schema default), and dropping a branch over
/// that would trade this bug for a quieter one: a node declared
/// `type="BSDF"` is a BSDF whatever the edge to it says.
fn bsdf_input(c: &Compiler<'_>, node: &Node, name: &str) -> Option<Node> {
    let declared = node.input(name).is_some_and(|i| is_bsdf_type(&i.type_name));
    let target = connected_node(c, node, name)?;
    (declared || is_bsdf_type(&target.type_name)).then_some(target)
}

/// MaterialX's type name for a BSDF, as authored (`BSDF`, upper case in every
/// document the specification's own examples ship).
fn is_bsdf_type(type_name: &str) -> bool {
    type_name.eq_ignore_ascii_case("bsdf")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{Program, ShadeCtx};
    use crate::parse::Doc;
    use glam::Vec3A;

    fn build(text: &str, root: &str) -> (Program, Vec<Lobe>) {
        let doc = Doc::parse(text).unwrap();
        let loader = |_: &str, _: Option<&str>| None;
        let mut c = Compiler::new(&doc, &loader);
        let one = c.constant(Val::ONE);
        let node = doc.find("", root).unwrap().clone();
        let mut lobes = Vec::new();
        flatten(&mut c, &node, one, 0, &mut lobes);
        (c.program, lobes)
    }

    fn weights(text: &str, root: &str) -> Vec<(LobeKind, f32)> {
        let (p, lobes) = build(text, root);
        let mut slots = Vec::new();
        p.eval(
            &ShadeCtx {
                uv: (0.0, 0.0),
                normal: Vec3A::Z,
                tangent: Vec3A::X,
                view: -Vec3A::Z,
                position: Vec3A::ZERO,
            },
            &mut slots,
        );
        lobes
            .iter()
            .map(|l| (l.kind, slots[l.weight as usize].x()))
            .collect()
    }

    const MIXED: &str = r#"<materialx>
      <oren_nayar_diffuse_bsdf name="d" type="BSDF">
        <input name="color" type="color3" value="0.8, 0.2, 0.2" />
      </oren_nayar_diffuse_bsdf>
      <conductor_bsdf name="c" type="BSDF">
        <input name="roughness" type="float" value="0.1" />
      </conductor_bsdf>
      <mix name="m" type="BSDF">
        <input name="bg" type="BSDF" nodename="c" />
        <input name="fg" type="BSDF" nodename="d" />
        <input name="mix" type="float" value="0.25" />
      </mix>
    </materialx>"#;

    /// `multiply` with the *scalar* authored first, and as a connected node
    /// rather than a literal — which is how a mask or a texture reaches an
    /// attenuation in a real look-dev graph.
    const SCALAR_FIRST: &str = r#"<materialx>
      <constant name="k" type="float">
        <input name="value" type="float" value="0.25" />
      </constant>
      <oren_nayar_diffuse_bsdf name="d" type="BSDF">
        <input name="color" type="color3" value="0.8, 0.2, 0.2" />
      </oren_nayar_diffuse_bsdf>
      <multiply name="m" type="BSDF">
        <input name="in1" type="float" nodename="k" />
        <input name="in2" type="BSDF" nodename="d" />
      </multiply>
    </materialx>"#;

    #[test]
    fn a_multiply_finds_its_bsdf_whichever_input_holds_it() {
        // The regression this pins: the operand used to be whichever input
        // resolved to a node *first*, so a connected scalar in `in1` was
        // taken as the BSDF — the real branch became the attenuation value
        // and its lobe vanished from the reduction entirely.
        let w = weights(SCALAR_FIRST, "m");
        assert_eq!(w.len(), 1, "the BSDF branch was dropped: {w:?}");
        assert_eq!(w[0].0, LobeKind::Diffuse);
        assert!((w[0].1 - 0.25).abs() < 1e-5, "weight {}", w[0].1);
    }

    #[test]
    fn a_multiply_still_attenuates_a_bsdf_authored_first() {
        // The conventional order, with a literal scalar, is unchanged.
        let w = weights(
            r#"<materialx>
                 <conductor_bsdf name="c" type="BSDF">
                   <input name="roughness" type="float" value="0.1" />
                 </conductor_bsdf>
                 <multiply name="m" type="BSDF">
                   <input name="in1" type="BSDF" nodename="c" />
                   <input name="in2" type="float" value="0.5" />
                 </multiply>
               </materialx>"#,
            "m",
        );
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(w[0].0, LobeKind::Conductor);
        assert!((w[0].1 - 0.5).abs() < 1e-5, "weight {}", w[0].1);
    }

    #[test]
    fn a_surfacematerial_still_reaches_its_bsdf() {
        // `surfaceshader` and `surfacematerial` inputs are not BSDF-typed, so
        // the type check must not be applied to them — the whole material
        // would reduce to no lobes at all.
        let w = weights(
            r#"<materialx>
                 <oren_nayar_diffuse_bsdf name="d" type="BSDF">
                   <input name="color" type="color3" value="0.8, 0.8, 0.8" />
                 </oren_nayar_diffuse_bsdf>
                 <surface name="s" type="surfaceshader">
                   <input name="bsdf" type="BSDF" nodename="d" />
                 </surface>
                 <surfacematerial name="mat" type="material">
                   <input name="surfaceshader" type="surfaceshader" nodename="s" />
                 </surfacematerial>
               </materialx>"#,
            "mat",
        );
        assert_eq!(w, vec![(LobeKind::Diffuse, 1.0)]);
    }

    #[test]
    fn an_untyped_edge_to_a_bsdf_node_is_still_a_bsdf() {
        // A `type` attribute omitted on the input parses as `float`. The node
        // it points at declares `BSDF`, and that is enough — rejecting here
        // would drop a branch that used to reduce correctly.
        let w = weights(
            r#"<materialx>
                 <oren_nayar_diffuse_bsdf name="d" type="BSDF">
                   <input name="color" type="color3" value="0.5, 0.5, 0.5" />
                 </oren_nayar_diffuse_bsdf>
                 <multiply name="m" type="BSDF">
                   <input name="in1" nodename="d" />
                   <input name="in2" type="float" value="0.5" />
                 </multiply>
               </materialx>"#,
            "m",
        );
        assert_eq!(w.len(), 1, "{w:?}");
        assert!((w[0].1 - 0.5).abs() < 1e-5, "weight {}", w[0].1);
    }

    #[test]
    fn a_mix_partitions_weight_between_its_branches() {
        let w = weights(MIXED, "m");
        let diffuse: f32 = w
            .iter()
            .filter(|(k, _)| *k == LobeKind::Diffuse)
            .map(|(_, v)| v)
            .sum();
        let metal: f32 = w
            .iter()
            .filter(|(k, _)| *k == LobeKind::Conductor)
            .map(|(_, v)| v)
            .sum();
        assert!((diffuse - 0.25).abs() < 1e-5, "diffuse {diffuse}");
        assert!((metal - 0.75).abs() < 1e-5, "metal {metal}");
        // Partition of unity: nothing created, nothing lost.
        assert!((diffuse + metal - 1.0).abs() < 1e-5);
    }
}
