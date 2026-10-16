//! Tools over a render: the diagnostic (`crust diagnostic`), the light path
//! inspector (`crust light-paths`) and the `crust-check/2` report.
//!
//! One layer of the engine (`docs/architecture.md` § Crates). Its modules
//! are public so the layers above can reach them, not as a supported API:
//! hosts name `crust_core::…`, which re-exports the supported surface. The
//! layers below are glob re-exported at the root, so a path written
//! `crate::…` resolves the same in every layer.

#![forbid(unsafe_code)]

pub use crust_tracer::*;
pub use crust_usd::*;

/// The `crust-check/2` report `crust check` writes.
pub mod check;
pub mod diagnostic;
/// The light path inspector: the individual light paths of a few pixels
/// (`crust light-paths`).
pub mod inspect;
