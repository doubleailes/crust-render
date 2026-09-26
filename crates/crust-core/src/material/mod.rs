// The trait lives in `material/material.rs` beside its implementations;
// renaming the file to satisfy the lint would only churn the history.
#[allow(clippy::module_inception)]
mod material;
pub use material::{Material, Resolution, ScatterSample, ShadingPoint};
mod emissive;
pub use emissive::Emissive;
mod brdf;
mod openpbr;
pub use openpbr::OpenPBR;
pub mod materialx;
pub use materialx::MtlxMaterial;
pub mod preview_surface;
pub use preview_surface::PreviewSurface;
