//! The integrator: the `Renderer` (passes, tiles, the adaptive schedule),
//! the path tracer with NEE and MIS, the AOV film, path guiding, the
//! subsurface random walk, and the learned and neural light selections'
//! training.
//!
//! One layer of the engine (`docs/architecture.md` § Crates). Its modules
//! are public so the layers above can reach them, not as a supported API:
//! hosts name `crust_core::…`, which re-exports the supported surface. The
//! layers below are glob re-exported at the root, so a path written
//! `crate::…` resolves the same in every layer.

#![forbid(unsafe_code)]

pub use crust_world::*;

pub mod guiding;
pub mod subsurface;
pub mod tracer;

pub use guiding::{GuidingConfig, GuidingField, SampleData};
pub use tracer::{
    AovFilm, ProgressCallback, Rendered, Renderer, ray_color, ray_color_with_light_samples,
};
