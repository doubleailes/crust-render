use glam::Vec3A;
use std::sync::Arc;

/// The `Light` trait defines the behavior of light sources in the ray tracing system.
/// Lights are responsible for illuminating the scene and providing sampling methods.
pub trait Light: Send + Sync {
    /// Samples a point on the light source.
    ///
    /// # Returns
    /// - A `Vec3A` representing a sampled point on the light.
    fn sample(&self) -> Vec3A;

    /// Samples a point on the light source using Correlated Multi-Jittered (CMJ) sampling.
    ///
    /// This method provides a fallback implementation that calls `sample` if not overridden.
    ///
    /// # Parameters
    /// - `u`: The first random parameter for sampling.
    /// - `v`: The second random parameter for sampling.
    ///
    /// # Returns
    /// - A `Vec3A` representing a sampled point on the light.
    #[allow(unused_variables)]
    fn sample_cmj(&self, u: f32, v: f32) -> Vec3A {
        self.sample() // fallback if not overridden
    }

    /// Computes the probability density function (PDF) for a given light sample.
    ///
    /// # Parameters
    /// - `hit_point`: The point on the surface being illuminated.
    /// - `light_point`: The sampled point on the light source.
    ///
    /// # Returns
    /// - A `f32` representing the PDF value for the given sample.
    fn pdf(&self, hit_point: Vec3A, light_point: Vec3A) -> f32;

    /// Returns the color of the light source.
    ///
    /// # Returns
    /// - A `Vec3A` representing the light's color.
    fn color(&self) -> Vec3A;
}

/// The `LightList` struct manages a collection of light sources in the scene.
/// It provides methods to add lights and sample them randomly.
pub struct LightList {
    /// A vector of light sources stored as `Arc<dyn Light>` for shared ownership.
    pub lights: Vec<Arc<dyn Light>>,
}

impl Default for LightList {
    /// Creates a new, empty `LightList` as the default implementation.
    fn default() -> Self {
        Self::new()
    }
}

impl LightList {
    /// Creates a new, empty `LightList`.
    ///
    /// # Returns
    /// - A new instance of `LightList`.
    pub fn new() -> Self {
        Self { lights: Vec::new() }
    }

    /// Adds a light source to the `LightList`.
    ///
    /// # Parameters
    /// - `light`: An `Arc<dyn Light>` representing the light source to add.
    pub fn add(&mut self, light: Arc<dyn Light>) {
        self.lights.push(light);
    }

    /// Randomly samples a light source from the `LightList`.
    ///
    /// # Returns
    /// - `Some(&Arc<dyn Light>)` if the list is not empty.
    /// - `None` if the list is empty.
    pub fn sample(&self) -> Option<&Arc<dyn Light>> {
        if self.lights.is_empty() {
            None
        } else {
            let i = (utils::random() * self.lights.len() as f32) as usize;
            self.lights.get(i)
        }
    }
    /// Returns the number of lights in the `LightList`.
    ///
    /// # Returns
    /// - The number of lights in the list.
    ///
    /// This method is useful for iterating over the lights or checking if the list is empty.
    pub fn count(&self) -> usize {
        self.lights.len()
    }
}
