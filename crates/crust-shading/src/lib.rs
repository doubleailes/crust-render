//! The materials: the `Material` trait and `ShadingPoint`, OpenPBR, the
//! MaterialX closure tree, `UsdPreviewSurface`, emission, displacement, the
//! lobe vocabulary, and the light path expressions (`lpe`) it is matched by.
//!
//! One layer of the engine (`docs/architecture.md` § Crates). Its modules
//! are public so the layers above can reach them, not as a supported API:
//! hosts name `crust_core::…`, which re-exports the supported surface. The
//! layers below are glob re-exported at the root, so a path written
//! `crate::…` resolves the same in every layer.

#![forbid(unsafe_code)]

pub use crust_base::*;

pub mod lpe;
pub mod material;

/// MaterialX surfaces — `MtlxMaterial`, which evaluates a document's closure
/// tree (`closure`), and the importer's `load`. The document reader itself is
/// the `crust-mtlx` crate, re-exported as [`mtlx`].
pub use material::materialx;

pub use material::*;
