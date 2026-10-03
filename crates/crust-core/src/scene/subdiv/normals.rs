//! Smooth per-vertex shading normals, for refined meshes and unrefined cages.

use glam::Vec3A;
use openusd::gf::Vec3f;

use super::topology::validate_cage;

/// Smooth per-vertex normals for a polygon mesh: each face accumulates its
/// *unnormalized* area vector (the sum of its fan's cross products — twice
/// the face normal scaled by area, so larger faces weigh more) onto **every**
/// vertex of the face — per-fan-triangle accumulation would weigh a vertex
/// by where it happens to sit in the fan. Every sum is then normalized
/// (zero-length sums fall back to +Y rather than yield NaNs; the kernel
/// treats shading normals as directions only).
pub(crate) fn smooth_normals(verts: &[[f32; 3]], counts: &[i32], indices: &[i32]) -> Vec<[f32; 3]> {
    let at = |i: usize| Vec3A::from_array(verts[i]);
    let mut sums = vec![Vec3A::ZERO; verts.len()];
    let mut off = 0usize;
    for &fc in counts {
        let fc = fc as usize;
        let face = &indices[off..off + fc];
        off += fc;
        let v0 = at(face[0] as usize);
        let mut area = Vec3A::ZERO;
        for k in 1..fc - 1 {
            let (i1, i2) = (face[k] as usize, face[k + 1] as usize);
            area += (at(i1) - v0).cross(at(i2) - v0);
        }
        for &i in face {
            sums[i as usize] += area;
        }
    }
    sums.iter()
        .map(|n| {
            if n.length_squared() > 1e-20 {
                n.normalize().to_array()
            } else {
                [0.0, 1.0, 0.0]
            }
        })
        .collect()
}

/// Smooth per-vertex normals for an *unrefined* subdivision cage — what a
/// subdivision surface shades with at refinement level 0, as Hydra's Storm
/// does at low complexity: the cage's silhouette, the surface's shading.
/// `None` for a cage [`subdivide`] would refuse too; it then renders faceted.
pub(crate) fn smooth_cage_normals(
    points: &[Vec3f],
    counts: &[i32],
    indices: &[i32],
) -> Option<Vec<[f32; 3]>> {
    validate_cage(points.len(), counts, indices).ok()?;
    let verts: Vec<[f32; 3]> = points.iter().map(|p| [p.x, p.y, p.z]).collect();
    Some(smooth_normals(&verts, counts, indices))
}
