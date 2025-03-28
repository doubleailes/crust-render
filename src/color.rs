use crate::vec3::Vec3;

// Type alias
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
}
