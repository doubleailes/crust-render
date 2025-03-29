use crate::color::Color;
use crate::vec3::{Point3, Vec3};

pub trait Light: Send + Sync {
    fn sample(&self) -> Point3;
    fn pdf(&self, hit_point: Point3, light_point: Point3) -> f32;
    fn color(&self) -> Color;
}
