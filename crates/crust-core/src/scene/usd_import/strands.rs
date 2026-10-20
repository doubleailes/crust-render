//! A curves prim's strand table ([`crate::StrandTable`]): built beside its
//! spans by [`super::shapes::curve_segments`] and
//! [`super::shapes::nurbs_curve_segments`], only when the bound material
//! reads a curve property, a curve primvar or the chart.
//!
//! Each span's primvar values are fixed linear combinations of at most four
//! authored values — the basis conversion and the NURBS blossom are both
//! linear in the control values — so the span loops hand over a
//! [`VertexRecipe`] and this module applies it to every primvar alike.

use crust_mtlx::Val;
use glam::Vec3A;
use openusd::sdf;
use openusd::usd::Prim;

use crate::color::Space;
use crate::material::CurveNeeds;
use crate::strands::{StrandSpan, StrandTable, StrandValues};
use crate::warning;

use super::attrs::{custom_i32_array, decode_text, in_working, prim_value};
use super::products::{KeptPrimvar, PrimvarKeep};

/// What a curves prim's material asks of it, and what reading it takes.
#[derive(Clone, Copy)]
pub(super) struct StrandRequest<'a> {
    pub(super) needs: CurveNeeds<'a>,
    /// The chart the material names other than `st` ([`crate::Material::uv_primvar`]).
    pub(super) chart: Option<&'a str>,
    pub(super) working: Space,
    /// The primvars render vars keep, read whatever the material reads.
    pub(super) kept: &'a PrimvarKeep,
}

impl<'a> StrandRequest<'a> {
    /// The request for `material` and the render's kept primvars, or `None`
    /// when neither reads anything a strand table carries — the case that
    /// must cost nothing.
    pub(super) fn of(
        material: &'a dyn crate::Material,
        working: Space,
        kept: &'a PrimvarKeep,
    ) -> Option<Self> {
        let needs = material.curve_needs();
        (needs.any() || !kept.is_empty()).then(|| StrandRequest {
            needs,
            chart: material.uv_primvar(),
            working,
            kept,
        })
    }
}

/// How one span's Bézier values come from the authored per-vertex values:
/// Bézier value `j` (of `n`, 2 for a linear span, 4 for a cubic one) is
/// `Σᵢ w[j][i] · value[idx[i]]` over the span's `m` authored values.
#[derive(Clone, Copy, Debug)]
pub(super) struct VertexRecipe {
    pub(super) idx: [usize; 4],
    pub(super) m: usize,
    pub(super) w: [[f32; 4]; 4],
    pub(super) n: usize,
}

impl VertexRecipe {
    /// A linear span between authored values `a` and `b`.
    pub(super) fn linear(a: usize, b: usize) -> Self {
        VertexRecipe {
            idx: [a, b, 0, 0],
            m: 2,
            w: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0; 4],
                [0.0; 4],
            ],
            n: 2,
        }
    }

    /// The same span as a cubic: the straight cubic tracing the line, which
    /// is how a linear NURBS span is carried when its prim also has cubic
    /// ones (see `nurbs_curve_segments`).
    pub(super) fn elevated(self) -> Self {
        if self.n == 4 {
            return self;
        }
        let (a, b) = (self.w[0], self.w[1]);
        let mix = |s: f32, t: f32| std::array::from_fn(|i| s * a[i] + t * b[i]);
        VertexRecipe {
            w: [a, mix(2.0 / 3.0, 1.0 / 3.0), mix(1.0 / 3.0, 2.0 / 3.0), b],
            n: 4,
            ..self
        }
    }
}

/// A primvar's interpolation, as far as curves read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interp {
    Constant,
    Uniform,
    Varying,
    Vertex,
}

/// One primvar being decoded into a table channel.
struct Channel {
    name: String,
    interp: Interp,
    values: Vec<Val>,
    out: StrandValues,
    /// A span asked for a value past the authored array: refused at the end.
    overrun: bool,
}

impl Channel {
    /// Adds the span's values: nothing for a `constant` or `uniform`
    /// primvar, which the table reads by curve.
    fn span(&mut self, vertex: &VertexRecipe, varying: [usize; 2]) {
        let at = |i: usize| self.values.get(i).copied();
        match self.interp {
            Interp::Constant | Interp::Uniform => {}
            Interp::Varying => match (at(varying[0]), at(varying[1])) {
                (Some(a), Some(b)) => push_linear(&mut self.out, [a, b]),
                _ => self.overrun = true,
            },
            Interp::Vertex => {
                let mut src = [Val::ZERO; 4];
                for (k, s) in src.iter_mut().enumerate().take(vertex.m) {
                    match at(vertex.idx[k]) {
                        Some(v) => *s = v,
                        None => {
                            self.overrun = true;
                            return;
                        }
                    }
                }
                let bez = |j: usize| weighted(&src[..vertex.m], &vertex.w[j][..vertex.m]);
                if vertex.n == 4 {
                    push_cubic(&mut self.out, [bez(0), bez(1), bez(2), bez(3)]);
                } else {
                    push_linear(&mut self.out, [bez(0), bez(1)]);
                }
            }
        }
    }
}

/// Appends a linear span, as a cubic when the channel already holds cubic
/// ones.
fn push_linear(out: &mut StrandValues, v: [Val; 2]) {
    match out {
        StrandValues::Linear(vs) => vs.push(v),
        StrandValues::Cubic(vs) => vs.push(elevate(v)),
        _ => unreachable!("varying and vertex channels are spans"),
    }
}

/// Appends a cubic span, turning the linear spans before it into cubic ones.
fn push_cubic(out: &mut StrandValues, v: [Val; 4]) {
    if let StrandValues::Linear(vs) = out {
        *out = StrandValues::Cubic(vs.drain(..).map(elevate).collect());
    }
    match out {
        StrandValues::Cubic(vs) => vs.push(v),
        _ => unreachable!("varying and vertex channels are spans"),
    }
}

/// The straight cubic through `[a, b]`.
fn elevate([a, b]: [Val; 2]) -> [Val; 4] {
    [
        a,
        weighted(&[a, b], &[2.0 / 3.0, 1.0 / 3.0]),
        weighted(&[a, b], &[1.0 / 3.0, 2.0 / 3.0]),
        b,
    ]
}

/// `Σ wᵢ·valsᵢ`, lane by lane, at the first value's width.
fn weighted(vals: &[Val], w: &[f32]) -> Val {
    let mut v = [0.0f32; 4];
    for (val, w) in vals.iter().zip(w) {
        for (lane, x) in v.iter_mut().zip(val.v) {
            *lane += w * x;
        }
    }
    Val {
        v,
        arity: vals.first().map_or(1, |v| v.arity),
    }
}

/// Builds a [`StrandTable`] as the span loops emit spans, in kernel
/// `prim_id` order.
pub(super) struct StrandBuilder {
    path: String,
    /// Whether the material reads the strand itself (a curve property or a
    /// primvar), or only the chart.
    strand: bool,
    spans: Vec<StrandSpan>,
    chart: Option<Channel>,
    primvars: Vec<Option<Channel>>,
    /// The kept primvars, by keep-set index, for the AOVs.
    kept: Vec<Option<Channel>>,
}

/// A curves prim's shape, which every primvar's length is checked against.
#[derive(Clone, Copy)]
pub(super) struct CurveCounts {
    pub(super) points: usize,
    pub(super) curves: usize,
    /// The `varying` values the prim needs: one per span end, curve by
    /// curve.
    pub(super) varying: usize,
}

impl StrandBuilder {
    /// Reads every primvar `request` asks for on `prim`, refusing (with a
    /// warning) any it cannot read, and the authored `primvars:curve:*`.
    pub(super) fn new(prim: &Prim, request: &StrandRequest, counts: CurveCounts) -> Self {
        let reserved: Vec<String> = prim
            .attributes_in_namespace("primvars:curve")
            .unwrap_or_default()
            .iter()
            .map(|a| a.path().to_string())
            .collect();
        if !reserved.is_empty() {
            warning!(
                CurvesReservedPrimvar,
                at = prim.path(),
                "{}: {} {} under primvars:curve:, the namespace of the curve properties — the \
                 material reads curve:v, curve:index and curve:random, not these",
                prim.path(),
                reserved.join(", "),
                if reserved.len() == 1 { "is" } else { "are" },
            );
        }
        let chart = request
            .needs
            .chart
            .then(|| {
                request
                    .chart
                    .into_iter()
                    .chain(["st", "uv", "st0", "UVMap"])
                    .find_map(|name| read(prim, name, request.working, counts, true))
            })
            .flatten();
        let primvars = request
            .needs
            .primvars
            .iter()
            .map(|name| read(prim, name, request.working, counts, false))
            .collect();
        let kept = request
            .kept
            .entries
            .iter()
            .map(|k| read_kept(prim, k, counts))
            .collect();
        StrandBuilder {
            path: prim.path().to_string(),
            strand: request.needs.strand,
            spans: Vec::new(),
            chart,
            primvars,
            kept,
        }
    }

    /// Records the next span: on curve `curve`, over `[v0, v1]` of it, its
    /// `vertex` values made by `vertex` and its `varying` ones the two at
    /// `varying`.
    pub(super) fn span(
        &mut self,
        curve: usize,
        v0: f32,
        v1: f32,
        vertex: &VertexRecipe,
        varying: [usize; 2],
    ) {
        self.spans.push(StrandSpan {
            curve: curve as u32,
            v0,
            v1,
        });
        for c in self
            .chart
            .iter_mut()
            .chain(self.primvars.iter_mut().flatten())
            .chain(self.kept.iter_mut().flatten())
        {
            c.span(vertex, varying);
        }
    }

    /// The table, with any primvar whose values ran out refused — or none
    /// when the material reads only the chart and the prim authors none: a
    /// table would then carry nothing a hit could read, and still cost its
    /// spans and take every hit off `World::intersect`'s plain path.
    pub(super) fn finish(self, prim: &Prim) -> Option<StrandTable> {
        let done = |c: Option<Channel>| -> Option<StrandValues> {
            let c = c?;
            if c.overrun {
                warning!(
                    CurvesUnsupportedPrimvar,
                    at = prim.path(),
                    "{}: primvars:{} has too few values for its {:?} interpolation — the \
                     material reads its default there",
                    prim.path(),
                    c.name,
                    c.interp,
                );
                return None;
            }
            Some(c.out)
        };
        let chart = done(self.chart);
        let primvars: Vec<_> = self.primvars.into_iter().map(done).collect();
        let kept: Vec<_> = self
            .kept
            .into_iter()
            .map(|c| {
                let c = c?;
                if c.overrun {
                    warning!(
                        AovPrimvarMismatch,
                        at = prim.path(),
                        "{}: primvars:{} has too few values for its {:?} interpolation; \
                         that prim reads the primvar vars' clear value",
                        prim.path(),
                        c.name,
                        c.interp,
                    );
                    return None;
                }
                Some(c.out)
            })
            .collect();
        // A table only the AOVs read is built only when the prim authors a
        // kept primvar: without one it would carry nothing a hit could read.
        let any_kept = kept.iter().any(Option::is_some);
        (self.strand || chart.is_some() || any_kept).then(|| {
            StrandTable::new(self.spans, &self.path)
                .with_chart(chart)
                .with_primvars(primvars)
                .with_kept(kept)
        })
    }
}

/// A kept primvar on `prim`, decoded as authored and checked against the
/// prim's counts and the var's component count; `None`, with one warning per
/// var and prim, for one the var cannot read, and silently for one that is
/// not authored.
fn read_kept(prim: &Prim, kept: &KeptPrimvar, counts: CurveCounts) -> Option<Channel> {
    let channel = match decode_channel(prim, &kept.name, None, counts) {
        Ok(c) => c?,
        Err(why) => return refuse_kept(prim, kept, &why),
    };
    match channel.values.first().map(|v| v.arity) {
        Some(n) if n == kept.components => Some(channel),
        Some(n) => refuse_kept(prim, kept, &format!("has {n} component(s)")),
        None => None,
    }
}

fn refuse_kept(prim: &Prim, kept: &KeptPrimvar, why: &str) -> Option<Channel> {
    warning!(
        AovPrimvarMismatch,
        at = prim.path(),
        "{} primvar {:?} as {} component(s), but on {} it {why}; that prim reads the \
         clear value",
        kept.asking(),
        kept.name,
        kept.components,
        prim.path(),
    );
    None
}

/// `primvars:<name>` on `prim`, decoded and checked against the prim's
/// counts; `None`, with a warning, for one that cannot be read, and silently
/// for one that is not authored. A `chart` must have two lanes, and is
/// refused silently otherwise: the next name in the chart list is tried.
fn read(
    prim: &Prim,
    name: &str,
    working: Space,
    counts: CurveCounts,
    chart: bool,
) -> Option<Channel> {
    match decode_channel(prim, name, Some(working), counts) {
        Ok(Some(c)) if chart && c.values.first().is_some_and(|v| v.arity != 2) => None,
        Ok(c) => c,
        Err(why) => {
            if !chart {
                warning!(
                    CurvesUnsupportedPrimvar,
                    at = prim.path(),
                    "{}: primvars:{name} {why} — the material reads its default there",
                    prim.path(),
                );
            }
            None
        }
    }
}

/// `primvars:<name>` on `prim`, decoded — colours into `working` when one is
/// given, as authored otherwise — and checked against the prim's counts.
/// `Ok(None)` for one that is not authored; `Err` with the reason for one
/// that cannot be read.
fn decode_channel(
    prim: &Prim,
    name: &str,
    working: Option<Space>,
    counts: CurveCounts,
) -> Result<Option<Channel>, String> {
    let attr_name = format!("primvars:{name}");
    let attr = prim.attribute(&attr_name);
    let Some(value) = prim_value(prim, &attr_name) else {
        return Ok(None);
    };
    let type_name = attr
        .type_name()
        .ok()
        .flatten()
        .map(|t| t.as_str().to_string())
        .unwrap_or_default();
    let color = type_name.starts_with("color3");
    let Some(mut values) = decode(value) else {
        return Err(format!(
            "is a {type_name}, which curves do not read (float, float2, float3, color3f, int)"
        ));
    };
    if let Some(working) = working
        && color
    {
        for v in &mut values {
            let rgb = in_working(&attr, Vec3A::new(v.v[0], v.v[1], v.v[2]), working);
            *v = rgb.into();
        }
    }
    if let Some(indices) = custom_i32_array(prim, &format!("{attr_name}:indices")) {
        let expanded: Option<Vec<Val>> = indices
            .iter()
            .map(|&i| usize::try_from(i).ok().and_then(|i| values.get(i).copied()))
            .collect();
        match expanded {
            Some(v) => values = v,
            None => return Err("has indices out of its range".into()),
        }
    }
    let interp = match attr
        .metadata::<sdf::Value>("interpolation")
        .ok()
        .flatten()
        .and_then(decode_text)
        .as_deref()
    {
        None | Some("constant") => Interp::Constant,
        Some("uniform") => Interp::Uniform,
        Some("varying") => Interp::Varying,
        Some("vertex") => Interp::Vertex,
        Some(other) => return Err(format!("is {other}, which curves do not read")),
    };
    let needed = match interp {
        Interp::Constant => 1,
        Interp::Uniform => counts.curves,
        Interp::Varying => counts.varying,
        Interp::Vertex => counts.points,
    };
    if values.len() < needed || (interp != Interp::Constant && values.len() != needed) {
        return Err(format!(
            "has {} values where its {interp:?} interpolation needs {needed}",
            values.len()
        ));
    }
    let out = match interp {
        Interp::Constant => StrandValues::Constant(values[0]),
        Interp::Uniform => StrandValues::PerCurve(values.clone()),
        Interp::Varying | Interp::Vertex => StrandValues::Linear(Vec::new()),
    };
    Ok(Some(Channel {
        name: name.to_string(),
        interp,
        values,
        out,
        overrun: false,
    }))
}

/// A primvar's values, one [`Val`] each: `float`, `int`, `float2`, `float3`
/// (and every other three-component role, colours among them), in any
/// precision, as an array or a lone `constant` value.
pub(super) fn decode(value: sdf::Value) -> Option<Vec<Val>> {
    use sdf::Value as V;
    let v2 = |x: f32, y: f32| Val::vec2(x, y);
    let v3 = |x: f32, y: f32, z: f32| Val::vec3(x, y, z);
    Some(match value {
        V::Float(x) => vec![Val::float(x)],
        V::Double(x) => vec![Val::float(x as f32)],
        V::Half(x) => vec![Val::float(x.to_f32())],
        V::Int(x) => vec![Val::float(x as f32)],
        V::FloatVec(v) => v.into_iter().map(Val::float).collect(),
        V::DoubleVec(v) => v.into_iter().map(|x| Val::float(x as f32)).collect(),
        V::HalfVec(v) => v.into_iter().map(|x| Val::float(x.to_f32())).collect(),
        V::IntVec(v) => v.into_iter().map(|x| Val::float(x as f32)).collect(),
        V::Vec2f(p) => vec![v2(p.x, p.y)],
        V::Vec2d(p) => vec![v2(p.x as f32, p.y as f32)],
        V::Vec2h(p) => vec![v2(p.x.to_f32(), p.y.to_f32())],
        V::Vec2fVec(v) => v.into_iter().map(|p| v2(p.x, p.y)).collect(),
        V::Vec2dVec(v) => v.into_iter().map(|p| v2(p.x as f32, p.y as f32)).collect(),
        V::Vec2hVec(v) => v
            .into_iter()
            .map(|p| v2(p.x.to_f32(), p.y.to_f32()))
            .collect(),
        V::Vec3f(p) => vec![v3(p.x, p.y, p.z)],
        V::Vec3d(p) => vec![v3(p.x as f32, p.y as f32, p.z as f32)],
        V::Vec3h(p) => vec![v3(p.x.to_f32(), p.y.to_f32(), p.z.to_f32())],
        V::Vec3fVec(v) => v.into_iter().map(|p| v3(p.x, p.y, p.z)).collect(),
        V::Vec3dVec(v) => v
            .into_iter()
            .map(|p| v3(p.x as f32, p.y as f32, p.z as f32))
            .collect(),
        V::Vec3hVec(v) => v
            .into_iter()
            .map(|p| v3(p.x.to_f32(), p.y.to_f32(), p.z.to_f32()))
            .collect(),
        _ => return None,
    })
}
