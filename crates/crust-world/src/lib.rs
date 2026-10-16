//! What a render is made of: the committed `World` (the kernel scene and
//! its material and side tables), the lights and their selection, the
//! environment, the camera, volumes, `RenderSettings` and `RenderControl`,
//! the AOV request vocabulary, the `AssetLoader` seam, the sampling stamp
//! every EXR carries, and `crust diff`'s comparison. Everything the import
//! fills and the integrator reads, so the two build side by side.
//!
//! One layer of the engine (`docs/architecture.md` § Crates). Its modules
//! are public so the layers above can reach them, not as a supported API:
//! hosts name `crust_core::…`, which re-exports the supported surface. The
//! layers below are glob re-exported at the root, so a path written
//! `crate::…` resolves the same in every layer.

#![forbid(unsafe_code)]

pub use crust_shading::*;

pub mod aov;
pub mod assets;
pub mod camera;
/// `crust diff`'s comparison of two decoded images.
pub mod compare;
pub mod control;
pub mod environment;
pub mod filter;
pub mod light;
pub mod rt_world;
pub mod sampler;
pub mod settings;
/// What every EXR records about how it was sampled.
pub mod stamp;
pub mod volume;

/// What every kernel scene commits with — the `CRUST_TRI_PACKETS` switch,
/// read once.
pub fn commit_options() -> crust_rt::CommitOptions {
    crust_rt::CommitOptions {
        layout: config().tri_packets.into(),
    }
}

pub use aov::{Accumulation, AovProduct, AovRequest, AovSource, AovVar, ChannelKind, Precision};
pub use assets::{AssetLoader, NoAssets};
pub use camera::Camera;
pub use control::{RenderControl, RenderOutcome};
pub use environment::EnvironmentMap;
pub use filter::{FilterSampler, PixelFilter};
pub use light::{
    AffineShape, AreaLight, AreaShape, DistantLight, DomeLight, EVERY_CLASS, FoundAlong, Light,
    LightBounds, LightKind, LightLinks, LightList, LightSample, LightSelection, LightShape,
    LightTree, Receiver, RectShape, ShapeExtent, ShapeHits, SolidAngleSampler, SolidAngleSampling,
    SphereShape, TREE_DEFENSIVE_SHARE, UnitShape, projected_cone_solid_angle,
};
#[cfg(feature = "traversal-stats")]
pub use rt_world::traversal_report;
pub use rt_world::{FaceMap, FanSlice, SubFace, UvMap, World, WorldBuilder, WorldHit, tangent_of};
pub use sampler::SamplerKind;
pub use settings::{
    DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE, DEFAULT_INDIRECT_CLAMP, DEFAULT_LIGHT_SAMPLES,
    MAX_LIGHT_SAMPLES, RenderSettings, SamplingStrategy,
};
pub use volume::{DensityField, PhaseMix, VolumeEvent, VolumeRegion, Volumes};
