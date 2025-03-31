mod vec3;
pub use vec3::Point3;
pub use vec3::Vec3;
pub use vec3::{
    align_to_normal, cross, dot, random_cosine_direction, random_in_unit_disk,
    random_in_unit_sphere, random_unit_vector, reflect, refract, unit_vector,
};
mod common;
pub use common::Lerp;
pub use common::{balance_heuristic, clamp};
pub use common::{degrees_to_radians, random, random_range, random2};
mod color;
pub use color::Color;
