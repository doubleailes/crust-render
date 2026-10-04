//! The bridge between the `crust-rt` kernel and the renderer: a
//! [`WorldBuilder`] pairs every attached kernel [`Geometry`] with its
//! [`Material`], and the committed [`World`] resolves kernel hits
//! (`geom_id`) back to materials — the ID-based split that keeps shading
//! state out of the intersection kernel.

use crate::hittable::HitRecord;
use crate::material::Material;
use crate::ray::Ray;
use crust_rt::{AABB, Geometry, InstanceHitId, MASK_ALL, RayMask, SceneBuilder};
use glam::{Affine3A, Vec3A};
use std::sync::Arc;

/// How one triangle sits inside the polygon it was cut from.
///
/// The importer fan-triangulates an n-gon anchored at its first vertex, so
/// triangle `k` of a face is `(v0, v[k], v[k+1])`. Recovering the polygon's own
/// `(u, v)` from a triangle's barycentrics needs to know which slice of the fan
/// the triangle is — hence one of these per triangle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FanSlice {
    /// The face was already a triangle: its barycentrics *are* its parametric
    /// coordinates, nothing to remap.
    Triangle,
    /// First half of a quad — `(v0, v1, v2)`.
    QuadLower,
    /// Second half of a quad — `(v0, v2, v3)`.
    QuadUpper,
    /// A slice of a face with more than four vertices. Ptex only defines quad
    /// and triangle faces, so such a face has no texture to sample and the
    /// lookup is suppressed rather than guessed at.
    Unmappable,
}

/// Per-triangle parametric density: how many texture-space units one unit of
/// the mesh's *own* space covers, i.e. `sqrt(parametric_area / world_area)`.
///
/// This is the conversion a texture footprint needs. A ray cone arrives at a
/// hit knowing only how wide it is in world units; a mip level is chosen in
/// texels, and the bridge between them is how stretched the chart is over
/// this particular triangle.
///
/// `0.0` means "no answer" — a degenerate triangle in either space — and
/// every consumer reads that as point-sampling the finest level, the same way
/// a `ZERO` tangent reads as "no tangent frame".
///
/// It is a *ratio of areas*, so it is invariant under exchanging two of a
/// triangle's vertices: a mirrored placement's index swap needs no correction
/// here, unlike every other lookup in this file. Do not add a `swapped` arm.
///
/// Always built in the mesh's local frame, never in world space. At
/// `flush_meshes` a baked placement gets a *shared* `FaceMap` (local) and a
/// *cloned* `UvMap`, and one placement scale cannot serve one table in world
/// space and the other in local; the placement's own scale is recorded
/// separately, on `SideTables`.
fn triangle_density(param_area: f32, p0: Vec3A, p1: Vec3A, p2: Vec3A) -> f32 {
    let world_area = 0.5 * (p1 - p0).cross(p2 - p0).length();
    if world_area <= 1e-20 || param_area <= 1e-20 {
        return 0.0;
    }
    (param_area / world_area).sqrt()
}

/// The ray cone's footprint *across the surface* at a hit, in world units.
///
/// Two steps. The cone's own diameter after `t · |dir|` world units of
/// travel — `t` is in the ray's unnormalized parameterisation, which is why
/// the direction's length appears — and then the grazing stretch: a cone
/// meeting a surface at angle θ paints an ellipse `1/|cos θ|` longer than its
/// cross-section.
///
/// That stretch is applied **here and discarded**. It must never be folded
/// back into the ray's cone: it would then multiply again at every subsequent
/// bounce, and a handful of grazing hits would send every texture to its 1×1
/// level. The clamp is the other half of the same concern — `normal` is the
/// interpolated shading normal, which goes to a zero dot product at a
/// smooth-shaded silhouette where the real footprint is perfectly finite.
fn footprint_width(ray: &Ray, t: f32, normal: Vec3A) -> f32 {
    let dir = ray.direction();
    let len = dir.length();
    let width = ray.cone().width_at(t * len);
    if width <= 0.0 {
        return 0.0;
    }
    let cos = if len > 0.0 {
        (dir.dot(normal) / len).abs()
    } else {
        1.0
    };
    width / cos.max(MIN_GRAZING_COS)
}

/// Floor on the grazing `1/|cos θ|` footprint stretch — five times the
/// cross-section, and no more.
const MIN_GRAZING_COS: f32 = 0.2;

/// Half the absolute cross product of a UV triangle's two edges — its area in
/// parametric space.
fn uv_area(uv: &[[f32; 2]; 3]) -> f32 {
    let (du1, dv1) = (uv[1][0] - uv[0][0], uv[1][1] - uv[0][1]);
    let (du2, dv2) = (uv[2][0] - uv[0][0], uv[2][1] - uv[0][1]);
    0.5 * (du1 * dv2 - du2 * dv1).abs()
}

/// One refined quad's place in its base-cage face: the dyadic cell
/// `[ou, ou + 2^-depth] × [ov, ov + 2^-depth]` it covers, and which of its
/// four corners sits at the cell's origin. Eight bytes per triangle, where
/// the three corner coordinates it reproduces were twenty-four.
///
/// Every value it stands for is a dyadic fraction `k / 2^depth` with
/// `depth ≤ 6` (the import's level cap), exact in `f32`, and the refined
/// face-varying channel this replaces produced those same values by
/// halving — so [`SubFace::corners`] is bit-identical to what the channel
/// held (`sub_face_corners_match_the_refined_channel`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SubFace {
    /// `origin_u` (bits 0–7), `origin_v` (8–15), `depth` (16–19), `rot`
    /// (20–21): the cell's integer origin at its depth, and the corner of
    /// the refined quad that lies at the origin.
    packed: u32,
}

impl SubFace {
    /// The cell holding a refined quad whose corners, in the quad's own
    /// vertex order, are `corners`; `None` when they are not the corners of
    /// one dyadic cell of the unit square at a depth up to 15.
    pub fn from_corners(corners: &[[f32; 2]; 4]) -> Option<SubFace> {
        let (mut lo_u, mut lo_v, mut hi_u, mut hi_v) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for c in corners {
            lo_u = lo_u.min(c[0]);
            lo_v = lo_v.min(c[1]);
            hi_u = hi_u.max(c[0]);
            hi_v = hi_v.max(c[1]);
        }
        let side = hi_u - lo_u;
        if side <= 0.0 || side != hi_v - lo_v {
            return None;
        }
        let depth = (-side.log2()).round();
        if !(0.0..=15.0).contains(&depth) {
            return None;
        }
        let scale = (1u32 << depth as u32) as f32;
        let (ou, ov) = ((lo_u * scale).round(), (lo_v * scale).round());
        if !(0.0..256.0).contains(&ou) || !(0.0..256.0).contains(&ov) {
            return None;
        }
        let rot = corners.iter().position(|c| c[0] == lo_u && c[1] == lo_v)? as u32;
        let sub = SubFace {
            packed: ou as u32 | (ov as u32) << 8 | (depth as u32) << 16 | rot << 20,
        };
        // Only an exact round trip is a correct answer.
        (sub.corners() == *corners).then_some(sub)
    }

    /// The four corner coordinates of the refined quad, in its own vertex
    /// order.
    #[inline]
    pub fn corners(&self) -> [[f32; 2]; 4] {
        let p = self.packed;
        let depth = (p >> 16) & 0xF;
        let rot = ((p >> 20) & 0x3) as usize;
        let s = 1.0 / (1u32 << depth) as f32;
        let ou = (p & 0xFF) as f32 * s;
        let ov = ((p >> 8) & 0xFF) as f32 * s;
        let cell = [[ou, ov], [ou + s, ov], [ou + s, ov + s], [ou, ov + s]];
        // Corner `rot` of the quad is the cell's origin, so quad corner `k`
        // is cell corner `k - rot`.
        std::array::from_fn(|k| cell[(k + 4 - rot) & 3])
    }

    /// The corners of the triangle `slice` cuts from the quad, in the fan's
    /// vertex order — `None` for a slice no quad has.
    #[inline]
    fn triangle_corners(&self, slice: FanSlice) -> Option<[[f32; 2]; 3]> {
        let [c0, c1, c2, c3] = self.corners();
        match slice {
            FanSlice::QuadLower => Some([c0, c1, c2]),
            FanSlice::QuadUpper => Some([c0, c2, c3]),
            FanSlice::Triangle | FanSlice::Unmappable => None,
        }
    }
}

/// Maps each triangle of one distinct mesh back to the polygon it came from.
///
/// Ptex face ids are indices into a mesh's *original* `faceVertexCounts`, but
/// the kernel only knows about triangles, so something has to survive
/// triangulation to bridge the two. Held behind an `Arc` because a mesh placed
/// as several instances shares one table across every placement.
///
/// Both vectors are indexed by the kernel's `prim_id` and are the same length
/// as the triangle list handed to the kernel.
pub struct FaceMap {
    /// Source polygon index per triangle — the Ptex face id.
    pub faces: Vec<u32>,
    /// Which slice of that polygon's fan the triangle is.
    pub slices: Vec<FanSlice>,
    /// Which dyadic cell of its base-cage face each triangle's refined quad
    /// is, index-parallel with `faces`. Subdivided meshes only: their
    /// triangles cover a *sub*-rectangle of the base-cage face, which no fan
    /// slice can express — `faces` then carries base-cage ids and each
    /// triangle's cell gives its corners' coordinates in the cage face.
    /// `None` for unsubdivided meshes, whose triangles resolve through
    /// `slices` alone.
    pub sub: Option<Vec<SubFace>>,
    /// Each triangle's three corners in its Ptex face, index-parallel with
    /// `faces`. Per-face tessellated meshes only: their stitched grids are not
    /// dyadic cells, so neither `slices` nor `sub` can express them. Checked
    /// before `sub`.
    pub corners: Option<Vec<[[f32; 2]; 3]>>,
    /// Face-space units per unit of local space, per triangle — see
    /// [`triangle_density`]. Empty when [`FaceMap::build_density`] was never
    /// called, which every consumer reads as "point-sample".
    pub density: Vec<f32>,
}

impl FaceMap {
    /// Resolves a triangle hit's barycentrics into `(face_id, u, v)` in the
    /// source polygon's parametric space, or `None` when the triangle has no
    /// Ptex-addressable face.
    ///
    /// `u` weights the triangle's second vertex and `v` its third (the
    /// kernel's Woop convention), and a quad's corners are parameterised
    /// `v0 = (0,0)`, `v1 = (1,0)`, `v2 = (1,1)`, `v3 = (0,1)` — Ptex's own
    /// quad convention. Substituting the fan's vertices into
    /// `uv = (1-u-v)·uv0 + u·uv1 + v·uv2` gives each arm below.
    ///
    /// `swapped` undoes the index swap a mirrored placement bakes in (see
    /// `bake_indices` in the importer): exchanging a triangle's second and
    /// third vertices exchanges the meaning of `u` and `v`.
    pub fn resolve(&self, prim_id: u32, u: f32, v: f32, swapped: bool) -> Option<(u32, f32, f32)> {
        let i = prim_id as usize;
        let (&face, &slice) = (self.faces.get(i)?, self.slices.get(i)?);
        let (u, v) = if swapped { (v, u) } else { (u, v) };
        if let Some(corners) = &self.corners {
            if slice == FanSlice::Unmappable {
                return None;
            }
            let [a, b, c] = *corners.get(i)?;
            let w = 1.0 - u - v;
            return Some((
                face,
                (w * a[0] + u * b[0] + v * c[0]).clamp(0.0, 1.0),
                (w * a[1] + u * b[1] + v * c[1]).clamp(0.0, 1.0),
            ));
        }
        if let Some(sub) = &self.sub {
            // Corner coordinates come out in the triangle's *original*
            // vertex order, so the swap above already restored the
            // barycentrics to that order and plain interpolation is right
            // for mirrors too.
            let [a, b, c] = sub.get(i)?.triangle_corners(slice)?;
            let w = 1.0 - u - v;
            return Some((
                face,
                (w * a[0] + u * b[0] + v * c[0]).clamp(0.0, 1.0),
                (w * a[1] + u * b[1] + v * c[1]).clamp(0.0, 1.0),
            ));
        }
        let (fu, fv) = match slice {
            // (v0,v1,v2) -> u·(1,0) + v·(1,1)
            FanSlice::QuadLower => (u + v, v),
            // (v0,v2,v3) -> u·(1,1) + v·(0,1)
            FanSlice::QuadUpper => (u, u + v),
            FanSlice::Triangle => (u, v),
            FanSlice::Unmappable => return None,
        };
        Some((face, fu.clamp(0.0, 1.0), fv.clamp(0.0, 1.0)))
    }

    /// Face-space units per unit of local space for `prim_id`, or `0.0` when
    /// the table was never built or the triangle is degenerate.
    #[inline]
    pub fn density(&self, prim_id: u32) -> f32 {
        self.density.get(prim_id as usize).copied().unwrap_or(0.0)
    }

    /// Builds the per-triangle density from the mesh's own (local) vertices.
    ///
    /// The parametric area is `0.5` for every mapped fan slice, and that is
    /// exact rather than approximate: `Triangle` is the identity, and both
    /// quad arms of [`FaceMap::resolve`] are unit-determinant shears, so all
    /// three carry the standard simplex onto a region of area exactly half
    /// the face's unit square.
    ///
    /// A *subdivided* mesh is the exception and must not use that constant.
    /// Its triangles carry explicit corner UVs covering a sub-rectangle of
    /// the base-cage face, so the area is `4^-L` of the constant; at level 3
    /// the constant would over-estimate the footprint 64× and every Ptex
    /// lookup on the mesh would read its 1×1 level.
    pub fn build_density(&mut self, verts: &[[f32; 3]], tris: &[[u32; 3]]) {
        self.density.clear();
        self.density.reserve(tris.len());
        for (t, tri) in tris.iter().enumerate() {
            let param = match (&self.sub, self.slices.get(t)) {
                (_, Some(FanSlice::Unmappable)) | (_, None) => 0.0,
                _ if self.corners.is_some() => self
                    .corners
                    .as_ref()
                    .and_then(|c| c.get(t))
                    .map_or(0.0, uv_area),
                (Some(sub), Some(&slice)) => sub
                    .get(t)
                    .and_then(|f| f.triangle_corners(slice))
                    .map_or(0.0, |uv| uv_area(&uv)),
                (None, Some(_)) => 0.5,
            };
            self.density.push(triangle_density(
                param,
                Vec3A::from_array(verts[tri[0] as usize]),
                Vec3A::from_array(verts[tri[1] as usize]),
                Vec3A::from_array(verts[tri[2] as usize]),
            ));
        }
    }
}

/// A mesh's `primvars:st` texture coordinates, and the tangent frame a
/// normal map needs to be read in.
///
/// Parallel to [`FaceMap`] but answering a different question: `FaceMap` maps a
/// triangle back to the *polygon* it was cut from (Ptex's addressing), whereas
/// this carries the mesh's UV chart, which a UDIM image set indexes. A mesh can
/// want either, both, or neither.
///
/// The chart is its authored (or refined) values plus one index per triangle
/// corner, because USD's `st` is usually **faceVarying**: a vertex on a UV
/// seam carries a different coordinate in each face touching it, so there is
/// no per-point value to interpolate — but the values themselves are shared
/// across the faces that agree, and indexing them costs 12 bytes per triangle
/// where expanded corners cost 24. Held behind an `Arc` for the same reason
/// as `FaceMap` — one distinct mesh, many placements.
pub struct UvMap {
    /// The chart's `(u, v)` values; the last entry is the `(0, 0)` every
    /// corner that could not be indexed points at.
    pub values: Vec<[f32; 2]>,
    /// Per triangle, the index into `values` of each corner, in the
    /// triangle's *original* vertex order.
    pub corners: Vec<[u32; 3]>,
    /// Chart UV units per unit of local space, per triangle — see
    /// [`triangle_density`]. Empty when [`UvMap::build_density`] was never
    /// called, which every consumer reads as "point-sample".
    ///
    /// Kept as a table (4 bytes per triangle) rather than derived at the hit:
    /// it is defined in the mesh's *local* frame, which a baked mesh no
    /// longer has once its vertices are in world space.
    pub density: Vec<f32>,
}

impl UvMap {
    /// The three corner coordinates of `prim_id`, in original vertex order.
    #[inline]
    pub fn corner_uvs(&self, prim_id: u32) -> Option<[[f32; 2]; 3]> {
        let [a, b, c] = *self.corners.get(prim_id as usize)?;
        Some([
            self.values[a as usize],
            self.values[b as usize],
            self.values[c as usize],
        ])
    }

    /// Interpolates the hit triangle's corner UVs and returns them with the
    /// triangle's tangent, or `None` when `prim_id` is out of range.
    ///
    /// `swapped` undoes a mirrored placement's index swap exactly as
    /// [`FaceMap::resolve`] does: corner UVs are stored in original vertex
    /// order, so restoring the barycentrics to that order is all it takes.
    ///
    /// `verts` are the triangle's world-space vertices in the *kernel's*
    /// order (swapped for a mirrored placement), from which the tangent is
    /// derived here, per hit, instead of being stored per triangle; `None`
    /// when the host cannot name them, which yields no tangent frame.
    pub fn resolve(
        &self,
        prim_id: u32,
        u: f32,
        v: f32,
        swapped: bool,
        verts: Option<[Vec3A; 3]>,
    ) -> Option<((f32, f32), Vec3A)> {
        let [a, b, c] = self.corner_uvs(prim_id)?;
        let (u, v) = if swapped { (v, u) } else { (u, v) };
        let w = 1.0 - u - v;
        let tangent = verts.map_or(Vec3A::ZERO, |[p0, p1, p2]| {
            // Back into the original vertex order the corners are in: a
            // mirrored placement's bake exchanged the second and third.
            let (p1, p2) = if swapped { (p2, p1) } else { (p1, p2) };
            tangent_of(&[a, b, c], p0, p1, p2)
        });
        Some((
            (
                w * a[0] + u * b[0] + v * c[0],
                w * a[1] + u * b[1] + v * c[1],
            ),
            tangent,
        ))
    }

    /// Chart UV units per unit of local space for `prim_id`, or `0.0` when
    /// the table was never built or the triangle is degenerate.
    #[inline]
    pub fn density(&self, prim_id: u32) -> f32 {
        self.density.get(prim_id as usize).copied().unwrap_or(0.0)
    }

    /// Builds the per-triangle density from the mesh's own (local) vertices.
    ///
    /// Wants *local* vertices, not world-space ones, and is therefore built
    /// once per distinct mesh rather than once per placement — see
    /// [`triangle_density`] for why.
    pub fn build_density(&mut self, verts: &[[f32; 3]], tris: &[[u32; 3]]) {
        self.density.clear();
        self.density.reserve(tris.len());
        for (t, tri) in tris.iter().enumerate() {
            let param = self.corner_uvs(t as u32).map_or(0.0, |uv| uv_area(&uv));
            self.density.push(triangle_density(
                param,
                Vec3A::from_array(verts[tri[0] as usize]),
                Vec3A::from_array(verts[tri[1] as usize]),
                Vec3A::from_array(verts[tri[2] as usize]),
            ));
        }
    }
}

/// The tangent of one triangle: the standard solve of
/// `[dP1; dP2] = [duv1; duv2] · [T; B]` for `T` — the direction in which `u`
/// grows across the triangle, from its world-space vertices and corner UVs
/// in the same vertex order. A degenerate UV triangle (zero area in texture
/// space, which a collapsed or unwrapped-flat face produces) has no such
/// direction; it gets `ZERO`, which the shader reads as "no tangent frame"
/// rather than as a valid but arbitrary one.
///
/// Was a per-triangle table built at import; now computed at the hit from
/// the kernel's shared vertices, with the same arithmetic.
pub fn tangent_of(uv: &[[f32; 2]; 3], p0: Vec3A, p1: Vec3A, p2: Vec3A) -> Vec3A {
    let (e1, e2) = (p1 - p0, p2 - p0);
    let (du1, dv1) = (uv[1][0] - uv[0][0], uv[1][1] - uv[0][1]);
    let (du2, dv2) = (uv[2][0] - uv[0][0], uv[2][1] - uv[0][1]);
    let det = du1 * dv2 - du2 * dv1;
    if det.abs() > 1e-20 {
        let tan = (e1 * dv2 - e2 * dv1) / det;
        if tan.length_squared() > 1e-30 {
            tan.normalize()
        } else {
            Vec3A::ZERO
        }
    } else {
        Vec3A::ZERO
    }
}

/// Where the vertices of a hit triangle can be read, per geometry, so a
/// tangent frame can be derived at the hit instead of stored per triangle.
///
/// A hit carries `(geom_id, prim_id)` and nothing about the placement it was
/// traversed through, so the only geometries whose vertices can be recovered
/// are those where the id *is* the placement: a baked mesh (the top-level
/// scene holds its world-space vertices under its own id) and a direct,
/// unlabelled, static instance (one scene, one transform, recorded here at
/// attach time). Everything else is `Unresolved`, deliberately: a prototype
/// part placed through an instancer's group reports a forwarded slot id
/// (`InstanceHitId::As` / `Offset`) that names the slot, not the placement
/// traversed — two differently transformed placements of one prototype
/// share it, so no transform can be recovered from the hit; and a
/// motion-blurred instance was intersected through a transform interpolated
/// at the ray's time, which no retained start transform reproduces. Neither
/// consults any scene: resolving a forwarded id against the top-level scene
/// would hand back whatever geometry happens to sit at that index. Both
/// shade normal maps with the geometric normal, as every instance did before
/// tangents were derived at the hit.
enum VertexSource {
    /// No vertices, no tangent frame.
    Unresolved,
    /// A baked top-level triangle mesh: the scene's own table, world space.
    Baked,
    /// A direct, unlabelled, static instance: that scene in local space,
    /// carried into the world through its placement.
    Placed(Arc<crust_rt::Scene>, Affine3A),
}

/// A geometry's side tables, plus whether its placement mirrored the winding.
///
/// One struct rather than two parallel `Vec`s because the mirror flag and the
/// barycentric convention it corrects are shared: both tables index the same
/// triangles and both must undo the same swap.
struct SideTables {
    map: Option<Arc<FaceMap>>,
    uv: Option<Arc<UvMap>>,
    swapped: bool,
    /// Where a hit's triangle vertices may be read from, decided when the
    /// geometry is attached — see [`VertexSource`].
    vertices: VertexSource,
    /// The placement's uniform scale — `cbrt(|det|)` of its linear part.
    ///
    /// Both tables' densities are in the mesh's *local* frame, so a texture
    /// footprint arriving in world units has to be divided by this before it
    /// can be multiplied by one. `1.0` where the table was already built on
    /// world-space vertices.
    scale: f32,
}

/// Hand-written rather than derived, for one field: `scale` must default to
/// `1.0`, not `0.0`.
///
/// `attach_masked` pushes a `SideTables` for *every* geometry, textured or
/// not, so a derived default would divide every footprint by zero and hand
/// each texture an infinite width — which reads as a perfectly plausible
/// coarse mip and would be a miserable thing to track down.
impl Default for SideTables {
    fn default() -> Self {
        SideTables {
            map: None,
            uv: None,
            swapped: false,
            vertices: VertexSource::Unresolved,
            scale: 1.0,
        }
    }
}

/// Scene-construction container: kernel geometries plus the material
/// table indexed by `geom_id`. Commit into a [`World`] to render.
#[derive(Default)]
pub struct WorldBuilder {
    rt: SceneBuilder,
    materials: Vec<Arc<dyn Material>>,
    /// Sparse, indexed by `geom_id`: only geometries whose material actually
    /// samples a per-face or UV texture carry a table, so a scene with no
    /// textures pays one empty entry per geometry and nothing more.
    faces: Vec<SideTables>,
    /// Per `geom_id`, the light-link class of its receiver prim, when any
    /// light authors a `lightLink`; empty otherwise (see
    /// [`World::light_class`]).
    light_classes: Vec<u16>,
}

impl WorldBuilder {
    pub fn new() -> Self {
        Default::default()
    }

    /// Attaches a geometry with its material; returns the `geom_id` that
    /// hits on this geometry will carry.
    pub fn attach(&mut self, geometry: Geometry, material: Arc<dyn Material>) -> u32 {
        self.attach_masked(geometry, material, MASK_ALL)
    }

    /// Attaches a geometry visible only to the ray categories in `mask`
    /// (see the `MASK_*` constants).
    pub fn attach_masked(
        &mut self,
        geometry: Geometry,
        material: Arc<dyn Material>,
        mask: RayMask,
    ) -> u32 {
        self.attach_labelled(geometry, material, mask, InstanceHitId::Own)
    }

    /// Attaches an instance whose hits report `label` rather than its own
    /// id — see `crust_rt::InstanceHitId`. `material` is bound to the
    /// instance's own id; an `Offset` label that reaches further ids needs
    /// them claimed too (with [`WorldBuilder::reserve_slot`]), since those
    /// ids are what hits will carry and what the material table is read at.
    pub fn attach_labelled(
        &mut self,
        geometry: Geometry,
        material: Arc<dyn Material>,
        mask: RayMask,
        label: InstanceHitId,
    ) -> u32 {
        let vertices = Self::vertex_source(&geometry, label);
        let id = self.rt.attach_labelled(geometry, mask, label);
        self.materials.push(material);
        self.faces.push(SideTables {
            vertices,
            ..SideTables::default()
        });
        debug_assert_eq!(id as usize + 1, self.materials.len());
        id
    }

    /// Where a hit's vertices can be read from — see [`VertexSource`] for
    /// why only these two shapes resolve.
    fn vertex_source(geometry: &Geometry, label: InstanceHitId) -> VertexSource {
        match geometry {
            Geometry::TriangleMesh { .. } if label == InstanceHitId::Own => VertexSource::Baked,
            Geometry::Instance {
                scene,
                transform,
                transform_end: None,
            } if label == InstanceHitId::Own => VertexSource::Placed(Arc::clone(scene), *transform),
            _ => VertexSource::Unresolved,
        }
    }

    /// Records the per-face table for a geometry, so hits on it can report a
    /// Ptex face id and parametric `(u, v)`.
    ///
    /// `swapped` when the placement mirrored the triangle winding, which
    /// exchanges the barycentrics the kernel reports.
    ///
    /// # Panics
    /// If `id` was never attached or reserved.
    pub fn set_face_map(&mut self, id: u32, map: Arc<FaceMap>, swapped: bool) {
        let t = &mut self.faces[id as usize];
        t.map = Some(map);
        t.swapped = swapped;
    }

    /// Records the per-triangle UV table for a geometry, so hits on it report
    /// `primvars:st` coordinates and a tangent frame.
    ///
    /// `swapped` when the placement mirrored the triangle winding, exactly as
    /// for [`WorldBuilder::set_face_map`] — a geometry carrying both tables
    /// shares the one flag, since both index the same triangles.
    ///
    /// # Panics
    /// If `id` was never attached or reserved.
    pub fn set_uv_map(&mut self, id: u32, map: Arc<UvMap>, swapped: bool) {
        let t = &mut self.faces[id as usize];
        t.uv = Some(map);
        t.swapped = swapped;
    }

    /// Records the uniform scale of this geometry's placement, which converts
    /// a world-space texture footprint into the local frame both side tables'
    /// densities are expressed in.
    ///
    /// `cbrt(|det|)` is the geometric mean of the three axis scales, so a
    /// non-uniform placement is filtered by that mean: a `(1, 1, 10)` scale
    /// reads a mip up to ~4.6× off on the stretched axis. That degrades to a
    /// slightly wrong level, never to a wrong lookup, and an exact answer
    /// would need per-axis densities the isotropic cone could not use anyway.
    ///
    /// Only needed where the tables are in local space, i.e. everywhere the
    /// vertices were not baked into world space first.
    pub fn set_placement_scale(&mut self, id: u32, scale: f32) {
        if scale.is_finite() && scale > 0.0 {
            self.faces[id as usize].scale = scale;
        }
    }

    /// Number of geometries attached so far.
    pub fn count(&self) -> usize {
        self.materials.len()
    }

    /// Claims a `geom_id` and binds its material now, leaving the geometry to
    /// be supplied later by [`WorldBuilder::set_geometry`].
    ///
    /// `geom_id`s are handed out in attach order and index the material
    /// table, so a caller that wants to *decide* a geometry's representation
    /// late — but keep its position in the table — reserves the slot here.
    /// The importer uses this to defer the instance-vs-bake choice for a mesh
    /// until it knows how many times that mesh is placed, without perturbing
    /// the id every other part of the import (lights especially) already
    /// depends on.
    ///
    /// A slot never filled in commits to zero primitives: harmless, just
    /// invisible.
    pub fn reserve_slot(&mut self, material: Arc<dyn Material>, mask: RayMask) -> u32 {
        self.attach_masked(SceneBuilder::empty_geometry(), material, mask)
    }

    /// Fills in a slot from [`WorldBuilder::reserve_slot`].
    ///
    /// # Panics
    /// If `id` was never reserved.
    pub fn set_geometry(&mut self, id: u32, geometry: Geometry) {
        self.faces[id as usize].vertices = Self::vertex_source(&geometry, InstanceHitId::Own);
        self.rt.set_geometry(id, geometry);
    }

    /// The ray mask the geometry at `id` was attached with.
    pub fn mask(&self, id: u32) -> RayMask {
        self.rt.mask(id)
    }

    /// Replaces the ray mask of an attached geometry — see
    /// `crust_rt::SceneBuilder::set_mask`.
    pub fn set_mask(&mut self, id: u32, mask: RayMask) {
        self.rt.set_mask(id, mask);
    }

    /// Reserves capacity for `additional` more geometries — see
    /// `crust_rt::SceneBuilder::reserve`. Callers importing a known-size
    /// batch (a `PointInstancer` with N placements) should call this
    /// first: growing `materials` and the kernel's geometry table by
    /// repeated doubling otherwise re-copies everything so far at each
    /// step, and can leave up to ~2x the final size over-allocated.
    pub fn reserve(&mut self, additional: usize) {
        self.rt.reserve(additional);
        self.materials.reserve(additional);
        self.faces.reserve(additional);
    }

    /// Builds the acceleration structure (parallel, deterministic).
    #[must_use = "the committed world is the only way to intersect it"]
    pub fn commit(self) -> World {
        let cutouts = self.materials.iter().any(|m| m.has_cutout());
        World {
            scene: self.rt.commit_with(crate::commit_options()),
            materials: self.materials,
            faces: self.faces,
            cutouts,
            light_classes: self.light_classes,
        }
    }

    /// Installs the per-`geom_id` light-link classes (one entry per attached
    /// geometry). Only the importer's link resolution calls this, and only
    /// when some light authors a `lightLink`.
    ///
    /// # Panics
    /// If `classes` does not have one entry per attached geometry.
    pub fn set_light_classes(&mut self, classes: Vec<u16>) {
        assert_eq!(classes.len(), self.count(), "one class per geometry");
        self.light_classes = classes;
    }
}

/// A successful world intersection: the material-facing [`HitRecord`],
/// the material looked up from the hit's `geom_id`, and the IDs
/// themselves (the integrator attributes bounce-hit lights by `geom_id`).
pub struct WorldHit<'a> {
    pub rec: HitRecord,
    pub mat: &'a dyn Material,
    pub geom_id: u32,
    pub prim_id: u32,
}

/// The committed scene geometry the renderer traces against.
pub struct World {
    scene: crust_rt::Scene,
    materials: Vec<Arc<dyn Material>>,
    faces: Vec<SideTables>,
    /// Whether any material has a cutout ([`Material::has_cutout`]).
    cutouts: bool,
    light_classes: Vec<u16>,
}

impl World {
    /// The light-link class of the prim a hit on `geom_id` belongs to — what
    /// [`LightList::illuminates`](crate::LightList::illuminates) is asked
    /// about. [`EVERY_CLASS`](crate::EVERY_CLASS) when no light authors a
    /// `lightLink`.
    #[inline]
    pub fn light_class(&self, geom_id: u32) -> u16 {
        self.light_classes
            .get(geom_id as usize)
            .copied()
            .unwrap_or(crate::EVERY_CLASS)
    }

    /// Closest hit in `(t_min, t_max)` with its material resolved.
    #[must_use]
    pub fn intersect(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<WorldHit<'_>> {
        let h = self.scene.intersect(ray.rt(), t_min, t_max)?;
        // The kernel reports triangle barycentrics; a per-face-textured mesh
        // needs the polygon they belong to. Geometries without a table (the
        // overwhelming majority) skip straight past this.
        let tables = &self.faces[h.geom_id as usize];
        let face = tables.map.as_ref().and_then(|m| {
            m.resolve(h.prim_id, h.u, h.v, tables.swapped)
                .map(|(id, u, v)| crate::hittable::FaceHit { id, uv: (u, v) })
        });
        let (uv, tangent, has_uv) = match &tables.uv {
            Some(m) => {
                let verts = self.world_vertices(h.geom_id, h.prim_id, tables);
                match m.resolve(h.prim_id, h.u, h.v, tables.swapped, verts) {
                    Some((uv, tangent)) => (uv, tangent, true),
                    None => ((0.0, 0.0), Vec3A::ZERO, false),
                }
            }
            // No UV map: a curve's direction, which the kernel carries to
            // world space through every placement (zero for anything else) —
            // the strand a fibre BSDF shades along. Tested before normalising:
            // nearly every hit has none, and the square root is not free.
            None if h.dpdu == Vec3A::ZERO => ((0.0, 0.0), Vec3A::ZERO, false),
            None => ((0.0, 0.0), h.dpdu.normalize_or_zero(), false),
        };
        // The ray's texture footprint, converted into each parameterisation
        // the shader might index. Zero unless the ray carries a cone *and*
        // this geometry carries the density to convert it with, and zero
        // reads as "point-sample the finest level" everywhere downstream.
        let (uv_width, face_width) = match (tables.map.is_some() || tables.uv.is_some())
            .then(|| footprint_width(ray, h.t, h.normal))
        {
            Some(w) if w > 0.0 => {
                // Both densities are in the mesh's local frame, so undo the
                // placement's scale before applying one.
                let local = w / tables.scale;
                (
                    tables
                        .uv
                        .as_ref()
                        .map_or(0.0, |m| local * m.density(h.prim_id)),
                    tables
                        .map
                        .as_ref()
                        .map_or(0.0, |m| local * m.density(h.prim_id)),
                )
            }
            _ => (0.0, 0.0),
        };
        Some(WorldHit {
            rec: HitRecord {
                p: ray.at(h.t),
                normal: h.normal,
                t: h.t,
                front_face: h.front_face,
                face,
                uv,
                tangent,
                has_uv,
                uv_width,
                face_width,
            },
            mat: self.materials[h.geom_id as usize].as_ref(),
            geom_id: h.geom_id,
            prim_id: h.prim_id,
        })
    }

    /// The world-space vertices of the triangle a hit landed on, in the
    /// kernel's vertex order: read from the top-level scene for a baked
    /// mesh, or through the placement of a direct instance. `None` where
    /// neither applies — see [`VertexSource`].
    fn world_vertices(
        &self,
        geom_id: u32,
        prim_id: u32,
        tables: &SideTables,
    ) -> Option<[Vec3A; 3]> {
        match &tables.vertices {
            VertexSource::Unresolved => None,
            VertexSource::Baked => self.scene.triangle_vertices(geom_id, prim_id),
            VertexSource::Placed(scene, l2w) => scene
                .triangle_vertices(0, prim_id)
                .map(|vs| vs.map(|v| l2w.transform_point3a(v))),
        }
    }

    /// Early-exit occlusion query — the shadow-ray fast path.
    #[must_use]
    pub fn occluded(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        self.scene.occluded(ray.rt(), t_min, t_max)
    }

    /// Whether any geometry's material can be less than fully present
    /// ([`Material::has_cutout`]). When none is, a segment's closest hit is
    /// where it ends and any hit occludes a shadow ray, which is what the
    /// integrator's fast paths assume; otherwise it asks each hit on a
    /// cutout material for its opacity.
    #[inline]
    pub fn has_cutouts(&self) -> bool {
        self.cutouts
    }

    /// World bounds of all geometry; `None` for an empty world.
    pub fn bounds(&self) -> Option<AABB> {
        self.scene.bounds()
    }

    /// Number of geometries (`geom_id`s are `0..count`).
    pub fn count(&self) -> usize {
        self.materials.len()
    }

    /// Number of top-level primitives in the acceleration structure.
    ///
    /// An instance counts as **one** primitive however much geometry it
    /// references, so comparing this against a scene's triangle count is
    /// how you tell instanced geometry from baked geometry: an instanced
    /// import stays flat as placements multiply, a baked one does not.
    pub fn primitive_count(&self) -> usize {
        self.scene.primitive_count()
    }

    /// Top-level primitives split by kind — see
    /// [`crust_rt::Scene::primitive_breakdown`].
    pub fn primitive_breakdown(&self) -> crust_rt::PrimitiveBreakdown {
        self.scene.primitive_breakdown()
    }

    /// Primitives resident in memory, counting each distinct instanced
    /// prototype once — see [`crust_rt::Scene::unique_primitive_breakdown`].
    pub fn unique_primitive_breakdown(&self) -> crust_rt::PrimitiveBreakdown {
        self.scene.unique_primitive_breakdown()
    }

    /// Top-level primitive extents relative to the scene — see
    /// [`crust_rt::Scene::primitive_extents`].
    pub fn primitive_extents(&self) -> (usize, f32, f32, f32) {
        self.scene.primitive_extents()
    }

    /// See [`crust_rt::Scene::describe_instances`].
    #[cfg(feature = "traversal-stats")]
    pub fn describe_instances(
        &self,
        ids: &std::collections::HashSet<u32>,
    ) -> Vec<(u32, crust_rt::AABB, usize, usize)> {
        self.scene.describe_instances(ids)
    }

    /// Does anything move over the shutter interval — see
    /// [`crust_rt::Scene::has_motion`]. When `false` the integrator can skip
    /// sampling the shutter coordinate entirely, because nothing reads it.
    pub fn has_motion(&self) -> bool {
        self.scene.has_motion()
    }

    /// Exact kernel-resident bytes — see
    /// [`crust_rt::Scene::memory_footprint`].
    pub fn memory_footprint(&self) -> crust_rt::MemoryFootprint {
        self.scene.memory_footprint()
    }

    /// Distinct materials in the table, counted once however many
    /// geometries share one, grouped by [`Material::kind`] — Guerilla's
    /// "allocated materials". Heaviest kind first.
    pub fn material_breakdown(&self) -> Vec<(&'static str, usize)> {
        let mut seen = std::collections::HashSet::new();
        crate::stats::breakdown(
            self.materials
                .iter()
                .filter(|m| seen.insert(Arc::as_ptr(m) as *const () as usize))
                .map(|m| m.kind()),
        )
    }

    /// The material bound to a geometry.
    pub fn material(&self, geom_id: u32) -> &dyn Material {
        self.materials[geom_id as usize].as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quad fan-triangulated into two triangles, as the importer emits it.
    fn quad() -> FaceMap {
        FaceMap {
            faces: vec![7, 7],
            slices: vec![FanSlice::QuadLower, FanSlice::QuadUpper],
            sub: None,
            corners: None,
            density: Vec::new(),
        }
    }

    /// The four corners are the whole contract: get one wrong and a texture
    /// lands rotated or mirrored on the surface, which is exactly the kind of
    /// error that still looks plausible in a render.
    #[test]
    fn quad_corners_map_to_the_unit_square() {
        let m = quad();
        // Lower triangle (v0,v1,v2): barycentric (u,v) weights v1 and v2.
        // v0 -> (0,0)
        assert_eq!(m.resolve(0, 0.0, 0.0, false), Some((7, 0.0, 0.0)));
        // v1 -> (1,0)
        assert_eq!(m.resolve(0, 1.0, 0.0, false), Some((7, 1.0, 0.0)));
        // v2 -> (1,1)
        assert_eq!(m.resolve(0, 0.0, 1.0, false), Some((7, 1.0, 1.0)));

        // Upper triangle (v0,v2,v3): u weights v2, v weights v3.
        // v0 -> (0,0)
        assert_eq!(m.resolve(1, 0.0, 0.0, false), Some((7, 0.0, 0.0)));
        // v2 -> (1,1)
        assert_eq!(m.resolve(1, 1.0, 0.0, false), Some((7, 1.0, 1.0)));
        // v3 -> (0,1)
        assert_eq!(m.resolve(1, 0.0, 1.0, false), Some((7, 0.0, 1.0)));
    }

    /// The two halves must agree along the shared v0–v2 diagonal, or the seam
    /// shows as a visible crease straight across every quad.
    #[test]
    fn fan_halves_agree_on_the_shared_diagonal() {
        let m = quad();
        // Midpoint of the v0–v2 diagonal is (0.5, 0.5) in the quad.
        // Lower: v0 and v2 at equal weight -> u = 0.5 on v2's slot.
        assert_eq!(m.resolve(0, 0.0, 0.5, false), Some((7, 0.5, 0.5)));
        // Upper: v2 is the second vertex, so u = 0.5 again.
        assert_eq!(m.resolve(1, 0.5, 0.0, false), Some((7, 0.5, 0.5)));
    }

    /// A mirrored placement bakes a vertex swap into the indices, which swaps
    /// the barycentrics back out again at hit time.
    #[test]
    fn mirrored_placement_unswaps_barycentrics() {
        let m = quad();
        // Same hit as v2 above, but reported with u/v exchanged.
        assert_eq!(m.resolve(0, 1.0, 0.0, true), Some((7, 1.0, 1.0)));
    }

    /// Ptex has no representation for an n-gon, so the lookup must decline
    /// rather than address some arbitrary face.
    #[test]
    fn ngon_slices_have_no_face() {
        let m = FaceMap {
            faces: vec![3],
            slices: vec![FanSlice::Unmappable],
            sub: None,
            corners: None,
            density: Vec::new(),
        };
        assert_eq!(m.resolve(0, 0.25, 0.25, false), None);
    }

    /// A `prim_id` past the table cannot panic: it is reachable from the
    /// integrator, where a panic kills a render thread.
    #[test]
    fn out_of_range_prim_id_declines() {
        assert_eq!(quad().resolve(99, 0.0, 0.0, false), None);
    }

    /// A subdivided quad's upper-left quadrant, fan-triangulated: the child
    /// quad's corners are (0,0.5) (0.5,0.5) (0.5,1) (0,1) in the base face.
    fn sub_quad() -> FaceMap {
        let cell = SubFace::from_corners(&[[0.0, 0.5], [0.5, 0.5], [0.5, 1.0], [0.0, 1.0]])
            .expect("a dyadic cell");
        FaceMap {
            faces: vec![7, 7],
            slices: vec![FanSlice::QuadLower, FanSlice::QuadUpper],
            sub: Some(vec![cell, cell]),
            corners: None,
            density: Vec::new(),
        }
    }

    /// Every dyadic cell at every depth the import can produce, at every
    /// rotation, round-trips through the eight bytes exactly — bit for bit,
    /// since the corners are what the refined channel held.
    #[test]
    fn sub_face_round_trips_every_cell_exactly() {
        for depth in 0..=6u32 {
            let n = 1u32 << depth;
            let s = 1.0 / n as f32;
            for i in 0..n {
                for j in 0..n {
                    let (ou, ov) = (i as f32 * s, j as f32 * s);
                    let base = [[ou, ov], [ou + s, ov], [ou + s, ov + s], [ou, ov + s]];
                    for rot in 0..4 {
                        let corners: [[f32; 2]; 4] = std::array::from_fn(|k| base[(k + rot) % 4]);
                        let cell = SubFace::from_corners(&corners).expect("a dyadic cell");
                        assert_eq!(
                            cell.corners(),
                            corners,
                            "depth {depth} cell {i},{j} rot {rot}"
                        );
                    }
                }
            }
        }
        // Not a cell: a rectangle, and corners off the dyadic grid.
        assert!(
            SubFace::from_corners(&[[0.0, 0.0], [0.5, 0.0], [0.5, 0.25], [0.0, 0.25]]).is_none()
        );
        assert!(SubFace::from_corners(&[[0.1, 0.0], [0.6, 0.0], [0.6, 0.5], [0.1, 0.5]]).is_none());
    }

    /// Explicit UVs resolve each triangle corner to its patch of the *base*
    /// face — the whole point of carrying them for subdivided meshes.
    #[test]
    fn explicit_uvs_map_to_the_sub_rectangle() {
        let m = sub_quad();
        assert_eq!(m.resolve(0, 0.0, 0.0, false), Some((7, 0.0, 0.5)));
        assert_eq!(m.resolve(0, 1.0, 0.0, false), Some((7, 0.5, 0.5)));
        assert_eq!(m.resolve(0, 0.0, 1.0, false), Some((7, 0.5, 1.0)));
        assert_eq!(m.resolve(1, 1.0, 0.0, false), Some((7, 0.5, 1.0)));
        assert_eq!(m.resolve(1, 0.0, 1.0, false), Some((7, 0.0, 1.0)));
    }

    /// Both halves land on the same point along their shared diagonal, same
    /// contract as the fan-slice path.
    #[test]
    fn explicit_uv_halves_agree_on_the_shared_diagonal() {
        let m = sub_quad();
        assert_eq!(m.resolve(0, 0.0, 0.5, false), m.resolve(1, 0.5, 0.0, false));
    }

    /// The mirror swap composes with explicit UVs exactly as with fan
    /// slices: corner UVs are stored in original vertex order, so unswapping
    /// the barycentrics is the whole fix.
    #[test]
    fn explicit_uvs_unswap_mirrored_barycentrics() {
        let m = sub_quad();
        // Same hit as triangle 0's second vertex, reported swapped.
        assert_eq!(m.resolve(0, 0.0, 1.0, true), Some((7, 0.5, 0.5)));
    }

    /// Descendants of a non-quad cage face decline even though the table
    /// carries UVs for their neighbours.
    #[test]
    fn explicit_uv_unmappable_declines() {
        let m = FaceMap {
            faces: vec![u32::MAX],
            slices: vec![FanSlice::Unmappable],
            sub: Some(vec![SubFace::default()]),
            corners: None,
            density: Vec::new(),
        };
        assert_eq!(m.resolve(0, 0.25, 0.25, false), None);
        assert_eq!(m.resolve(9, 0.25, 0.25, false), None);
    }
}
