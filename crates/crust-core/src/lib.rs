//! The engine as a library: renderer, integrator, materials, lights,
//! volumes, path guiding and USD import.
//!
//! The engine is built as layered crates (`docs/architecture.md` § Crates):
//! `crust-base` (configuration, colour, stats, warnings, rays),
//! `crust-shading` (materials, light path expressions), `crust-world` (the
//! world, lights, camera, volumes, settings, AOV requests), then
//! `crust-tracer` (the integrator) and `crust-usd` (the import) side by side,
//! and `crust-tools` (the diagnostic, the inspector, `crust check`) on top.
//! This crate re-exports their supported surface under the paths it has
//! always had, so a host depends on `crust-core` alone.
//!
//! `forbid(unsafe_code)` here and in every layer but `crust-usd`, which is
//! `deny` for its two audited opt-ins (see that crate).
#![forbid(unsafe_code)]

use crust_base::aabb;
use crust_base::buffer;
pub use crust_base::color;
pub use crust_base::config;
use crust_base::error;
use crust_base::hittable;
use crust_base::lux;
use crust_base::medium;
pub use crust_base::names;
use crust_base::pdf;
/// The opt-in render profile (`--profile`): per-section thread time inside
/// the render, after Guerilla Render's "Render Profile".
pub use crust_base::profile;
use crust_base::ray;
use crust_base::region;
/// The envelope and value rules every JSON report shares.
pub use crust_base::report;
use crust_base::stats;
use crust_base::texture;
/// Coded import warnings: the vocabulary, the collector and the macros.
pub use crust_base::warnings;
pub use crust_shading::lpe;
use crust_shading::material;
/// The `crust-check/2` report `crust check` writes.
pub use crust_tools::check;
pub use crust_tools::diagnostic;
/// The light path inspector: the individual light paths of a few pixels
/// (`crust light-paths`).
pub use crust_tools::inspect;
use crust_tracer::guiding;
use crust_tracer::tracer;
pub use crust_usd::live;
pub use crust_usd::place;
use crust_usd::scene;
use crust_usd::world;
pub use crust_world::aov;
use crust_world::assets;
use crust_world::camera;
/// `crust diff`'s comparison of two decoded images.
pub use crust_world::compare;
use crust_world::control;
use crust_world::environment;
use crust_world::filter;
use crust_world::light;
use crust_world::rt_world;
use crust_world::sampler;
use crust_world::settings;
/// What every EXR records about how it was sampled.
pub use crust_world::stamp;
use crust_world::volume;
/// MaterialX surfaces — `MtlxMaterial`, which evaluates a document's closure
/// tree (`closure`), and the importer's `load`. The document reader itself is the `crust-mtlx`
/// crate, re-exported below as [`mtlx`].
pub use material::materialx;

pub use crust_base::PathSampler;

/// The standalone MaterialX reader crust-core builds `MtlxMaterial` on.
pub use crust_base::mtlx;
/// The intersection kernel (Embree-shaped scene/geometry API), re-exported
/// so applications can build [`rt::Geometry`] values for [`WorldBuilder`].
pub use crust_base::rt;

pub use aabb::AABB;
pub use aov::{Accumulation, AovProduct, AovRequest, AovSource, AovVar, ChannelKind, Precision};
pub use buffer::Buffer;
pub use camera::Camera;
// `config`, the function, comes with the module's `pub use` above.
pub use config::{
    Config, DEFAULT_TEX_MAX_OPEN_FILES, DiskSampling, PtexMipSpace, TriPackets, TubeSampling,
};

pub use crust_base::{Mat3A, Mat4, Vec3A};
pub use environment::EnvironmentMap;
pub use error::Error;
pub use filter::{FilterSampler, PixelFilter};
pub use guiding::{GuidingConfig, GuidingField, SampleData};
pub use hittable::{FaceHit, HitRecord};
pub use light::{
    AffineShape, AreaLight, AreaShape, DistantLight, DomeLight, EVERY_CLASS, FoundAlong, Light,
    LightBounds, LightKind, LightLinks, LightList, LightSample, LightSelection, LightShape,
    LightTree, Receiver, RectShape, ShapeExtent, ShapeHits, SolidAngleSampler, SolidAngleSampling,
    SphereShape, TREE_DEFENSIVE_SHARE, UnitShape, projected_cone_solid_angle,
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
pub use sampler::SamplerKind;
pub use scene::Scene;
/// The implicit-shape tessellators, exposed for `tests/geomutil_oracle.rs`
/// only: not a supported API.
#[doc(hidden)]
pub mod implicit_shapes {
    pub use crate::scene::implicit::{
        Axis, MIN_CAP_AXIAL, MIN_RADIAL, Tessellation, capsule, cone, cuboid, cylinder, plane,
    };
}
pub use assets::{AssetLoader, NoAssets};
pub use control::{RenderControl, RenderOutcome};
pub use crust_base::Luma;
pub use region::PixelRect;
#[cfg(feature = "traversal-stats")]
pub use rt_world::traversal_report;
pub use scene::{
    CameraRecord, LS_FORMAT, LightRecord, ListKind, ListRecord, Listing, MaterialRecord,
    UsdImportOptions, usd_schema_registry,
};
pub use settings::{
    DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE, DEFAULT_INDIRECT_CLAMP, DEFAULT_LIGHT_SAMPLES,
    MAX_LIGHT_SAMPLES, RenderSettings, SamplingStrategy,
};
pub use stats::{
    DisplacementCounters, ImageCounters, MemorySample, Phase, PrimitiveCounts, PtexCacheStats,
    RayStats, RenderStats, SceneCounters, SubdivisionCounters, TextureCacheStats,
    current_memory_bytes, machine_memory_bytes, peak_memory_bytes,
};
pub use texture::{ColorSpace, PtexRef, PtexTexture, ResolvedColorSpace, Texture2D, TextureRef};
pub use tracer::{
    AovFilm, ProgressCallback, Rendered, Renderer, ray_color, ray_color_with_light_samples,
};
pub use volume::{DensityField, PhaseMix, VolumeEvent, VolumeRegion, Volumes};
pub use warnings::{LogPolicy, Warning, WarningCode, WarningKind, WarningScope};
pub use world::{get_settings, simple_scene};

/// The coded-warning macros (`crust_core::warning!` and friends).
pub use crust_base::{__warning_emit, cause_warning, record_warning, warning};
