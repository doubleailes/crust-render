//! The integrator: [`Renderer`] and the path tracer, its AOVs and light path
//! routing, pixel filters, QMC samplers, path guiding, volumes, subsurface
//! random walks and the [`Camera`].
#![forbid(unsafe_code)]

// The crates below, glob-imported at the root so `crate::color`,
// `crate::LightList` and the like keep naming what they named when this was
// all one crate (crust-core): the modules moved here unchanged.
#[allow(unused_imports)]
use crust_base::*;
#[allow(unused_imports)]
use crust_shading::*;
#[allow(unused_imports)]
use crust_world::*;

pub mod aov;
#[doc(hidden)]
pub mod camera;
#[doc(hidden)]
pub mod filter;
#[doc(hidden)]
pub mod guiding;
#[doc(hidden)]
pub mod sampler;
#[doc(hidden)]
pub mod subsurface;
#[doc(hidden)]
pub mod tracer;
#[doc(hidden)]
pub mod volume;

pub use aov::{
    Accumulation, AovFilm, AovProduct, AovRequest, AovSource, AovVar, ChannelKind, Precision,
};
pub use camera::Camera;
pub use filter::{FilterSampler, PixelFilter};
pub use guiding::{GuidingConfig, GuidingField, SampleData};
pub use sampler::SamplerKind;
pub use tracer::{
    DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE, DEFAULT_INDIRECT_CLAMP, DEFAULT_LIGHT_SAMPLES,
    MAX_LIGHT_SAMPLES, ProgressCallback, RenderControl, RenderOutcome, RenderSettings, Rendered,
    Renderer, SamplingStrategy, ray_color, ray_color_with_light_samples,
};
pub use volume::{DensityField, PhaseMix, VolumeEvent, VolumeRegion, Volumes};
