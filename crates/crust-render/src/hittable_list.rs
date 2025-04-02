use crate::hittable::{HitRecord, Hittable};
use crate::ray::Ray;

/// The `HittableList` struct represents a collection of objects that can be intersected by rays.
/// It allows for managing multiple `Hittable` objects and testing for ray intersections with all of them.
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
}

impl Hittable for HittableList {
    /// Determines if a ray intersects any object in the list.
    ///
    /// # Parameters
    /// - `ray`: The ray to test for intersections.
    /// - `t_min`: The minimum value of the parameter `t` to consider.
    /// - `t_max`: The maximum value of the parameter `t` to consider.
    /// - `rec`: A mutable reference to a `HitRecord` to store intersection details.
    ///
    /// # Returns
    /// - `true` if the ray intersects any object in the list, `false` otherwise.
    ///
    /// This method iterates through all objects in the list and checks for intersections.
    /// If an intersection is found, it updates the `HitRecord` with the closest intersection.
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32, rec: &mut HitRecord) -> bool {
        let mut temp_rec = HitRecord::new();
        let mut hit_anything = false;
        let mut closest_so_far = t_max;

        for object in &self.objects {
            if object.hit(ray, t_min, closest_so_far, &mut temp_rec) {
                hit_anything = true;
                closest_so_far = temp_rec.t;
                *rec = temp_rec.clone();
            }
        }

        hit_anything
    }
}
