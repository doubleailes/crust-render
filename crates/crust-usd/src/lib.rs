//! The USD import: the streaming stage traversal, schema mapping,
//! subdivision, displacement, instancing, render settings and products, the
//! in-place rebuilds an MCP edit applies (`live`), and placement (`place`).
//!
//! `deny(unsafe_code)` rather than `forbid`, for exactly two reasons: the
//! subdivision allocation probe in `scene/subdiv/tests.rs` installs a
//! counting `GlobalAlloc`, and implementing that trait is inherently unsafe;
//! and the import, behind the `mmap` feature, opts into openusd's
//! memory-mapped file reads (`usd_import::stage_builder`), whose opt-in is an
//! `unsafe fn` because the caller promises no mapped file changes while it is
//! read. `deny` lets each opt out explicitly and visibly; `forbid` could not
//! be overridden at all. Every other engine crate is `forbid`.
//!
//! One layer of the engine (`docs/architecture.md` § Crates). Its modules
//! are public so the layers above can reach them, not as a supported API:
//! hosts name `crust_core::…`, which re-exports the supported surface. The
//! layers below are glob re-exported at the root, so a path written
//! `crate::…` resolves the same in every layer.
#![deny(unsafe_code)]

pub use crust_world::*;

pub mod live;
pub mod place;
pub mod scene;
pub mod world;

pub use scene::{
    CameraRecord, LS_FORMAT, LightRecord, ListKind, ListRecord, Listing, MaterialRecord, Scene,
    UsdImportOptions, usd_schema_registry,
};
pub use world::{get_settings, simple_scene};
