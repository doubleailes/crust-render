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
//! One structural fact survives the flattening, because a flat list would
//! lose it and the pooling needs it: *which* specular interface a dielectric
//! is. A `dielectric_bsdf` layered directly over a diffuse (or nothing) is the
//! base specular — that is how OpenPBR's own dielectric base is built. A
//! dielectric layered over a base that **already carries a specular** (another
//! dielectric, a conductor) is a second interface above the first: a varnish,
//! a clear glaze over a satin one. Those arrive as [`LobeKind::Coat`], and
//! everything above the outermost base specular is coat — OpenPBR has exactly
//! two specular lobes, so a three-deep stack pools its upper two.
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
    /// `dielectric_bsdf`, `generalized_schlick_bsdf` (and `thin_film_bsdf`,
    /// which this reduction cannot tell from the dielectric it films) sitting
    /// directly over a non-specular base — OpenPBR's base specular.
    Dielectric,
    /// A `dielectric_bsdf` / `generalized_schlick_bsdf` that is the `top` of a
    /// `layer` whose base already carries a specular interface — OpenPBR's
    /// coat lobe, the second of its two specular lobes.
    Coat,
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
    flatten_inner(c, node, weight, depth, false, out);
}

/// [`flatten`] with the one piece of tree context a leaf needs:
/// `over_specular` is true while inside the `top` of a `layer` whose base
/// carries a specular interface, and it is what turns a dielectric leaf into a
/// [`LobeKind::Coat`].
fn flatten_inner(
    c: &mut Compiler<'_>,
    node: &Node,
    weight: u32,
    depth: usize,
    over_specular: bool,
    out: &mut Vec<Lobe>,
) {
    if depth > MAX_BSDF_DEPTH {
        return;
    }
    match node.category.as_str() {
        "surfacematerial" => {
            // `surfaceshader`-typed, not BSDF-typed, and the node's only
            // connection — there is nothing to disambiguate, so this follows
            // whatever it points at.
            if let Some(n) = connected_node(c, node, "surfaceshader") {
                flatten_inner(c, &n, weight, depth + 1, over_specular, out);
            }
        }
        "surface" => {
            if let Some(n) = connected_node(c, node, "bsdf") {
                flatten_inner(c, &n, weight, depth + 1, over_specular, out);
            }
        }
        "layer" => {
            // A layer does not split energy the way a mix does: the top sits
            // *over* the base and both are present. OpenPBR's own stack is
            // layered the same way, so both branches keep the full weight.
            //
            // What the layer does decide is *which* specular lobe a dielectric
            // in its top becomes. The base is flattened first; if it produced
            // any specular interface, the top's dielectrics sit above one and
            // are the coat. The check is structural (which leaves exist, not
            // what their weights evaluate to), so the decision is the same at
            // every shading point — a per-point flip between "coat" and "base
            // specular" would draw a seam along a mask's zero contour, since
            // the two attenuate the substrate very differently. The one
            // structural lie, a literal `weight = 0` dummy leaf, is pruned in
            // `leaf` before it can count here.
            //
            // The flag is inherited by everything beneath the top — a nested
            // layer's base included — because everything above the outermost
            // base specular is coat: OpenPBR has two specular lobes, not N.
            let before = out.len();
            if let Some(n) = bsdf_input(c, node, "base") {
                flatten_inner(c, &n, weight, depth + 1, over_specular, out);
            }
            let base_has_specular = out[before..].iter().any(|l| {
                matches!(
                    l.kind,
                    LobeKind::Dielectric | LobeKind::Conductor | LobeKind::Coat
                )
            });
            if let Some(n) = bsdf_input(c, node, "top") {
                flatten_inner(
                    c,
                    &n,
                    weight,
                    depth + 1,
                    over_specular || base_has_specular,
                    out,
                );
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
                flatten_inner(c, &n, w_bg, depth + 1, over_specular, out);
            }
            if let Some(n) = bsdf_input(c, node, "fg") {
                flatten_inner(c, &n, w_fg, depth + 1, over_specular, out);
            }
        }
        "add" => {
            for name in ["in1", "in2"] {
                if let Some(n) = bsdf_input(c, node, name) {
                    flatten_inner(c, &n, weight, depth + 1, over_specular, out);
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
                flatten_inner(c, &n, w, depth + 1, over_specular, out);
            }
        }
        _ => {
            if let Some(lobe) = leaf(c, node, weight, over_specular) {
                out.push(lobe);
            }
        }
    }
}

/// Builds a leaf lobe, or `None` for a BSDF node this reduction has no pool
/// for (a layered BSDF crust cannot express) — or for a leaf that can never
/// contribute (a literal `weight = 0`).
fn leaf(c: &mut Compiler<'_>, node: &Node, weight: u32, over_specular: bool) -> Option<Lobe> {
    let kind = match node.category.as_str() {
        "oren_nayar_diffuse_bsdf" | "diffuse_bsdf" | "burley_diffuse_bsdf" => LobeKind::Diffuse,
        "dielectric_bsdf" | "generalized_schlick_bsdf" if over_specular => LobeKind::Coat,
        "dielectric_bsdf" | "generalized_schlick_bsdf" => LobeKind::Dielectric,
        // A film is a *modifier* on the interface beneath it, and MaterialX
        // authors it as `layer(thin_film_bsdf, dielectric_bsdf)` — so it
        // always sits over a specular. Promoting it would turn every filmed
        // dielectric into a mirror clearcoat; it stays pooled with its base.
        "thin_film_bsdf" => LobeKind::Dielectric,
        "conductor_bsdf" => LobeKind::Conductor,
        "sheen_bsdf" => LobeKind::Sheen,
        "subsurface_bsdf" | "translucent_bsdf" => LobeKind::Subsurface,
        other => {
            c.unsupported.insert(other.to_string());
            return None;
        }
    };

    // A leaf whose own `weight` is a literal zero is dropped here rather than
    // carried at weight 0: it contributes nothing to any pool either way, but
    // it *would* count as a specular interface in the `layer` promotion above
    // — and the `transmissiondummy` both DPEL assets use as a mix's null
    // branch is exactly such a dielectric, sitting in the base under the real
    // glaze. Only a literal is pruned; a weight *connected* to a constant or a
    // mask is a runtime value and reaches the pool like any other.
    if let Some(input) = node.input("weight")
        && let Source::Value(v) = &input.source
        && v.x() == 0.0
    {
        return None;
    }

    // A leaf's own `weight` input multiplies the weight that reached it. This
    // is what silences a `weight = 0` authored through a connection, and
    // scales every partially weighted leaf.
    let own = c.input_or(node, "weight", Val::ONE);
    let weight = c.emit(Op::Binary {
        op: super::eval::BinOp::Mul,
        a: weight,
        b: own,
    });

    let color_input = match kind {
        LobeKind::Conductor => "reflectivity",
        LobeKind::Dielectric | LobeKind::Coat => "tint",
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

    // --- Two specular lobes: which dielectric is the coat ------------------

    const DIFFUSE: &str = r#"<oren_nayar_diffuse_bsdf name="d" type="BSDF">
        <input name="color" type="color3" value="0.8, 0.2, 0.2" />
      </oren_nayar_diffuse_bsdf>"#;
    const SATIN: &str = r#"<dielectric_bsdf name="satin" type="BSDF">
        <input name="roughness" type="vector2" value="0.4, 0.4" />
      </dielectric_bsdf>"#;
    const CLEAR: &str = r#"<dielectric_bsdf name="clear" type="BSDF">
        <input name="roughness" type="vector2" value="0.02, 0.02" />
      </dielectric_bsdf>"#;
    const CONDUCTOR: &str = r#"<conductor_bsdf name="c" type="BSDF">
        <input name="roughness" type="float" value="0.1" />
      </conductor_bsdf>"#;

    fn layer(name: &str, top: &str, base: &str) -> String {
        format!(
            r#"<layer name="{name}" type="BSDF">
                 <input name="top" type="BSDF" nodename="{top}" />
                 <input name="base" type="BSDF" nodename="{base}" />
               </layer>"#
        )
    }

    fn kinds(text: &str, root: &str) -> Vec<LobeKind> {
        weights(text, root).into_iter().map(|(k, _)| k).collect()
    }

    #[test]
    fn a_dielectric_over_a_diffuse_stays_the_base_specular() {
        // OpenPBR's own dielectric base is exactly this shape; promoting it
        // would leave every plastic with a coat and no base specular.
        let text = format!(
            "<materialx>{DIFFUSE}{CLEAR}{}</materialx>",
            layer("L", "clear", "d")
        );
        assert_eq!(
            kinds(&text, "L"),
            vec![LobeKind::Diffuse, LobeKind::Dielectric]
        );
    }

    #[test]
    fn a_dielectric_over_a_dielectric_is_promoted_to_coat() {
        // The teapot ceramic: a clear glaze over a satin one over diffuse.
        let text = format!(
            "<materialx>{DIFFUSE}{SATIN}{CLEAR}{}{}</materialx>",
            layer("inner", "satin", "d"),
            layer("outer", "clear", "inner")
        );
        assert_eq!(
            kinds(&text, "outer"),
            vec![LobeKind::Diffuse, LobeKind::Dielectric, LobeKind::Coat]
        );
    }

    #[test]
    fn a_dielectric_over_a_conductor_is_a_coat() {
        // Varnish over metal — the one case the single-pool reduction lost
        // outright, since a metal base zeroes the dielectric Fresnel term.
        let text = format!(
            "<materialx>{CONDUCTOR}{CLEAR}{}</materialx>",
            layer("L", "clear", "c")
        );
        assert_eq!(kinds(&text, "L"), vec![LobeKind::Conductor, LobeKind::Coat]);
    }

    #[test]
    fn a_thin_film_over_a_dielectric_is_not_promoted() {
        let text = format!(
            r#"<materialx>{SATIN}
                 <thin_film_bsdf name="film" type="BSDF">
                   <input name="thickness" type="float" value="500" />
                 </thin_film_bsdf>
                 {}</materialx>"#,
            layer("L", "film", "satin")
        );
        assert_eq!(
            kinds(&text, "L"),
            vec![LobeKind::Dielectric, LobeKind::Dielectric]
        );
    }

    #[test]
    fn a_masked_dielectric_in_the_top_is_a_coat_at_the_mask_weight() {
        // The top is `mix(clear, dummy, mask)`: partial coverage is OpenPBR's
        // `coat_weight`, and the literal-zero dummy never reaches the list.
        let text = format!(
            r#"<materialx>{DIFFUSE}{SATIN}{CLEAR}
                 <dielectric_bsdf name="dummy" type="BSDF">
                   <input name="weight" type="float" value="0" />
                 </dielectric_bsdf>
                 <mix name="masked" type="BSDF">
                   <input name="fg" type="BSDF" nodename="clear" />
                   <input name="bg" type="BSDF" nodename="dummy" />
                   <input name="mix" type="float" value="0.3" />
                 </mix>
                 {}{}</materialx>"#,
            layer("inner", "satin", "d"),
            layer("outer", "masked", "inner")
        );
        let w = weights(&text, "outer");
        assert_eq!(w.len(), 3, "{w:?}");
        let coat: Vec<f32> = w
            .iter()
            .filter(|(k, _)| *k == LobeKind::Coat)
            .map(|(_, v)| *v)
            .collect();
        assert_eq!(coat.len(), 1, "{w:?}");
        assert!((coat[0] - 0.3).abs() < 1e-5, "coat weight {}", coat[0]);
    }

    #[test]
    fn every_dielectric_above_the_outermost_specular_is_a_coat() {
        // Three interfaces: OpenPBR has two specular lobes, so both upper
        // dielectrics pool into the coat — including the one that is the
        // *base* of the nested layer, since it still sits above the metal.
        let text = format!(
            "<materialx>{CONDUCTOR}{SATIN}{CLEAR}{}{}</materialx>",
            layer("inner", "clear", "satin"),
            layer("outer", "inner", "c")
        );
        assert_eq!(
            kinds(&text, "outer"),
            vec![LobeKind::Conductor, LobeKind::Coat, LobeKind::Coat]
        );
    }

    #[test]
    fn a_literal_zero_weight_leaf_is_pruned() {
        // The transmission dummy in the base must not make the glaze above
        // it look like a second interface.
        let text = format!(
            r#"<materialx>{DIFFUSE}{CLEAR}
                 <dielectric_bsdf name="dummy" type="BSDF">
                   <input name="weight" type="float" value="0" />
                 </dielectric_bsdf>
                 <mix name="m" type="BSDF">
                   <input name="fg" type="BSDF" nodename="d" />
                   <input name="bg" type="BSDF" nodename="dummy" />
                   <input name="mix" type="float" value="0.5" />
                 </mix>
                 {}</materialx>"#,
            layer("L", "clear", "m")
        );
        assert_eq!(
            kinds(&text, "L"),
            vec![LobeKind::Diffuse, LobeKind::Dielectric]
        );
    }

    #[test]
    fn a_connected_zero_weight_leaf_still_reaches_the_pool() {
        // Pruning is for literals only: a weight fed by a node is a runtime
        // value, even when that node happens to be a zero constant.
        let text = r#"<materialx>
          <constant name="k" type="float">
            <input name="value" type="float" value="0" />
          </constant>
          <dielectric_bsdf name="t" type="BSDF">
            <input name="weight" type="float" nodename="k" />
          </dielectric_bsdf>
        </materialx>"#;
        let w = weights(text, "t");
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(w[0].0, LobeKind::Dielectric);
        assert!(w[0].1.abs() < 1e-6, "weight {}", w[0].1);
    }
}
