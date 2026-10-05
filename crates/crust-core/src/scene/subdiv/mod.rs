//! Subdivision-surface refinement for USD meshes, via the pure-Rust
//! [`opensubdiv-rs`] port of OpenSubdiv's Far/Sdc layers.
//!
//! The importer hands this module a base cage (points + faceVertexCounts +
//! faceVertexIndices, exactly as authored) and gets back a uniformly refined
//! mesh whose positions sit **on the limit surface** and whose vertices carry
//! smooth shading normals. Everything downstream — triangulation, interning,
//! instancing, baking — then treats the refined mesh like any other polygon
//! mesh.
//!
//! Ptex face ids index the *base cage*, so when the caller needs per-face
//! texturing the refinement also reports, per refined face, which cage face
//! it descends from and where its corners sit inside that face's unit square
//! ([`SubdivFaces`]). The sub-face UVs come from a synthetic face-varying
//! channel (each cage face owns four values at the Ptex corners, so every
//! edge of the channel is a face-varying boundary and it refines bilinearly
//! under every [`FVarLinearInterpolation`] rule but `None`, which is
//! refined apart) — the channel *is* the parameterization, so the smoothing
//! rules must never touch it.
//!
//! The authored texture chart ([`UvChannel`]) is refined with the surface:
//! a `faceVarying` chart as a real face-varying channel under the mesh's
//! `faceVaryingLinearInterpolation`, a `vertex` chart like the points. Both
//! are snapped to the limit, so a texel stays on the limit-surface point its
//! vertex was snapped to.
//!
//! [`opensubdiv-rs`]: https://github.com/doubleailes/OpenSubdiv-rs
//! [`FVarLinearInterpolation`]: sdc::FVarLinearInterpolation

mod adaptive;
mod normals;
#[cfg(test)]
mod tests;
mod topology;
mod uniform;

use opensubdiv_rs::sdc;
use openusd::gf::Vec3f;
use std::fmt;

pub(crate) use adaptive::{QUALITY_BINS, TessellatedFaces, TessellatedMesh, tessellate_adaptive};
pub(crate) use normals::{smooth_cage_normals, smooth_normals};
pub(crate) use uniform::subdivide;

/// The subdivision schemes the importer maps from `subdivisionScheme`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SubdivScheme {
    CatmullClark,
    Bilinear,
    /// Loop subdivision refines triangles into triangles; the factory rejects
    /// any non-triangular face, so the caller pre-checks the cage.
    Loop,
}

/// Everything `subdivide` needs beyond the cage arrays, borrowed straight
/// from the authored attributes.
#[derive(Clone, Copy)]
pub(crate) struct SubdivRequest<'a> {
    pub scheme: SubdivScheme,
    /// Uniform refinement depth, `>= 1` (level 0 never reaches this module).
    pub level: u32,
    pub boundary: sdc::VtxBoundaryInterpolation,
    /// USD authors creases as runs of vertices: run `i` spans
    /// `crease_lengths[i]` consecutive entries of `crease_indices` and
    /// describes `crease_lengths[i] - 1` edges.
    pub crease_indices: &'a [i32],
    pub crease_lengths: &'a [i32],
    /// One sharpness per run *or* one per edge — both are legal USD.
    pub crease_sharpnesses: &'a [f32],
    pub corner_indices: &'a [i32],
    pub corner_sharpnesses: &'a [f32],
    /// Build [`SubdivFaces`] (only wanted when the material samples a
    /// per-face texture).
    pub want_face_uvs: bool,
    /// The authored texture chart to refine, when the material reads one.
    pub uvs: Option<UvChannel<'a>>,
}

/// An authored texture-coordinate primvar, as the importer read it. The
/// caller checks it with [`UvChannel::is_well_formed`] first — a refiner
/// cannot skip a bad value the way a triangle lookup can.
#[derive(Clone, Copy)]
pub(crate) struct UvChannel<'a> {
    pub values: &'a [[f32; 2]],
    /// The primvar's `:indices` into `values`, or `None` for direct
    /// addressing.
    pub indices: Option<&'a [i32]>,
    /// `faceVarying` (one entry per face-vertex) rather than `vertex` (one
    /// per point).
    pub face_varying: bool,
    /// The mesh's `faceVaryingLinearInterpolation`; only a `faceVarying`
    /// chart reads it.
    pub linear: sdc::FVarLinearInterpolation,
}

impl UvChannel<'_> {
    /// Whether every entry the cage addresses resolves to a value:
    /// `n_entries` is the face-vertex count for a `faceVarying` chart, the
    /// point count for a `vertex` one.
    pub(crate) fn is_well_formed(&self, n_entries: usize) -> bool {
        match self.indices {
            Some(idx) => {
                idx.len() >= n_entries
                    && idx[..n_entries]
                        .iter()
                        .all(|&i| i >= 0 && (i as usize) < self.values.len())
            }
            None => self.values.len() >= n_entries,
        }
    }

    /// The value entry `i` (a face-vertex or a point) addresses.
    pub(super) fn value_index(&self, i: usize) -> usize {
        match self.indices {
            Some(idx) => idx[i] as usize,
            None => i,
        }
    }
}

/// The refined chart, in the shape the importer's `UvSource` holds.
pub(crate) struct RefinedUvs {
    pub values: Vec<[f32; 2]>,
    /// Per refined face-vertex, into `values` — `Some` iff `face_varying`.
    pub indices: Option<Vec<i32>>,
    pub face_varying: bool,
}

/// Per refined face: the base-cage face it descends from and its corner UVs
/// inside that face's unit square (Ptex convention: `v0=(0,0) v1=(1,0)
/// v2=(1,1) v3=(0,1)`).
pub(crate) struct SubdivFaces {
    /// `None` for a face with no Ptex-addressable ancestor (its cage face
    /// was not a quad — Ptex subfaces are out of scope, matching the
    /// unsubdivided importer's treatment of n-gons).
    pub base_face: Vec<Option<u32>>,
    /// Corner UVs of the refined quad, in `face_vertices` order.
    pub corner_uvs: Vec<[[f32; 2]; 4]>,
}

/// A refined mesh in the same array shapes `triangulate` consumes.
pub(crate) struct SubdividedMesh {
    /// Limit-surface positions (uniform refinement, then limit snap).
    pub points: Vec<Vec3f>,
    /// All 4s for Catmull-Clark/Bilinear, all 3s for Loop.
    pub counts: Vec<i32>,
    pub indices: Vec<i32>,
    /// Smooth per-vertex shading normals, parallel to `points`, unpadded.
    pub normals: Vec<[f32; 3]>,
    /// `Some` iff the request asked for face UVs.
    pub faces: Option<SubdivFaces>,
    /// `Some` iff the request carried a texture chart.
    pub uvs: Option<RefinedUvs>,
}

#[derive(Debug)]
pub(crate) enum SubdivError {
    /// The cage failed validation before it reached the refiner — unlike
    /// `triangulate`, a topology refiner cannot skip a malformed face, so the
    /// whole mesh degrades to its cage.
    BadTopology(String),
    Refine(opensubdiv_rs::far::Error),
}

impl fmt::Display for SubdivError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubdivError::BadTopology(why) => write!(f, "{why}"),
            SubdivError::Refine(e) => write!(f, "{e}"),
        }
    }
}
