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

/// Scene-construction container: kernel geometries plus the material
/// table indexed by `geom_id`. Commit into a [`World`] to render.
#[derive(Default)]
pub struct WorldBuilder {
    rt: SceneBuilder,
    materials: Vec<Arc<dyn Material>>,
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
        debug_assert_eq!(id as usize + 1, self.materials.len());
        id
    }

    /// Number of geometries attached so far.
    pub fn count(&self) -> usize {
        self.materials.len()
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
    }

    /// Builds the acceleration structure (parallel, deterministic).
    pub fn commit(self) -> World {
        World {
            scene: self.rt.commit(),
            materials: self.materials,
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
}

impl World {
    /// Closest hit in `(t_min, t_max)` with its material resolved.
    pub fn intersect(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<WorldHit<'_>> {
        let h = self.scene.intersect(ray.rt(), t_min, t_max)?;
        Some(WorldHit {
            rec: HitRecord {
                p: ray.at(h.t),
                normal: h.normal,
                t: h.t,
                front_face: h.front_face,
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
