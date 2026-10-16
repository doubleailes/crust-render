//! What a hit on a curve knows about its strand: where along the strand it
//! lies, which curve of its prim it is, and the curve primvars its material
//! reads.
//!
//! The kernel reports a curve hit as a span — `prim_id`, one linear segment
//! or one cubic span — and a parameter `u` within it, and knows nothing of
//! strands, as it knows nothing of the polygon a triangle came from. A
//! [`StrandTable`] maps the one to the other, the way a [`crate::FaceMap`]
//! maps a triangle to its polygon. The import builds one only for curves whose
//! material reads a curve property, a curve primvar or the chart
//! ([`crate::Material::curve_needs`]), so a groom shaded by a material that
//! reads none of them pays nothing.

use crust_mtlx::Val;

/// One kernel primitive of a curves prim: the curve it belongs to, and the
/// interval of that curve's parameter it covers, normalised so the curve's
/// first authored point (the start of a NURBS curve's rendered domain) is 0
/// and its last is 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrandSpan {
    pub curve: u32,
    pub v0: f32,
    pub v1: f32,
}

/// A curve primvar decoded for a [`StrandTable`]: its values arranged so a
/// hit interpolates them as USD specifies for its interpolation.
#[derive(Clone, Debug, PartialEq)]
pub enum StrandValues {
    /// One value for the whole prim (`constant`).
    Constant(Val),
    /// One value per curve (`uniform`), by curve index.
    PerCurve(Vec<Val>),
    /// Each span's values at its two ends, linear in `u` between them:
    /// `varying`, and `vertex` on a linear curve.
    Linear(Vec<[Val; 2]>),
    /// Each span's four Bézier control values, cubic in `u`: `vertex` on a
    /// cubic curve, converted through the curve's basis as its points are.
    Cubic(Vec<[Val; 4]>),
}

impl StrandValues {
    /// The value on span `span` (of curve `curve`) at its parameter `u`.
    fn at(&self, span: usize, curve: u32, u: f32) -> Option<Val> {
        match self {
            StrandValues::Constant(v) => Some(*v),
            StrandValues::PerCurve(vs) => vs.get(curve as usize).copied(),
            StrandValues::Linear(vs) => {
                let [a, b] = vs.get(span)?;
                Some(weighted(&[*a, *b], [1.0 - u, u]))
            }
            StrandValues::Cubic(vs) => {
                let c = vs.get(span)?;
                let s = 1.0 - u;
                Some(weighted(
                    c,
                    [s * s * s, 3.0 * u * s * s, 3.0 * u * u * s, u * u * u],
                ))
            }
        }
    }

    /// Bytes held, for `--stats`.
    fn bytes(&self) -> usize {
        let n = match self {
            StrandValues::Constant(_) => 1,
            StrandValues::PerCurve(vs) => vs.len(),
            StrandValues::Linear(vs) => 2 * vs.len(),
            StrandValues::Cubic(vs) => 4 * vs.len(),
        };
        n * std::mem::size_of::<Val>()
    }
}

/// `Σ wᵢ·valsᵢ`, lane by lane, at the first value's width.
fn weighted<const N: usize>(vals: &[Val; N], w: [f32; N]) -> Val {
    let mut v = [0.0f32; 4];
    for (val, w) in vals.iter().zip(w) {
        for (lane, x) in v.iter_mut().zip(val.v) {
            *lane += w * x;
        }
    }
    Val {
        v,
        arity: vals[0].arity,
    }
}

/// The span → strand map of one curves prim (or prototype part), and the
/// primvars its material reads, decoded. See the module documentation.
#[derive(Clone, Debug)]
pub struct StrandTable {
    /// By kernel `prim_id`.
    spans: Vec<StrandSpan>,
    /// The prim path's hash, which `curve:random` folds in so that two prims
    /// with the same curves do not repeat each other's pattern.
    seed: [i32; 2],
    /// The chart `primvars:st` (or the material's own) fills, when the
    /// material reads one and the prim authors it.
    chart: Option<StrandValues>,
    /// The primvars the material's program names, in its order
    /// ([`crust_mtlx::Program::primvars`]); `None` where the prim authors no
    /// readable primvar of that name.
    primvars: Vec<Option<StrandValues>>,
}

/// The seed `curve:random` hashes under, so its values are not those of any
/// other use of [`utils::hash3`].
const RANDOM_SEED: u32 = 0x6375_7276; // "curv"

impl StrandTable {
    /// A table over `spans`, for the prim at `path`.
    pub fn new(spans: Vec<StrandSpan>, path: &str) -> StrandTable {
        let h = fnv1a(path.as_bytes());
        StrandTable {
            spans,
            seed: [h as i32, (h >> 32) as i32],
            chart: None,
            primvars: Vec::new(),
        }
    }

    pub fn with_chart(mut self, chart: Option<StrandValues>) -> StrandTable {
        self.chart = chart;
        self
    }

    pub fn with_primvars(mut self, primvars: Vec<Option<StrandValues>>) -> StrandTable {
        self.primvars = primvars;
        self
    }

    /// Number of spans, one per kernel primitive.
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// The span a hit on primitive `prim_id` lies on.
    pub fn span(&self, prim_id: u32) -> Option<&StrandSpan> {
        self.spans.get(prim_id as usize)
    }

    /// `curve:random` of curve `curve`: a hash of the prim's path and the
    /// index, uniform over [0, 1).
    pub fn random(&self, curve: u32) -> f32 {
        utils::hash3(curve as i32, self.seed[0], self.seed[1], RANDOM_SEED)
    }

    /// The chart at `u` on primitive `prim_id`, when the table carries one.
    pub fn chart_at(&self, prim_id: u32, u: f32) -> Option<(f32, f32)> {
        let span = self.span(prim_id)?;
        let v = self.chart.as_ref()?.at(prim_id as usize, span.curve, u)?;
        Some((v.v[0], v.v[1]))
    }

    /// Bytes held, for `--stats`.
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<StrandTable>()
            + self.spans.len() * std::mem::size_of::<StrandSpan>()
            + self.chart.as_ref().map_or(0, StrandValues::bytes)
            + self
                .primvars
                .iter()
                .flatten()
                .map(StrandValues::bytes)
                .sum::<usize>()
    }
}

/// FNV-1a over `bytes`: a hash with no seed, stable across runs, platforms
/// and toolchains — which `std`'s hashers do not promise.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// A hit's place on its strand ([`crate::HitRecord::curve`]): the table of
/// the curves it hit and where on it, from which everything a material reads
/// is looked up when it reads it.
///
/// Sixteen bytes, and its `Option` no more (the reference is never null):
/// every hit record carries one, curve or not, and a record grown past what
/// LLVM copies inline turns each copy into a `memcpy` call — measured at
/// +0.6% of cornellbox's instructions when this held its span by value.
#[derive(Clone, Copy)]
pub struct CurveHit<'w> {
    table: &'w StrandTable,
    /// Always a span of `table`'s (see [`CurveHit::new`]).
    prim_id: u32,
    u: f32,
}

impl<'w> CurveHit<'w> {
    /// The hit at `u` on primitive `prim_id` of `table`'s curves, or `None`
    /// for a primitive the table does not cover.
    pub fn new(table: &'w StrandTable, prim_id: u32, u: f32) -> Option<CurveHit<'w>> {
        table.span(prim_id)?;
        Some(CurveHit { table, prim_id, u })
    }

    /// The span hit. `new` checked it exists.
    fn span(&self) -> StrandSpan {
        self.table.spans[self.prim_id as usize]
    }

    /// Position along the whole strand, 0 at its first point and 1 at its
    /// last — `curve:v`.
    pub fn v(&self) -> f32 {
        let s = self.span();
        s.v0 + self.u * (s.v1 - s.v0)
    }

    /// The curve's index within its prim — `curve:index`.
    pub fn index(&self) -> u32 {
        self.span().curve
    }

    /// `curve:random`.
    pub fn random(&self) -> f32 {
        self.table.random(self.span().curve)
    }

    /// The value of each primvar the table carries, in the program's order,
    /// into `out` (cleared first).
    pub fn primvars_into(&self, out: &mut Vec<Option<Val>>) {
        out.clear();
        let curve = self.span().curve;
        out.extend(self.table.primvars.iter().map(|p| {
            p.as_ref()
                .and_then(|p| p.at(self.prim_id as usize, curve, self.u))
        }));
    }

    /// The chart at the hit, when the table carries one.
    pub fn chart(&self) -> Option<(f32, f32)> {
        self.table.chart_at(self.prim_id, self.u)
    }

    /// Whether the table carries any primvar values at all.
    pub fn has_primvars(&self) -> bool {
        !self.table.primvars.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> StrandTable {
        // Two curves: the first of two spans, the second of one.
        StrandTable::new(
            vec![
                StrandSpan {
                    curve: 0,
                    v0: 0.0,
                    v1: 0.5,
                },
                StrandSpan {
                    curve: 0,
                    v0: 0.5,
                    v1: 1.0,
                },
                StrandSpan {
                    curve: 1,
                    v0: 0.0,
                    v1: 1.0,
                },
            ],
            "/World/groom",
        )
    }

    #[test]
    fn v_runs_along_the_whole_strand() {
        let t = table();
        let at = |prim, u| CurveHit::new(&t, prim, u).unwrap().v();
        assert_eq!(at(0, 0.0), 0.0);
        assert_eq!(at(0, 1.0), 0.5);
        assert_eq!(at(1, 0.0), 0.5);
        assert_eq!(at(1, 0.5), 0.75);
        assert_eq!(at(2, 0.25), 0.25);
        assert!(CurveHit::new(&t, 3, 0.0).is_none());
    }

    #[test]
    fn values_interpolate_as_their_interpolation_says() {
        let t = table().with_primvars(vec![
            Some(StrandValues::Constant(Val::float(2.0))),
            Some(StrandValues::PerCurve(vec![
                Val::float(1.0),
                Val::float(3.0),
            ])),
            Some(StrandValues::Linear(vec![
                [Val::float(0.0), Val::float(1.0)],
                [Val::float(1.0), Val::float(3.0)],
                [Val::float(5.0), Val::float(5.0)],
            ])),
            Some(StrandValues::Cubic(vec![
                [
                    Val::float(0.0),
                    Val::float(0.0),
                    Val::float(1.0),
                    Val::float(1.0)
                ];
                3
            ])),
            None,
        ]);
        let mut out = Vec::new();
        CurveHit::new(&t, 1, 0.5).unwrap().primvars_into(&mut out);
        let x: Vec<Option<f32>> = out.iter().map(|v| v.map(|v| v.x())).collect();
        // Bézier (0, 0, 1, 1) at ½ is ½.
        assert_eq!(x, [Some(2.0), Some(1.0), Some(2.0), Some(0.5), None]);
        CurveHit::new(&t, 2, 0.5).unwrap().primvars_into(&mut out);
        assert_eq!(out[1].unwrap().x(), 3.0);
    }

    #[test]
    fn random_is_stable_and_differs_between_prims() {
        let (a, b) = (table(), StrandTable::new(Vec::new(), "/World/other"));
        assert_eq!(a.random(5), table().random(5));
        assert_ne!(a.random(5), b.random(5));
        assert_ne!(a.random(5), a.random(6));
        assert!((0.0..1.0).contains(&a.random(5)));
    }
}
