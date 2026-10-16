//! The engine's foundation layer, which every other engine crate builds on:
//! configuration (the only place the environment is read), names, coded
//! warnings, the JSON reports' envelope, render statistics and the profile,
//! colour management, texture references, UsdLux units, the error type,
//! pixel regions and buffers — and rays: [`Ray`], [`HitRecord`], carried
//! [`Medium`]s.
//!
//! The layers and what may import what are in `docs/architecture.md`
//! § Layering.
#![forbid(unsafe_code)]

// Modules marked `doc(hidden)` are public for the crates above, not part of
// `crust_core`'s documented surface.
#[doc(hidden)]
pub mod aabb;
#[doc(hidden)]
pub mod buffer;
pub mod color;
pub mod config;
#[doc(hidden)]
pub mod distribution;
#[doc(hidden)]
pub mod error;
#[doc(hidden)]
pub mod hittable;
#[doc(hidden)]
pub mod lux;
#[doc(hidden)]
pub mod medium;
pub mod names;
#[doc(hidden)]
pub mod pdf;
/// The opt-in render profile (`--profile`): per-section thread time inside
/// the render, after Guerilla Render's "Render Profile".
pub mod profile;
#[doc(hidden)]
pub mod ray;
#[doc(hidden)]
pub mod region;
/// The envelope and value rules every JSON report shares.
pub mod report;
#[doc(hidden)]
pub mod stats;
#[doc(hidden)]
pub mod texture;
/// Coded import warnings: the vocabulary, the collector and the macros.
pub mod warnings;

/// The default QMC sampler — OpenQMC's Owen-scrambled Sobol, consumed
/// through its native pass-by-value domain-tree API — by name: what the
/// `learned` light-selection pre-pass always draws from, and what tests
/// build their domains with. The integrator itself is generic over
/// `openqmc::SamplerImpl`, and a render picks its sampler through
/// `SamplerKind` (`crust:sampler` / `--sampler`).
pub type PathSampler = openqmc::SobolSampler;

/// The intersection kernel (Embree-shaped scene/geometry API), re-exported
/// so applications can build `rt::Geometry` values for `WorldBuilder`.
pub use crust_rt as rt;

pub use aabb::AABB;
pub use buffer::Buffer;
pub use config::{
    Config, DEFAULT_TEX_MAX_OPEN_FILES, DiskSampling, PtexMipSpace, TriPackets, TubeSampling,
    config,
};
pub use error::Error;
pub use glam::{Mat3A, Mat4, Vec3A};
pub use hittable::{FaceHit, HitRecord};
pub use lux::{
    IesProfile, IesShaping, LightTexture, RectTexture, Shaping, blackbody_in, blackbody_rgb,
    distant_illuminance, distant_size_factor,
};
pub use medium::Medium;
pub use pdf::{InvPdfArea, PdfSolidAngle};
pub use ray::{
    MASK_ALL, MASK_CAMERA, MASK_INDIRECT, MASK_SHADOW, Ray, RayCone, RayMask, TRACE_T_MIN,
};
pub use region::PixelRect;
pub use stats::{
    DisplacementCounters, ImageCounters, MemorySample, Phase, PrimitiveCounts, PtexCacheStats,
    RayStats, RenderStats, SceneCounters, SubdivisionCounters, TextureCacheStats,
    current_memory_bytes, machine_memory_bytes, peak_memory_bytes,
};
pub use texture::{ColorSpace, PtexRef, PtexTexture, ResolvedColorSpace, Texture2D, TextureRef};
pub use utils::Luma;
pub use warnings::{LogPolicy, Warning, WarningCode, WarningKind, WarningScope};
