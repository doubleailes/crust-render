//! Faces into triangles: fan triangulation with its Ptex face and UV side
//! tables, and the rewrite of a refined mesh's face table to cage faces.

use openusd::usd::Prim;
use tracing::warn;

use crate::material::Material;
use crate::rt_world::{FaceMap, FanSlice, SubFace, UvMap};
use crate::scene::subdiv;

use super::source::{RefinedFaces, UvSource};

/// [`remap_subdivided_faces`] or [`remap_tessellated_faces`], by how the mesh
/// was refined.
pub(super) fn remap_refined_faces(map: FaceMap, faces: &RefinedFaces) -> FaceMap {
    match faces {
        RefinedFaces::Uniform(sub) => remap_subdivided_faces(map, sub),
        RefinedFaces::PerFace(per_face) => remap_tessellated_faces(map, per_face),
    }
}

/// Rewrites a [`FaceMap`] built by triangulating a per-face tessellation —
/// all triangles, so `faces` numbers them — into cage face ids plus each
/// triangle's explicit corners. A triangle of an `n`-gon is unmappable, as
/// under uniform refinement.
pub(super) fn remap_tessellated_faces(map: FaceMap, t: &subdiv::TessellatedFaces) -> FaceMap {
    let n = map.faces.len();
    let mut faces = Vec::with_capacity(n);
    let mut slices = Vec::with_capacity(n);
    let mut corners = Vec::with_capacity(n);
    for &tri in &map.faces {
        let tri = tri as usize;
        match t.base_face.get(tri).copied().flatten() {
            Some(base) => {
                faces.push(base);
                slices.push(FanSlice::Triangle);
                corners.push(t.corner_uvs[tri]);
            }
            None => {
                faces.push(u32::MAX);
                slices.push(FanSlice::Unmappable);
                corners.push([[0.0; 2]; 3]);
            }
        }
    }
    FaceMap {
        faces,
        slices,
        sub: None,
        corners: Some(corners),
        density: Vec::new(),
    }
}

/// Rewrites a [`FaceMap`] built by triangulating *refined* quads — whose
/// `faces` therefore number refined faces — into base-cage face ids plus
/// explicit sub-face UVs. The fan of a refined quad is `k=1 -> (v0,v1,v2)`
/// (`QuadLower`) and `k=2 -> (v0,v2,v3)` (`QuadUpper`), so each triangle
/// takes the matching three of its face's four corner UVs.
pub(super) fn remap_subdivided_faces(map: FaceMap, sub: &subdiv::SubdivFaces) -> FaceMap {
    let n = map.faces.len();
    let mut faces = Vec::with_capacity(n);
    let mut slices = Vec::with_capacity(n);
    let mut subs = Vec::with_capacity(n);
    for (&refined, &slice) in map.faces.iter().zip(&map.slices) {
        // A refined face with no Ptex-addressable ancestor (its cage face
        // was not a quad), or — never seen, since the refiner halves
        // dyadics — one whose corners are not a dyadic cell: unmappable.
        let cell = sub.base_face[refined as usize].and_then(|base| {
            SubFace::from_corners(&sub.corner_uvs[refined as usize]).map(|c| (base, c))
        });
        match cell {
            Some((base, cell)) => {
                faces.push(base);
                slices.push(slice);
                subs.push(cell);
            }
            None => {
                // The face table's own sentinel: there the layout is the point.
                faces.push(u32::MAX);
                slices.push(FanSlice::Unmappable);
                subs.push(SubFace::default());
            }
        }
    }
    FaceMap {
        faces,
        slices,
        sub: Some(subs),
        corners: None,
        density: Vec::new(),
    }
}

/// Warns when a per-face texture's face count disagrees with the mesh's.
///
/// A Ptex face id *is* a mesh face index, so the two counts must match
/// exactly. When they do not, the texture belongs to different geometry and
/// every lookup is quietly wrong — the render still completes, and still looks
/// like a plausible rock, which is precisely what makes it worth a warning.
/// `n_base_faces` is the *authored cage's* face count
/// ([`MeshSource::base_face_count`]) — never the refined count: subdivision
/// changes the mesh's face count but Ptex ids keep indexing the cage.
pub(super) fn check_face_count(prim: &Prim, n_base_faces: usize, material: &dyn Material) {
    let Some(tex) = material.face_texture() else {
        return;
    };
    if tex.num_faces() != n_base_faces {
        warn!(
            "Mesh at {} has {} faces but its per-face texture has {} — \
             the texture does not match this geometry, so shading will be wrong",
            prim.path(),
            n_base_faces,
            tex.num_faces()
        );
    }
}

/// Fan-triangulates the faces into an index-triple list; `None` if
/// nothing survives.
///
/// With `want_faces`, also returns the table mapping each emitted triangle
/// back to the source face it was cut from — what a per-face (Ptex) texture
/// needs, since its face ids index `counts`, not the triangles. The two
/// outputs are index-parallel by construction: every `push` to one pushes to
/// the other in the same statement, so the skip paths cannot desynchronise
/// them.
///
/// The triangle list, plus the per-face and per-triangle-UV side tables
/// when the material asked for them.
type Triangulated = (Vec<[u32; 3]>, Option<FaceMap>, Option<UvMap>);

pub(super) fn triangulate(
    counts: &[i32],
    indices: &[i32],
    n_verts: usize,
    want_faces: bool,
    uv_src: Option<&UvSource>,
) -> Option<Triangulated> {
    let mut tris: Vec<[u32; 3]> = Vec::new();
    let mut faces: Vec<u32> = Vec::new();
    let mut slices: Vec<FanSlice> = Vec::new();
    let mut corners: Vec<[u32; 3]> = Vec::new();
    // The chart's values plus one `(0, 0)` at the end for every corner the
    // source cannot index (`UvSource::at`'s answer), so a corner is an index
    // and never a copied value.
    let fallback = uv_src.map_or(0, |src| src.values.len() as u32);
    let mut offset = 0usize;
    for (face, &fc) in counts.iter().enumerate() {
        let fc = fc as usize;
        if fc < 3 || offset + fc > indices.len() {
            offset += fc;
            continue;
        }
        for k in 1..(fc - 1) {
            let i0 = indices[offset];
            let i1 = indices[offset + k];
            let i2 = indices[offset + k + 1];
            if i0 < 0 || i1 < 0 || i2 < 0 {
                continue;
            }
            let (i0, i1, i2) = (i0 as u32, i1 as u32, i2 as u32);
            if i0 as usize >= n_verts || i1 as usize >= n_verts || i2 as usize >= n_verts {
                continue;
            }
            tris.push([i0, i1, i2]);
            // Pushed in the same statement sequence as the triangle, so the
            // skip paths above cannot desynchronise the tables from it —
            // the same discipline `faces`/`slices` rely on.
            if let Some(src) = uv_src {
                // A faceVarying chart is indexed by the *face-vertex* slot,
                // which is why the fan's offsets are carried through rather
                // than just the point indices: two faces meeting at a seam
                // share `i0` but not its texture coordinate.
                corners.push([
                    src.index_at(offset, i0 as usize).unwrap_or(fallback),
                    src.index_at(offset + k, i1 as usize).unwrap_or(fallback),
                    src.index_at(offset + k + 1, i2 as usize)
                        .unwrap_or(fallback),
                ]);
            }
            if want_faces {
                faces.push(face as u32);
                // Ptex defines quad and triangle faces only, so a larger
                // polygon has no addressable texture — mark it rather than
                // inventing a parameterisation for it.
                slices.push(match (fc, k) {
                    (3, _) => FanSlice::Triangle,
                    (4, 1) => FanSlice::QuadLower,
                    (4, 2) => FanSlice::QuadUpper,
                    _ => FanSlice::Unmappable,
                });
            }
        }
        offset += fc;
    }
    if tris.is_empty() {
        return None;
    }
    // `then_some` rather than `then`: the value is two already-built vectors
    // being moved, so there is no work for a closure to defer.
    let map = want_faces.then_some(FaceMap {
        faces,
        slices,
        sub: None,
        corners: None,
        density: Vec::new(),
    });
    // The densities are left empty: they want the mesh's *local* vertices,
    // which this function does not receive, so the callers that hold them
    // fill them in (`MeshArena::intern`, and the non-invertible bake above).
    let uv_map = uv_src.map(|src| {
        let mut values = src.values.clone();
        values.push([0.0, 0.0]);
        UvMap {
            values,
            corners,
            density: Vec::new(),
        }
    });
    Some((tris, map, uv_map))
}
