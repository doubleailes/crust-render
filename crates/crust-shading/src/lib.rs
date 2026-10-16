//! Materials: the [`Material`] trait and [`ShadingPoint`], OpenPBR,
//! MaterialX closure trees ([`MtlxMaterial`]) and `UsdPreviewSurface` — and
//! light path expressions ([`lpe`]), which match the lobe vocabulary the
//! materials split their values into (`material/lobe.rs`).
#![forbid(unsafe_code)]

// The crates below, glob-imported at the root so `crate::color`,
// `crate::LightList` and the like keep naming what they named when this was
// all one crate (crust-core): the modules moved here unchanged.
#[allow(unused_imports)]
use crust_base::*;

pub mod lpe;
#[doc(hidden)]
pub mod material;

/// MaterialX surfaces — `MtlxMaterial`, which evaluates a document's closure
/// tree (`closure`), and the importer's `load`. The document reader itself is
/// the `crust-mtlx` crate, re-exported below as [`mtlx`].
pub use material::materialx;

/// The standalone MaterialX reader `MtlxMaterial` is built on.
pub use crust_mtlx as mtlx;

pub use material::*;
