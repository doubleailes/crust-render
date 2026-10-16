//! The engine's foundation: configuration (`Config`, the one place the
//! environment is read), colour management, stats and the render profile,
//! coded import warnings and their macros, the JSON reports' envelope, the
//! texture traits, rays and media.
//!
//! One layer of the engine (`docs/architecture.md` § Crates). Its modules
//! are public so the layers above can reach them, not as a supported API:
//! hosts name `crust_core::…`, which re-exports the supported surface. The
//! layers below are glob re-exported at the root, so a path written
//! `crate::…` resolves the same in every layer.

#![forbid(unsafe_code)]

pub mod aabb;
pub mod buffer;
pub mod color;
pub mod config;
pub mod distribution;
pub mod error;
pub mod hittable;
pub mod lux;
pub mod medium;
pub mod names;
pub mod pdf;
/// The opt-in render profile (`--profile`): per-section thread time inside
/// the render, after Guerilla Render's "Render Profile".
pub mod profile;
pub mod ray;
pub mod region;
/// The envelope and value rules every JSON report shares.
pub mod report;
pub mod stats;
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

/// The standalone MaterialX reader the materials are built on.
pub use crust_mtlx as mtlx;
/// The intersection kernel (Embree-shaped scene/geometry API), re-exported
/// so applications can build [`rt::Geometry`] values for the world.
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
