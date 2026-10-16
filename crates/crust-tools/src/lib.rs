//! What runs renders to answer a question about them: `crust diagnostic`,
//! the light path inspector, `crust diff`'s comparison, the sampling stamp
//! every EXR carries and `crust check`'s report.
#![forbid(unsafe_code)]

// The crates below, glob-imported at the root so `crate::color`,
// `crate::LightList` and the like keep naming what they named when this was
// all one crate (crust-core): the modules moved here unchanged.
#[allow(unused_imports)]
use crust_base::*;
#[allow(unused_imports)]
use crust_integrator::*;
#[allow(unused_imports)]
use crust_shading::*;
#[allow(unused_imports)]
use crust_usd::*;
#[allow(unused_imports)]
use crust_world::*;

/// The `crust-check/2` report `crust check` writes.
pub mod check;
/// `crust diff`'s comparison of two decoded images.
pub mod compare;
pub mod diagnostic;
/// The light path inspector: the individual light paths of a few pixels
/// (`crust light-paths`).
pub mod inspect;
/// What every EXR records about how it was sampled.
pub mod stamp;
