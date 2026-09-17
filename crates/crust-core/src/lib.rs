mod aabb;
mod buffer;
mod camera;
mod environment;
mod error;
mod filter;
mod guiding;
mod hittable;
mod light;
mod material;
mod medium;
mod ray;
mod rt_world;
mod scene;
/// MaterialX surfaces — `MtlxMaterial`, the lobe pooling onto OpenPBR, and
/// the importer's `load`. The document reader itself is the `crust-mtlx`
/// crate, re-exported below as [`mtlx`].
pub use material::materialx;
mod stats;
mod texture;
mod tracer;
mod volume;
mod world;

/// The path tracer's QMC sampler: OpenQMC's Owen-scrambled Sobol, consumed
/// through its native pass-by-value domain-tree API. Aliased here so a single
/// edit can swap in another OpenQMC sampler (e.g. `SobolBnSampler`).
pub type PathSampler = openqmc::SobolSampler;

/// The standalone MaterialX reader crust-core builds `MtlxMaterial` on.
pub use crust_mtlx as mtlx;
/// The intersection kernel (Embree-shaped scene/geometry API), re-exported
/// so applications can build [`rt::Geometry`] values for [`WorldBuilder`].
pub use crust_rt as rt;

pub use aabb::AABB;
pub use buffer::Buffer;
pub use camera::Camera;
pub use environment::EnvironmentMap;
pub use error::Error;
pub use filter::{FilterSampler, PixelFilter};
pub use glam::{Mat4, Vec3A};
pub use guiding::{GuidingConfig, GuidingField, SampleData};
pub use hittable::HitRecord;
pub use light::{
    AreaLight, DistantLight, DomeLight, Light, LightList, LightSample, LightShape, RectShape,
    SphereShape,
};
pub use material::*;
pub use medium::Medium;
pub use ray::{MASK_ALL, MASK_CAMERA, MASK_INDIRECT, MASK_SHADOW, Ray};
pub use rt_world::{FaceMap, FanSlice, UvMap, World, WorldBuilder, WorldHit};
pub use scene::Scene;
pub use scene::{AssetLoader, NoAssets};
pub use stats::{
    ImageCounters, MemorySample, Phase, PrimitiveCounts, RayStats, RenderStats, SceneCounters,
    peak_memory_bytes,
};
pub use texture::{ColorSpace, PtexRef, PtexTexture, Texture2D, TextureRef};
pub use tracer::{ProgressCallback, RenderSettings, Renderer, SamplingStrategy, ray_color};
pub use volume::{DensityField, PhaseMix, VolumeEvent, VolumeRegion, Volumes};
pub use world::{get_settings, simple_scene};
