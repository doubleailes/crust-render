use crate::color::Color;
use crate::vec3::Point3;
use std::sync::Arc;

pub trait Light: Send + Sync {
    fn sample(&self) -> Point3;
    fn pdf(&self, hit_point: Point3, light_point: Point3) -> f32;
    fn color(&self) -> Color;
}

pub struct LightList {
    pub lights: Vec<Arc<dyn Light>>,
}

impl Default for LightList {
    fn default() -> Self {
        Self::new()
    }
}

impl LightList {
    pub fn new() -> Self {
        Self { lights: Vec::new() }
    }

    pub fn add(&mut self, light: Arc<dyn Light>) {
        self.lights.push(light);
    }

    pub fn sample(&self) -> Option<&Arc<dyn Light>> {
        if self.lights.is_empty() {
            None
        } else {
            let i = (crate::common::random() * self.lights.len() as f32) as usize;
            self.lights.get(i)
        }
    }
}
