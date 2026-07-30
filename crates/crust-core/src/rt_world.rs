//! The bridge between the `crust-rt` kernel and the renderer: a
//! [`WorldBuilder`] pairs every attached kernel [`Geometry`] with its
//! [`Material`], and the committed [`World`] resolves kernel hits
//! (`geom_id`) back to materials — the ID-based split that keeps shading
//! state out of the intersection kernel.

use crate::hittable::HitRecord;
use crate::material::Material;
use crate::ray::Ray;
use crust_rt::{AABB, Geometry, MASK_ALL, SceneBuilder};
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
}

/// A geometry's face table, plus whether its placement mirrored the winding.
struct FaceRef {
    map: Arc<FaceMap>,
    swapped: bool,
}

/// Scene-construction container: kernel geometries plus the material
/// table indexed by `geom_id`. Commit into a [`World`] to render.
#[derive(Default)]
pub struct WorldBuilder {
    rt: SceneBuilder,
    materials: Vec<Arc<dyn Material>>,
    /// Sparse, indexed by `geom_id`: only geometries whose material actually
    /// samples a per-face texture carry a table, so a scene with no Ptex pays
    /// one `None` per geometry and nothing more.
    faces: Vec<Option<FaceRef>>,
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
        mask: u32,
    ) -> u32 {
        let id = self.rt.attach_masked(geometry, mask);
        self.materials.push(material);
        self.faces.push(None);
        debug_assert_eq!(id as usize + 1, self.materials.len());
        id
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
        self.faces[id as usize] = Some(FaceRef { map, swapped });
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
    pub fn reserve_slot(&mut self, material: Arc<dyn Material>, mask: u32) -> u32 {
        self.attach_masked(SceneBuilder::empty_geometry(), material, mask)
    }

    /// Fills in a slot from [`WorldBuilder::reserve_slot`].
    ///
    /// # Panics
    /// If `id` was never reserved.
    pub fn set_geometry(&mut self, id: u32, geometry: Geometry) {
        self.rt.set_geometry(id, geometry);
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
    pub fn commit(self) -> World {
        World {
            scene: self.rt.commit(),
            materials: self.materials,
            faces: self.faces,
        }
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
    faces: Vec<Option<FaceRef>>,
}

impl World {
    /// Closest hit in `(t_min, t_max)` with its material resolved.
    pub fn intersect(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<WorldHit<'_>> {
        let h = self.scene.intersect(ray.rt(), t_min, t_max)?;
        // The kernel reports triangle barycentrics; a per-face-textured mesh
        // needs the polygon they belong to. Geometries without a table (the
        // overwhelming majority) skip straight past this.
        let (face_id, face_uv) = match &self.faces[h.geom_id as usize] {
            Some(f) => match f.map.resolve(h.prim_id, h.u, h.v, f.swapped) {
                Some((face, u, v)) => (face, (u, v)),
                None => (HitRecord::NO_FACE, (0.0, 0.0)),
            },
            None => (HitRecord::NO_FACE, (0.0, 0.0)),
        };
        Some(WorldHit {
            rec: HitRecord {
                p: ray.at(h.t),
                normal: h.normal,
                t: h.t,
                front_face: h.front_face,
                face_id,
                face_uv,
            },
            mat: self.materials[h.geom_id as usize].as_ref(),
            geom_id: h.geom_id,
            prim_id: h.prim_id,
        })
    }

    /// Early-exit occlusion query — the shadow-ray fast path.
    pub fn occluded(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        self.scene.occluded(ray.rt(), t_min, t_max)
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
        };
        assert_eq!(m.resolve(0, 0.25, 0.25, false), None);
    }

    /// A `prim_id` past the table cannot panic: it is reachable from the
    /// integrator, where a panic kills a render thread.
    #[test]
    fn out_of_range_prim_id_declines() {
        assert_eq!(quad().resolve(99, 0.0, 0.0, false), None);
    }
}
