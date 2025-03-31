// Type alias
use crate::Vec3;
pub type Color = Vec3;

impl Color {
    pub fn r(&self) -> f32 {
        self.x()
    }
    pub fn g(&self) -> f32 {
        self.y()
    }
    pub fn b(&self) -> f32 {
        self.z()
    }
    pub fn rgb(&self) -> (f32, f32, f32) {
        (self.r(), self.g(), self.b())
    }
    pub fn zero() -> Self {
        Self::new(0.0, 0.0, 0.0)
    }
    pub fn max_component(&self) -> f32 {
        self.x().max(self.y()).max(self.z())
    }
}
