//! A mesh's primvars as a table per triangle, read by the primvar AOVs at
//! the camera's first hit and by its material at every hit ([`MeshHit`]).
//!
//! The kernel reports a triangle and its barycentrics; a [`PrimvarTable`]
//! holds what the mesh authors in the shape each interpolation needs to be
//! read at them, as a [`crate::StrandTable`] does for a curve's primvars.

/// A mesh's primvars, per triangle, in two lists: the ones render vars ask
/// for (`channels`, one per entry of the render's primvar keep set) and the
/// ones its material reads (`shading`, one per name of the material's
/// [`crate::Material::primvars`], in that order). `None` where the mesh does not
/// author one (or authors it with a type that cannot be read). Values are
/// four lanes, the unused ones zero.
///
/// `channels` is read only by the AOV first hit (`World::primvars_at`),
/// never by `World::intersect`: a geometry carrying only those keeps the
/// `plain` fast path, so the beauty pays nothing per hit. A geometry whose
/// table holds a `shading` channel leaves it, and its hits carry a
/// [`MeshHit`] for the material to read. Shared by `Arc` across the
/// placements of one distinct mesh, like [`crate::UvMap`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PrimvarTable {
    pub channels: Vec<Option<PrimvarChannel>>,
    /// Each with its authored component count, which `geompropvalue`
    /// converts from as MaterialX's `convert` does.
    pub shading: Vec<Option<(PrimvarChannel, u8)>>,
}

/// One primvar of a [`PrimvarTable`], in the shape its interpolation needs.
#[derive(Debug, Clone, PartialEq)]
pub enum PrimvarChannel {
    /// `constant`: one value for the whole mesh.
    Constant([f32; 4]),
    /// `uniform`: per triangle, the index of its polygon's value.
    PerTriangle { values: Vec<[f32; 4]>, of: Vec<u32> },
    /// `vertex`, `varying` and `faceVarying`: per triangle, the index of each
    /// corner's value, in the triangle's original vertex order.
    Corners {
        values: Vec<[f32; 4]>,
        corners: Vec<[u32; 3]>,
    },
}

impl PrimvarChannel {
    /// The value at barycentrics `(u, v)` of triangle `prim_id` (in the
    /// kernel's vertex order, which `swapped` undoes as [`UvMap::resolve`]
    /// does), or `None` for a triangle or index the channel does not cover.
    pub fn at(&self, prim_id: u32, u: f32, v: f32, swapped: bool) -> Option<[f32; 4]> {
        match self {
            PrimvarChannel::Constant(x) => Some(*x),
            PrimvarChannel::PerTriangle { values, of } => {
                values.get(*of.get(prim_id as usize)? as usize).copied()
            }
            PrimvarChannel::Corners { values, corners } => {
                let [a, b, c] = corners
                    .get(prim_id as usize)?
                    .map(|i| values.get(i as usize));
                let (a, b, c) = (a?, b?, c?);
                let (u, v) = if swapped { (v, u) } else { (u, v) };
                let w = 1.0 - u - v;
                Some(std::array::from_fn(|l| w * a[l] + u * b[l] + v * c[l]))
            }
        }
    }

    fn bytes(&self) -> usize {
        let lanes = std::mem::size_of::<[f32; 4]>();
        match self {
            PrimvarChannel::Constant(_) => lanes,
            PrimvarChannel::PerTriangle { values, of } => values.len() * lanes + of.len() * 4,
            PrimvarChannel::Corners { values, corners } => {
                values.len() * lanes + corners.len() * 12
            }
        }
    }
}

impl PrimvarTable {
    /// Whether no channel holds anything: such a table is never attached.
    pub fn is_empty(&self) -> bool {
        self.channels.iter().all(Option::is_none) && !self.shades()
    }

    /// Whether the material reads any of its channels: what takes the
    /// geometry off `World::intersect`'s plain path.
    pub fn shades(&self) -> bool {
        self.shading.iter().any(Option::is_some)
    }

    /// Heap bytes of the values and indices, for `--stats`.
    pub fn bytes(&self) -> usize {
        let shading = self.shading.iter().flatten().map(|(c, _)| c);
        self.channels
            .iter()
            .flatten()
            .chain(shading)
            .map(PrimvarChannel::bytes)
            .sum()
    }
}

/// Where a hit lies on a mesh whose material reads primvars
/// ([`PrimvarTable::shading`]): the table, the kernel's triangle and its
/// barycentrics, the winding already undone. What a hit on any geometry
/// that is neither a Ptex-mapped mesh nor a curve carries when its material
/// reads one, an analytic sphere's constant primvars included.
///
/// Twenty bytes, packed to four-byte alignment: aligned to eight it would
/// pad to 24, leave [`HitPart`](crate::HitPart)'s tag no room beside it, and
/// grow the record past 96 bytes (`a_world_hit_stays_an_inline_copy`). The
/// fields are only ever read by value, which packing allows.
#[derive(Clone, Copy)]
#[repr(C, packed(4))]
pub struct MeshHit<'w> {
    table: &'w PrimvarTable,
    prim_id: u32,
    u: f32,
    v: f32,
}

impl<'w> MeshHit<'w> {
    /// The hit at barycentrics `(u, v)` of triangle `prim_id` (in the
    /// kernel's vertex order, which `swapped` undoes).
    pub fn new(table: &'w PrimvarTable, prim_id: u32, u: f32, v: f32, swapped: bool) -> Self {
        let (u, v) = if swapped { (v, u) } else { (u, v) };
        MeshHit {
            table,
            prim_id,
            u,
            v,
        }
    }

    /// The value of each primvar the material reads, in its program's order,
    /// into `out` (cleared first): `None` for one the mesh does not carry.
    pub fn primvars_into(&self, out: &mut Vec<Option<crust_mtlx::Val>>) {
        out.clear();
        let MeshHit {
            table,
            prim_id,
            u,
            v,
        } = *self;
        out.extend(table.shading.iter().map(|c| {
            let (c, arity) = c.as_ref()?;
            let v = c.at(prim_id, u, v, false)?;
            Some(crust_mtlx::Val { v, arity: *arity })
        }));
    }
}
