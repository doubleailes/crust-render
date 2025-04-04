use std::sync::Arc;
use utils::Color;
use utils::Point3;

/// The `Light` trait defines the behavior of light sources in the ray tracing system.
/// Lights are responsible for illuminating the scene and providing sampling methods.
pub trait Light: Send + Sync {
    /// Samples a point on the light source.
    ///
    /// # Returns
    /// - A `Point3` representing a sampled point on the light.
    fn sample(&self) -> Point3;

    /// Samples a point on the light source using Correlated Multi-Jittered (CMJ) sampling.
    ///
    /// This method provides a fallback implementation that calls `sample` if not overridden.
    ///
    /// # Parameters
    /// - `u`: The first random parameter for sampling.
    /// - `v`: The second random parameter for sampling.
    ///
    /// # Returns
    /// - A `Point3` representing a sampled point on the light.
    #[allow(unused_variables)]
    fn sample_cmj(&self, u: f32, v: f32) -> Point3 {
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
    fn pdf(&self, hit_point: Point3, light_point: Point3) -> f32;

    /// Returns the color of the light source.
    ///
    /// # Returns
    /// - A `Color` representing the light's color.
    fn color(&self) -> Color;
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
}
