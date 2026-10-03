//! `UsdGeomMesh` → kernel triangles: interning distinct meshes, the deferred
//! instance-vs-bake decision, triangulation and the per-triangle side tables
//! (Ptex faces, UVs, texture density).
//!
//! | file        | holds                                                         |
//! |-------------|---------------------------------------------------------------|
//! | [`source`]  | authored arrays and UVs, subdivision ([`mesh_source`], [`SubdivPolicy`]) |
//! | [`faces`]   | triangulation, Ptex face tables, the refined-face remap       |
//! | [`arena`]   | interning by content ([`MeshArena`]), committing kernel scenes |
//! | [`bake`]    | placements: [`emit_mesh`], then bake or instance ([`flush_meshes`]) |
//!
//! The files share through `pub(super)`; what the rest of the importer uses
//! is `pub(in crate::scene::usd_import)` and re-exported here.

mod arena;
mod bake;
mod faces;
mod source;

#[cfg(test)]
mod tests;

pub(super) use arena::MeshArena;
pub(super) use bake::{MeshPlacement, emit_mesh, flush_meshes, placement_scale};
pub(super) use source::{MeshPlace, SubdivPolicy, mesh_source};
