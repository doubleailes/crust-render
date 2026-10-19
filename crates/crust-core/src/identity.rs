//! What the identity AOVs and the ID mattes name, and what the primvar AOVs
//! read: tables the import keeps beside the world only when a render asks
//! for them (`openspec/specs/aovs/design.md`, "Identity and primvars").
//!
//! - [`Identity`]: from `geom_id` to the names a hit stands for — the prim
//!   the geometry came from, its bound material, its asset — and to the
//!   instance of a `PointInstancer` it is. The prim is the one light linking
//!   judges a receiver by: the instance or instancer prim for instanced
//!   geometry, never a prototype's `/__Prototype_N`, whose numbering is per
//!   stage.
//! - [`AovGeometry`]: per geometry, where a triangle lies in the authored
//!   mesh ([`ElementMap`]) and the primvars the render's vars read
//!   ([`PrimvarSet`]).
//!
//! Nothing here is read by the beauty: the integrator's AOV instantiation
//! looks a camera ray's first hit up in them, and nothing else does.

use std::sync::Arc;

use crate::rt_world::FaceMap;

/// What an identity names, and so what the ids of an ID matte stand for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NameKey {
    /// The prim the hit geometry came from ([`AovSource::PrimId`](crate::aov::AovSource::PrimId)).
    Prim,
    /// Its bound material ([`AovSource::MaterialId`](crate::aov::AovSource::MaterialId)).
    Material,
    /// Its asset ([`AovSource::AssetId`](crate::aov::AovSource::AssetId)).
    Asset,
}

impl NameKey {
    /// Every key, in the order a hit carries them ([`HitIdentity`]).
    pub const ALL: [NameKey; 3] = [NameKey::Prim, NameKey::Material, NameKey::Asset];

    /// The key's place in [`NameKey::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }
}

/// MurmurHash3_x86_32 of `bytes` with seed 0 — the hash `primId`,
/// `materialId` and `assetId` are, over a path's UTF-8. Any stable 32-bit
/// hash would do; this one is small and the one compositors already meet in
/// ID passes.
pub fn murmur3_32(bytes: &[u8]) -> u32 {
    const C1: u32 = 0xcc9e_2d51;
    const C2: u32 = 0x1b87_3593;
    let mut h: u32 = 0;
    let (chunks, tail) = bytes.as_chunks::<4>();
    for &chunk in chunks {
        let mut k = u32::from_le_bytes(chunk);
        k = k.wrapping_mul(C1).rotate_left(15).wrapping_mul(C2);
        h ^= k;
        h = h.rotate_left(13).wrapping_mul(5).wrapping_add(0xe654_6b64);
    }
    if !tail.is_empty() {
        let mut k: u32 = 0;
        for (i, &b) in tail.iter().enumerate() {
            k |= u32::from(b) << (8 * i);
        }
        k = k.wrapping_mul(C1).rotate_left(15).wrapping_mul(C2);
        h ^= k;
    }
    h ^= bytes.len() as u32;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    h
}

/// A name a hit carries no id for: no table, or a geometry no run covers.
pub const NO_NAME: u32 = u32::MAX;

/// "No integer": the `-1` an identity AOV holds where there is none, as
/// the bits it is written with.
pub const NO_ID: u32 = u32::MAX;

/// The names one [`NameKey`] hands out, each with its
/// hash, and the `geom_id` runs that carry each.
#[derive(Debug, Clone, Default)]
pub struct NameTable {
    names: Vec<String>,
    hashes: Vec<u32>,
    /// `(first geom_id, one past the last, name)`, sorted by first id and
    /// disjoint.
    runs: Vec<(u32, u32, u32)>,
}

impl NameTable {
    /// The name at `index`.
    pub fn name(&self, index: u32) -> Option<&str> {
        self.names.get(index as usize).map(String::as_str)
    }

    /// How many names the table holds.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether the table holds no name.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// The name `geom_id` carries, or [`NO_NAME`].
    fn of(&self, geom_id: u32) -> u32 {
        run_value(&self.runs, geom_id).unwrap_or(NO_NAME)
    }
}

/// The value of the run covering `id` in sorted, disjoint `runs`.
fn run_value(runs: &[(u32, u32, u32)], id: u32) -> Option<u32> {
    let i = runs.partition_point(|r| r.0 <= id).checked_sub(1)?;
    let (_, end, value) = runs[i];
    (id < end).then_some(value)
}

/// Builds a [`NameTable`]: names interned in first-use order, runs added
/// in `geom_id` order (merged where a run continues the previous one).
#[derive(Debug, Default)]
pub struct NameTableBuilder {
    table: NameTable,
    index: std::collections::HashMap<String, u32>,
}

impl NameTableBuilder {
    /// Records that `geom_ids` carry `name`. Runs must arrive in increasing
    /// id order and must not overlap; an empty range records nothing.
    pub fn add(&mut self, geom_ids: std::ops::Range<u32>, name: &str) {
        if geom_ids.is_empty() {
            return;
        }
        let index = match self.index.get(name) {
            Some(&i) => i,
            None => {
                let i = self.table.names.len() as u32;
                self.table.names.push(name.to_owned());
                self.table.hashes.push(murmur3_32(name.as_bytes()));
                self.index.insert(name.to_owned(), i);
                i
            }
        };
        match self.table.runs.last_mut() {
            Some(last) if last.1 == geom_ids.start && last.2 == index => last.1 = geom_ids.end,
            Some(last) => {
                debug_assert!(last.1 <= geom_ids.start, "runs in increasing id order");
                self.table.runs.push((geom_ids.start, geom_ids.end, index));
            }
            None => self.table.runs.push((geom_ids.start, geom_ids.end, index)),
        }
    }

    pub fn build(self) -> NameTable {
        self.table
    }
}

/// From `geom_id` to everything an identity AOV names: see the module
/// documentation. Built by the import ([`IdentityBuilder`]); a table the
/// render's vars do not read is left empty.
#[derive(Debug, Clone, Default)]
pub struct Identity {
    /// Per [`NameKey`], in [`NameKey::ALL`]'s order.
    names: [NameTable; 3],
    /// `(first geom_id, one past the last, index)` per top-level
    /// `PointInstancer` placement, sorted and disjoint.
    instances: Vec<(u32, u32, u32)>,
}

/// What a hit is, as the identity AOVs and mattes read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitIdentity {
    /// Per [`NameKey`]: the name's index in its table,
    /// or [`NO_NAME`].
    pub names: [u32; 3],
    /// The hash of each name (the flat ids' value); meaningless where the
    /// name is [`NO_NAME`].
    pub hashes: [u32; 3],
    /// The instance index, or [`NO_ID`].
    pub instance: u32,
    /// The authored face, or [`NO_ID`].
    pub element: u32,
}

impl HitIdentity {
    /// A hit that names nothing: an escape, a volume scatter, or a world
    /// without tables.
    pub const NONE: HitIdentity = HitIdentity {
        names: [NO_NAME; 3],
        hashes: [0; 3],
        instance: NO_ID,
        element: NO_ID,
    };
}

impl Default for HitIdentity {
    fn default() -> Self {
        HitIdentity::NONE
    }
}

impl Identity {
    /// The names of `key`.
    pub fn names(&self, key: NameKey) -> &NameTable {
        &self.names[key.index()]
    }

    /// What `geom_id` names; the element is left for the geometry's
    /// [`ElementMap`].
    pub fn lookup(&self, geom_id: u32) -> HitIdentity {
        let mut out = HitIdentity::NONE;
        for (k, table) in self.names.iter().enumerate() {
            let i = table.of(geom_id);
            out.names[k] = i;
            if i != NO_NAME {
                out.hashes[k] = table.hashes[i as usize];
            }
        }
        out.instance = run_value(&self.instances, geom_id).unwrap_or(NO_ID);
        out
    }
}

/// Builds an [`Identity`].
#[derive(Debug, Default)]
pub struct IdentityBuilder {
    names: [NameTableBuilder; 3],
    instances: Vec<(u32, u32, u32)>,
}

impl IdentityBuilder {
    /// Records that `geom_ids` carry `name` under `key` (in increasing id
    /// order per key).
    pub fn name(&mut self, key: NameKey, geom_ids: std::ops::Range<u32>, name: &str) {
        self.names[key.index()].add(geom_ids, name);
    }

    /// Records that `geom_ids` are placement `index` of a `PointInstancer`.
    pub fn instance(&mut self, geom_ids: std::ops::Range<u32>, index: u32) {
        if !geom_ids.is_empty() {
            self.instances.push((geom_ids.start, geom_ids.end, index));
        }
    }

    pub fn build(mut self) -> Identity {
        self.instances.sort_unstable();
        let [a, b, c] = self.names;
        Identity {
            names: [a.build(), b.build(), c.build()],
            instances: self.instances,
        }
    }
}

/// Where each triangle of one distinct mesh lies in the mesh as authored:
/// what `elementId` reports, and what a primvar is interpolated over.
pub enum ElementMap {
    /// Unrefined: triangle `t` of face `faces[t]` is its fan slice `(0,
    /// fan[t], fan[t] + 1)` of the face's corners — exact for every polygon.
    Cage { faces: Vec<u32>, fan: Vec<u32> },
    /// A subdivision refinement: the cage face each triangle lies in
    /// (`u32::MAX` where the refiner lost it), and where in it, as the Ptex
    /// face table resolves it — quads only, so a refined triangle or n-gon
    /// is read at its face's centre.
    Refined { faces: Vec<u32>, map: FaceMap },
}

impl std::fmt::Debug for ElementMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ElementMap::Cage { faces, .. } => {
                write!(f, "ElementMap::Cage({} triangles)", faces.len())
            }
            ElementMap::Refined { faces, .. } => {
                write!(f, "ElementMap::Refined({} triangles)", faces.len())
            }
        }
    }
}

/// Where a hit lies in an authored face: up to four of its corners
/// (indices within the face) and their weights, or its centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Spot {
    Corners {
        corners: [u32; 4],
        weights: [f32; 4],
        n: u8,
    },
    Centre,
}

impl ElementMap {
    /// The authored face triangle `prim_id` lies in.
    pub fn face(&self, prim_id: u32) -> Option<u32> {
        let faces = match self {
            ElementMap::Cage { faces, .. } | ElementMap::Refined { faces, .. } => faces,
        };
        faces
            .get(prim_id as usize)
            .copied()
            .filter(|&f| f != u32::MAX)
    }

    /// The authored face a hit on `prim_id` at barycentrics `(u, v)` lies
    /// in, and where. `swapped` undoes a mirrored bake, as for every
    /// per-triangle table.
    pub fn locate(&self, prim_id: u32, u: f32, v: f32, swapped: bool) -> Option<(u32, Spot)> {
        let face = self.face(prim_id)?;
        match self {
            ElementMap::Cage { fan, .. } => {
                let k = *fan.get(prim_id as usize)?;
                let (u, v) = if swapped { (v, u) } else { (u, v) };
                Some((
                    face,
                    Spot::Corners {
                        corners: [0, k, k + 1, 0],
                        weights: [1.0 - u - v, u, v, 0.0],
                        n: 3,
                    },
                ))
            }
            // The Ptex table places a refined triangle only inside a cage
            // quad, in Ptex's quad convention: v0 = (0,0), v1 = (1,0),
            // v2 = (1,1), v3 = (0,1) — bilinear.
            ElementMap::Refined { map, .. } => Some(match map.resolve(prim_id, u, v, swapped) {
                Some((_, s, t)) => (
                    face,
                    Spot::Corners {
                        corners: [0, 1, 2, 3],
                        weights: [(1.0 - s) * (1.0 - t), s * (1.0 - t), s * t, (1.0 - s) * t],
                        n: 4,
                    },
                ),
                None => (face, Spot::Centre),
            }),
        }
    }

    /// Bytes held, for the import's statistics.
    pub fn bytes(&self) -> usize {
        match self {
            ElementMap::Cage { faces, fan } => 4 * (faces.len() + fan.len()),
            ElementMap::Refined { faces, map } => {
                faces.len() * 4
                    + map.faces.len() * 4
                    + map.slices.len()
                    + map.sub.as_ref().map_or(0, |s| s.len() * 4)
                    + map.corners.as_ref().map_or(0, |c| c.len() * 24)
            }
        }
    }
}

/// How a primvar's values are spread over its prim (UsdGeom's
/// interpolation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpolation {
    Constant,
    Uniform,
    Vertex,
    Varying,
    FaceVarying,
}

/// One primvar of one prim, as authored: values (up to four components
/// each, padded with zeros), their optional indices, and how they spread.
#[derive(Debug, Clone, PartialEq)]
pub struct PrimvarData {
    pub interpolation: Interpolation,
    pub values: Vec<[f32; 4]>,
    pub indices: Option<Vec<u32>>,
}

impl PrimvarData {
    /// The value element `i` of the interpolation addresses, through the
    /// indices when there are some.
    fn value(&self, i: usize) -> Option<[f32; 4]> {
        let i = match &self.indices {
            Some(ix) => *ix.get(i)? as usize,
            None => i,
        };
        self.values.get(i).copied()
    }
}

/// A mesh's authored faces: where each starts among the face-vertices, and
/// the point each face-vertex is — what a `vertex` or `faceVarying` primvar
/// is read through.
#[derive(Debug, Clone, PartialEq)]
pub struct CageTopology {
    /// Per face its first face-vertex, plus one past the last.
    pub starts: Vec<u32>,
    /// Per face-vertex, its point.
    pub points: Vec<u32>,
}

impl CageTopology {
    /// From `faceVertexCounts` and `faceVertexIndices`; `None` when they do
    /// not agree (a negative count or index, or too few indices).
    pub fn new(counts: &[i32], indices: &[i32]) -> Option<CageTopology> {
        let mut starts = Vec::with_capacity(counts.len() + 1);
        let mut at = 0u32;
        starts.push(0);
        for &c in counts {
            at = at.checked_add(u32::try_from(c).ok()?)?;
            starts.push(at);
        }
        if (at as usize) > indices.len() {
            return None;
        }
        let points = indices[..at as usize]
            .iter()
            .map(|&i| u32::try_from(i).ok())
            .collect::<Option<Vec<u32>>>()?;
        Some(CageTopology { starts, points })
    }

    /// The face-vertices of face `f`.
    fn face(&self, f: u32) -> Option<std::ops::Range<usize>> {
        let f = f as usize;
        Some(*self.starts.get(f)? as usize..*self.starts.get(f + 1)? as usize)
    }
}

/// The primvars of one prim the render's vars read, in the render's
/// primvar order (an entry is `None` where the prim does not author it).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PrimvarSet {
    pub vars: Vec<Option<PrimvarData>>,
    /// The mesh's faces, when one of `vars` is read per face or per corner.
    pub topology: Option<Arc<CageTopology>>,
    /// The indices of `vars` the prim's own ancestors left open: nothing up
    /// to a native prototype's root says anything, so the prim that places
    /// the prototype decides. Empty off a prototype.
    pub open: Vec<usize>,
}

impl PrimvarSet {
    /// Whether any primvar needs to know where in its face a hit lies.
    pub fn needs_spot(&self) -> bool {
        self.vars.iter().flatten().any(|p| {
            matches!(
                p.interpolation,
                Interpolation::Vertex | Interpolation::Varying | Interpolation::FaceVarying
            )
        })
    }

    /// Primvar `i` at a hit in `face` at `spot` (`None` off a mesh, which
    /// reads constant primvars only). `None` where the prim does not author
    /// it or its values do not reach the hit.
    pub fn eval(&self, i: usize, at: Option<(u32, Spot)>) -> Option<[f32; 4]> {
        let p = self.vars.get(i)?.as_ref()?;
        match p.interpolation {
            Interpolation::Constant => p.value(0),
            Interpolation::Uniform => p.value(at?.0 as usize),
            Interpolation::Vertex | Interpolation::Varying | Interpolation::FaceVarying => {
                let (face, spot) = at?;
                let topo = self.topology.as_deref()?;
                let range = topo.face(face)?;
                let element = |corner: usize| -> Option<usize> {
                    let fv = range.start + corner;
                    if fv >= range.end {
                        return None;
                    }
                    Some(if p.interpolation == Interpolation::FaceVarying {
                        fv
                    } else {
                        *topo.points.get(fv)? as usize
                    })
                };
                let mut out = [0.0f32; 4];
                let mut add = |e: usize, w: f32| -> Option<()> {
                    let v = p.value(e)?;
                    for (o, x) in out.iter_mut().zip(v) {
                        *o += w * x;
                    }
                    Some(())
                };
                match spot {
                    Spot::Corners {
                        corners,
                        weights,
                        n,
                    } => {
                        for (&c, &w) in corners.iter().zip(&weights).take(n as usize) {
                            add(element(c as usize)?, w)?;
                        }
                    }
                    Spot::Centre => {
                        let n = range.len();
                        for c in 0..n {
                            add(element(c)?, 1.0 / n as f32)?;
                        }
                    }
                }
                Some(out)
            }
        }
    }
}

/// What one geometry carries for the AOVs: its element map, its primvars,
/// and whether its placement mirrored the triangles (which exchanges the
/// barycentrics the kernel reports, as for the shading tables).
#[derive(Debug, Clone, Default)]
pub struct AovGeometry {
    pub elements: Option<Arc<ElementMap>>,
    pub primvars: Option<Arc<PrimvarSet>>,
    pub swapped: bool,
}

impl AovGeometry {
    /// Whether a hit's primvars need its barycentrics.
    pub fn needs_barycentrics(&self) -> bool {
        self.primvars.as_deref().is_some_and(PrimvarSet::needs_spot)
    }

    /// The authored face of a hit on `prim_id`, or [`NO_ID`].
    pub fn element(&self, prim_id: u32) -> u32 {
        self.elements
            .as_deref()
            .and_then(|m| m.face(prim_id))
            .unwrap_or(NO_ID)
    }

    /// The face and spot of a hit at barycentrics `(u, v)`, where the
    /// geometry has an element map.
    pub fn locate(&self, prim_id: u32, uv: Option<(f32, f32)>) -> Option<(u32, Spot)> {
        let map = self.elements.as_deref()?;
        match uv {
            Some((u, v)) => map.locate(prim_id, u, v, self.swapped),
            None => map.face(prim_id).map(|f| (f, Spot::Centre)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference values of MurmurHash3_x86_32, seed 0 (smhasher's own
    /// vectors), the tail lengths included.
    #[test]
    fn murmur3_matches_the_reference() {
        assert_eq!(murmur3_32(b""), 0);
        assert_eq!(murmur3_32(b"a"), 0x3c25_69b2);
        assert_eq!(murmur3_32(b"ab"), 0x9bbf_d75f);
        assert_eq!(murmur3_32(b"abc"), 0xb3dd_93fa);
        assert_eq!(murmur3_32(b"abcd"), 0x43ed_676a);
        assert_eq!(murmur3_32(b"Hello, world!"), 0xc036_3e43);
        assert_eq!(
            murmur3_32(b"The quick brown fox jumps over the lazy dog"),
            0x2e4f_f723
        );
    }

    #[test]
    fn names_resolve_by_run_and_runs_merge() {
        let mut b = IdentityBuilder::default();
        b.name(NameKey::Prim, 0..2, "/a");
        b.name(NameKey::Prim, 2..3, "/a");
        b.name(NameKey::Prim, 5..9, "/b");
        b.instance(5..7, 3);
        let id = b.build();
        let table = id.names(NameKey::Prim);
        assert_eq!(table.runs, vec![(0, 3, 0), (5, 9, 1)]);
        let hit = id.lookup(6);
        assert_eq!(hit.names[0], 1);
        assert_eq!(hit.hashes[0], murmur3_32(b"/b"));
        assert_eq!(hit.instance, 3);
        // An id no run covers names nothing.
        assert_eq!(id.lookup(4).names[0], NO_NAME);
        assert_eq!(id.lookup(8).instance, NO_ID);
        // A table nothing filled names nothing either.
        assert_eq!(id.lookup(0).names[1], NO_NAME);
    }

    /// A quad and a pentagon, each fanned from its first corner; a vertex
    /// primvar interpolates linearly over the hit triangle.
    #[test]
    fn a_cage_hit_interpolates_its_fan_triangle() {
        let topo = CageTopology::new(&[4, 5], &[0, 1, 2, 3, 3, 2, 4, 5, 6]).expect("valid");
        let map = ElementMap::Cage {
            faces: vec![0, 0, 1, 1, 1],
            fan: vec![1, 2, 1, 2, 3],
        };
        let set = PrimvarSet {
            vars: vec![Some(PrimvarData {
                interpolation: Interpolation::Vertex,
                values: (0..7).map(|i| [i as f32, 0.0, 0.0, 0.0]).collect(),
                indices: None,
            })],
            topology: Some(Arc::new(topo)),
            open: Vec::new(),
        };
        // Triangle 4 is the pentagon's (0, 3, 4) slice: points 3, 5, 6.
        let at = map.locate(4, 0.25, 0.5, false);
        assert_eq!(map.face(4), Some(1));
        let v = set.eval(0, at).expect("authored")[0];
        assert!(
            (v - (0.25 * 3.0 + 0.25 * 5.0 + 0.5 * 6.0)).abs() < 1e-6,
            "{v}"
        );
        // A mirrored bake exchanges the barycentrics back.
        let swapped = set
            .eval(0, map.locate(4, 0.5, 0.25, true))
            .expect("authored")[0];
        assert_eq!(swapped, v);
    }

    #[test]
    fn uniform_and_face_varying_read_the_face_and_its_corners() {
        let topo = Arc::new(CageTopology::new(&[3, 3], &[0, 1, 2, 2, 1, 3]).expect("valid"));
        let set = PrimvarSet {
            vars: vec![
                Some(PrimvarData {
                    interpolation: Interpolation::Uniform,
                    values: vec![[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
                    indices: None,
                }),
                Some(PrimvarData {
                    interpolation: Interpolation::FaceVarying,
                    values: vec![[0.0; 4], [10.0, 0.0, 0.0, 0.0]],
                    // Face 1's second corner alone is 10.
                    indices: Some(vec![0, 0, 0, 0, 1, 0]),
                }),
                None,
            ],
            topology: Some(topo),
            open: Vec::new(),
        };
        let map = ElementMap::Cage {
            faces: vec![0, 1],
            fan: vec![1, 1],
        };
        let at = map.locate(1, 0.5, 0.0, false);
        assert_eq!(set.eval(0, at), Some([0.0, 1.0, 0.0, 0.0]));
        assert_eq!(set.eval(1, at), Some([5.0, 0.0, 0.0, 0.0]));
        assert_eq!(set.eval(2, at), None);
        // Off a mesh, only a constant reads.
        assert_eq!(set.eval(0, None), None);
    }

    #[test]
    fn indices_out_of_range_read_as_unauthored() {
        let set = PrimvarSet {
            vars: vec![Some(PrimvarData {
                interpolation: Interpolation::Constant,
                values: vec![[1.0; 4]],
                indices: Some(vec![3]),
            })],
            topology: None,
            open: Vec::new(),
        };
        assert_eq!(set.eval(0, None), None);
    }
}
