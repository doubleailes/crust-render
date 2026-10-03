// The trait lives in `material/material.rs` beside its implementations;
// renaming the file to satisfy the lint would only churn the history.
#[allow(clippy::module_inception)]
mod material;
pub use material::{Material, Resolution, ScatterSample, ShadingPoint};
mod emissive;
pub use emissive::Emissive;
pub(crate) mod brdf;
pub mod closure;
mod openpbr;
mod pattern;
pub use openpbr::{InteriorCache, OpenPBR, ResolvedOpenPBR};
pub mod materialx;
pub use materialx::MtlxMaterial;
pub mod displacement;
pub mod preview_surface;
pub use displacement::{DispRemap, Displacement, DisplacementValue, VertexCtx, VertexField};
pub use preview_surface::PreviewSurface;
