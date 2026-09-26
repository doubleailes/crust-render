//! The engine as a library: renderer, integrator, materials, lights,
//! volumes, path guiding and USD import.
//!
//! `deny(unsafe_code)` rather than `forbid`, for exactly one reason: the
//! subdivision allocation probe in `scene/subdiv.rs` installs a counting
//! `GlobalAlloc`, and implementing that trait is inherently unsafe. `deny`
//! lets that one test module opt out explicitly and visibly; `forbid` could
//! not be overridden at all, and dropping the lint entirely would leave the
//! claim unchecked everywhere else. Every other crate in the workspace is
//! `forbid`.
#![deny(unsafe_code)]

mod aabb;
mod buffer;
mod camera;
mod environment;
mod error;
mod filter;
mod guiding;
mod hittable;
mod light;
mod lux;
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
    AffineShape, AreaLight, DistantLight, DomeLight, Light, LightList, LightSample, LightSelection,
    LightShape, RectShape, SphereShape, UnitShape, projected_cone_solid_angle,
};
pub use lux::{
    IesProfile, IesShaping, LightTexture, RectTexture, Shaping, blackbody_rgb, distant_illuminance,
    distant_size_factor,
};
pub use material::*;
pub use medium::Medium;
pub use ray::{MASK_ALL, MASK_CAMERA, MASK_INDIRECT, MASK_SHADOW, Ray, RayCone};
pub use rt_world::{FaceMap, FanSlice, UvMap, World, WorldBuilder, WorldHit};
pub use scene::Scene;
pub use scene::{AssetLoader, NoAssets, UsdImportOptions};
pub use stats::{
    ImageCounters, MemorySample, Phase, PrimitiveCounts, PtexCacheStats, RayStats, RenderStats,
    SceneCounters, TextureCacheStats, peak_memory_bytes,
};
pub use texture::{ColorSpace, PtexRef, PtexTexture, Texture2D, TextureRef};
pub use tracer::{
    DEFAULT_INDIRECT_CLAMP, ProgressCallback, RenderSettings, Renderer, SamplingStrategy, ray_color,
};
pub use volume::{DensityField, PhaseMix, VolumeEvent, VolumeRegion, Volumes};
pub use world::{get_settings, simple_scene};
