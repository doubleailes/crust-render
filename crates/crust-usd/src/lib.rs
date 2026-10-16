//! USD import: the streaming stage traversal and its schema mapping,
//! subdivision, displacement, instancing, render settings and products, the
//! procedural fallback scene, and following a stage live ([`live`]).
//!
//! `deny(unsafe_code)` rather than `forbid`, for exactly two reasons: the
//! subdivision allocation probe in `scene/subdiv/tests.rs` installs a
//! counting `GlobalAlloc`, and implementing that trait is inherently unsafe;
//! and the import, behind the `mmap` feature, opts into openusd's
//! memory-mapped file reads (`usd_import::stage_builder`), whose opt-in is an
//! `unsafe fn` because the caller promises no mapped file changes while it is
//! read. `deny` lets each opt out explicitly and visibly; `forbid` could not
//! be overridden at all, and dropping the lint entirely would leave the claim
//! unchecked everywhere else. `crust-jit` is the only other crate that is
//! `deny` rather than `forbid` (it calls generated code); every other crate
//! in the workspace is `forbid`.
#![deny(unsafe_code)]

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
use crust_world::*;

pub mod live;
pub mod place;
#[doc(hidden)]
pub mod scene;
#[doc(hidden)]
pub mod world;

pub use scene::Scene;
/// The implicit-shape tessellators, exposed for `tests/geomutil_oracle.rs`
/// only: not a supported API.
#[doc(hidden)]
pub mod implicit_shapes {
    pub use crate::scene::implicit::{
        Axis, MIN_CAP_AXIAL, MIN_RADIAL, Tessellation, capsule, cone, cuboid, cylinder, plane,
    };
}
pub use scene::{
    AssetLoader, CameraRecord, LS_FORMAT, LightRecord, ListKind, ListRecord, Listing,
    MaterialRecord, NoAssets, UsdImportOptions, usd_schema_registry,
};
pub use world::{get_settings, simple_scene};
