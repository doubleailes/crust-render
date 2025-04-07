use glam::Vec3A;

/// The `Ray` struct represents a ray in 3D space, defined by an origin and a direction.
/// Rays are used in ray tracing to determine intersections with objects in the scene.
#[derive(Default)]
pub struct Ray {
    /// The origin point of the ray.
    orig: Vec3A,
    /// The direction vector of the ray.
    dir: Vec3A,
}

impl Ray {
    /// Creates a new `Ray` with the specified origin and direction.
    ///
    /// # Parameters
    /// - `origin`: The starting point of the ray.
    /// - `direction`: The direction vector of the ray.
    ///
    /// # Returns
    /// - A new instance of `Ray`.
    pub fn new(origin: Vec3A, direction: Vec3A) -> Ray {
        Ray {
            orig: origin,
            dir: direction,
        }
    }

    /// Returns the origin of the ray.
    ///
    /// # Returns
    /// - A `Vec3A` representing the origin of the ray.
    pub fn origin(&self) -> Vec3A {
        self.orig
    }

    /// Returns the direction of the ray.
    ///
    /// # Returns
    /// - A `Vec3A` representing the direction of the ray.
    pub fn direction(&self) -> Vec3A {
        self.dir
    }

    /// Computes the point along the ray at a given parameter `t`.
    ///
    /// # Parameters
    /// - `t`: The parameter along the ray's direction.
    ///
    /// # Returns
    /// - A `Vec3A` representing the point at parameter `t`.
    pub fn at(&self, t: f32) -> Vec3A {
        self.orig + t * self.dir
    }
}
