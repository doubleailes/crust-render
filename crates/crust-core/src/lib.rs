mod aabb;
mod buffer;
mod camera;
mod checkpoint;
mod error;
mod film;
mod guiding;
mod hittable;
mod light;
mod material;
mod medium;
mod ray;
mod rt_world;
mod scene;
mod scheduler;
mod tracer;
mod volume;
mod world;

/// The path tracer's QMC sampler: OpenQMC's Owen-scrambled Sobol, consumed
/// through its native pass-by-value domain-tree API. Aliased here so a single
/// edit can swap in another OpenQMC sampler (e.g. `SobolBnSampler`).
pub type PathSampler = openqmc::SobolSampler;

/// The intersection kernel (Embree-shaped scene/geometry API), re-exported
/// so applications can build [`rt::Geometry`] values for [`WorldBuilder`].
pub use crust_rt as rt;

pub use aabb::AABB;
pub use buffer::Buffer;
pub use camera::Camera;
pub use checkpoint::{CHECKPOINT_VERSION, CheckpointCallback, CheckpointState};
pub use error::Error;
pub use glam::{Mat4, Vec3A};
pub use guiding::{GuidingConfig, GuidingField, SampleData};
pub use hittable::HitRecord;
pub use light::{AreaLight, Light, LightList, LightShape, RectShape, SphereShape};
pub use material::*;
pub use medium::Medium;
pub use ray::{MASK_ALL, MASK_CAMERA, MASK_INDIRECT, MASK_SHADOW, Ray};
pub use rt_world::{World, WorldBuilder, WorldHit};
pub use scene::Scene;
pub use scheduler::TileOrder;
pub use tracer::{
    ProgressCallback, RenderOptions, RenderSettings, Renderer, SamplingStrategy, ray_color,
};
pub use volume::{DensityField, PhaseMix, VolumeEvent, VolumeRegion, Volumes};
pub use world::{get_settings, simple_scene};
