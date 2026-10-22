// The trait lives in `material/material.rs` beside its implementations;
// renaming the file to satisfy the lint would only churn the history.
#[allow(clippy::module_inception)]
mod material;
pub(crate) use material::ResolvedBsdf;
pub use material::{
    BsdfDraw, CurveNeeds, Material, Resolution, ScatterSample, ShadingPoint, SubsurfaceEntry,
};
mod emissive;
pub use emissive::Emissive;
mod mesh_light;
pub use mesh_light::MeshLightMaterial;
pub(crate) mod brdf;
pub mod closure;
pub mod lobe;
mod ltc_sheen_table;
mod openpbr;
mod pattern;
pub use openpbr::{InteriorCache, OpenPBR, ResolvedOpenPBR};
pub mod materialx;
pub use materialx::MtlxMaterial;
pub mod displacement;
pub mod preview_surface;
pub use displacement::{DispRemap, Displacement, DisplacementValue, VertexCtx, VertexField};
pub use preview_surface::PreviewSurface;
