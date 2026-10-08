//! The engine as a library: renderer, integrator, materials, lights,
//! volumes, path guiding and USD import.
//!
//! `deny(unsafe_code)` rather than `forbid`, for exactly one reason: the
//! subdivision allocation probe in `scene/subdiv.rs` installs a counting
//! `GlobalAlloc`, and implementing that trait is inherently unsafe. `deny`
//! lets that one test module opt out explicitly and visibly; `forbid` could
//! not be overridden at all, and dropping the lint entirely would leave the
//! claim unchecked everywhere else. `crust-jit` is the only other crate that
//! is `deny` rather than `forbid` (it calls generated code); every other
//! crate in the workspace is `forbid`.
#![deny(unsafe_code)]

mod aabb;
pub mod aov;
mod buffer;
mod camera;
pub mod color;
pub mod config;
pub mod diagnostic;
mod environment;
mod error;
mod filter;
mod guiding;
mod hittable;
mod light;
mod light_cache;
pub mod lpe;
mod lux;
mod material;
mod medium;
pub mod names;
mod pdf;
/// The opt-in render profile (`--profile`): per-section thread time inside
/// the render, after Guerilla Render's "Render Profile".
pub mod profile;
mod ray;
mod rt_world;
mod scene;
/// MaterialX surfaces — `MtlxMaterial`, which evaluates a document's closure
/// tree (`closure`), and the importer's `load`. The document reader itself is the `crust-mtlx`
/// crate, re-exported below as [`mtlx`].
pub use material::materialx;
mod stats;
mod subsurface;
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
pub use aov::{
    Accumulation, AovFilm, AovProduct, AovRequest, AovSource, AovVar, ChannelKind, Precision,
};
pub use buffer::Buffer;
pub use camera::Camera;
pub use config::{Config, DEFAULT_TEX_MAX_OPEN_FILES, PtexMipSpace, TriPackets, config};

/// What every kernel scene commits with — the `CRUST_TRI_PACKETS` switch,
/// read once.
pub(crate) fn commit_options() -> crust_rt::CommitOptions {
    crust_rt::CommitOptions {
        layout: config().tri_packets.into(),
    }
}
pub use environment::EnvironmentMap;
pub use error::Error;
pub use filter::{FilterSampler, PixelFilter};
pub use glam::{Mat3A, Mat4, Vec3A};
pub use guiding::{GuidingConfig, GuidingField, SampleData};
pub use hittable::{FaceHit, HitRecord};
pub use light::{
    AffineShape, AreaLight, AreaShape, DistantLight, DomeLight, EVERY_CLASS, FoundAlong, Light,
    LightKind, LightLinks, LightList, LightSample, LightSelection, LightShape, RectShape,
    ShapeHits, SolidAngleSampler, SolidAngleSampling, SphereShape, UnitShape,
    projected_cone_solid_angle,
};
pub use lux::{
    IesProfile, IesShaping, LightTexture, RectTexture, Shaping, blackbody_in, blackbody_rgb,
    distant_illuminance, distant_size_factor,
};
pub use material::*;
pub use medium::Medium;
pub use pdf::{InvPdfArea, PdfSolidAngle};
pub use ray::{
    MASK_ALL, MASK_CAMERA, MASK_INDIRECT, MASK_SHADOW, Ray, RayCone, RayMask, TRACE_T_MIN,
};
pub use rt_world::{FaceMap, FanSlice, SubFace, UvMap, World, WorldBuilder, WorldHit, tangent_of};
pub use scene::Scene;
pub use scene::{AssetLoader, ListKind, NoAssets, UsdImportOptions};
#[cfg(feature = "traversal-stats")]
pub use stats::traversal_report;
pub use stats::{
    DisplacementCounters, ImageCounters, MemorySample, Phase, PrimitiveCounts, PtexCacheStats,
    RayStats, RenderStats, SceneCounters, SubdivisionCounters, TextureCacheStats,
    current_memory_bytes, machine_memory_bytes, peak_memory_bytes,
};
pub use texture::{ColorSpace, PtexRef, PtexTexture, ResolvedColorSpace, Texture2D, TextureRef};
pub use tracer::{
    DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE, DEFAULT_INDIRECT_CLAMP, DEFAULT_LIGHT_SAMPLES,
    MAX_LIGHT_SAMPLES, PixelRect, ProgressCallback, RenderSettings, Renderer, SamplingStrategy,
    ray_color, ray_color_with_light_samples,
};
pub use utils::Luma;
pub use volume::{DensityField, PhaseMix, VolumeEvent, VolumeRegion, Volumes};
pub use world::{get_settings, simple_scene};
