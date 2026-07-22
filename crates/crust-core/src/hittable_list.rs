use crate::aabb::AABB;
use crate::hittable::{Hit, Hittable};
use crate::ray::Ray;

/// The `HittableList` struct represents a collection of objects that can be intersected by rays.
/// It allows for managing multiple `Hittable` objects and testing for ray intersections with all of them.
///
/// This is the scene-construction container; for rendering, `Renderer::new`
/// converts it into a top-level [`crate::Bvh`] via [`HittableList::into_objects`].
#[derive(Default)]
pub struct HittableList {
    /// A vector of objects implementing the `Hittable` trait.
    objects: Vec<Box<dyn Hittable>>,
}

impl HittableList {
    /// Creates a new, empty `HittableList`.
    ///
    /// # Returns
    /// - A new instance of `HittableList`.
    pub fn new() -> HittableList {
        Default::default()
    }

    /// Adds a `Hittable` object to the list.
    ///
    /// # Parameters
    /// - `object`: A boxed object implementing the `Hittable` trait.
    pub fn add(&mut self, object: Box<dyn Hittable>) {
        self.objects.push(object);
    }
    /// Returns the count of objects in the list.
    ///
    /// # Returns
    /// - The number of objects in the list.
    pub fn count(&self) -> usize {
        self.objects.len()
    }

    /// Consumes the list and returns its objects, e.g. to build an
    /// acceleration structure over them.
    pub fn into_objects(self) -> Vec<Box<dyn Hittable>> {
        self.objects
    }
}

impl Hittable for HittableList {
    /// Finds the closest intersection of the ray with any object in the list
    /// by linear scan.
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<Hit<'_>> {
        let mut closest_so_far = t_max;
        let mut best: Option<Hit> = None;

        for object in &self.objects {
            if let Some(hit) = object.hit(ray, t_min, closest_so_far) {
                closest_so_far = hit.rec.t;
                best = Some(hit);
            }
        }

        best
    }
    fn bounding_box(&self) -> Option<AABB> {
        if self.objects.is_empty() {
            return None;
        }

        let mut temp_box: Option<AABB> = None;

        for object in &self.objects {
            if let Some(bbox) = object.bounding_box() {
                temp_box = Some(match temp_box {
                    Some(existing) => AABB::surrounding_box(existing, bbox),
                    None => bbox,
                });
            } else {
                return None; // fail if any object doesn't have a bounding box
            }
        }

        temp_box
    }
}
