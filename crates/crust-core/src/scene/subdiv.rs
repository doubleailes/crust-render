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
//! channel (each cage face owns four values at the Ptex corners, refined with
//! [`FVarLinearInterpolation::All`], i.e. pure bilinear interpolation) — the
//! channel *is* the parameterization, so the smoothing rules must never touch
//! it.
//!
//! [`opensubdiv-rs`]: https://github.com/doubleailes/OpenSubdiv-rs

use glam::Vec3A;
use opensubdiv_rs::far::{
    FVarChannelDescriptor, PrimvarRefiner, TopologyDescriptor, TopologyRefinerFactory,
    UniformOptions,
};
use opensubdiv_rs::sdc;
use openusd::gf::Vec3f;
use std::fmt;

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
}

/// Per refined face: the base-cage face it descends from and its corner UVs
/// inside that face's unit square (Ptex convention: `v0=(0,0) v1=(1,0)
/// v2=(1,1) v3=(0,1)`).
pub(crate) struct SubdivFaces {
    /// `u32::MAX` marks a face with no Ptex-addressable ancestor (its cage
    /// face was not a quad — Ptex subfaces are out of scope, matching the
    /// unsubdivided importer's treatment of n-gons).
    pub base_face: Vec<u32>,
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
    /// Smooth per-vertex shading normals, parallel to `points`.
    pub normals: Vec<Vec3A>,
    /// `Some` iff the request asked for face UVs.
    pub faces: Option<SubdivFaces>,
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

/// Uniformly refines the cage to `req.level` and snaps the result to the
/// limit surface. See the module docs for the shape of the answer.
pub(crate) fn subdivide(
    points: &[Vec3f],
    counts: &[i32],
    indices: &[i32],
    req: &SubdivRequest,
) -> Result<SubdividedMesh, SubdivError> {
    let (counts_us, indices_u32) = validate_cage(points.len(), counts, indices)?;
    let (crease_pairs, crease_weights) = expand_crease_runs(
        req.crease_indices,
        req.crease_lengths,
        req.crease_sharpnesses,
    )?;
    let corners = validate_corners(req.corner_indices, req.corner_sharpnesses)?;

    let scheme = match req.scheme {
        SubdivScheme::CatmullClark => sdc::SchemeType::Catmark,
        SubdivScheme::Bilinear => sdc::SchemeType::Bilinear,
        SubdivScheme::Loop => sdc::SchemeType::Loop,
    };
    // The synthetic Ptex channel must interpolate bilinearly everywhere —
    // any corner/boundary smoothing would bend the parameterization itself.
    let options = sdc::Options::default()
        .with_vtx_boundary_interpolation(req.boundary)
        .with_fvar_linear_interpolation(sdc::FVarLinearInterpolation::All);

    // Loop cannot refine quads at all, and the Ptex channel only makes sense
    // for the quad-split schemes.
    let want_uvs = req.want_face_uvs && req.scheme != SubdivScheme::Loop;
    let (fvar_uvs, fvar_indices) = if want_uvs {
        ptex_fvar_channel(&counts_us)
    } else {
        (Vec::new(), Vec::new())
    };
    let channels = [FVarChannelDescriptor::new(fvar_uvs.len(), &fvar_indices)];

    let mut descriptor = TopologyDescriptor::new(points.len(), &counts_us, &indices_u32)
        .with_creases(&crease_pairs, &crease_weights)
        .with_corners(&corners.0, &corners.1);
    if want_uvs {
        descriptor = descriptor.with_fvar_channels(&channels);
    }

    let mut refiner =
        TopologyRefinerFactory::create(descriptor, scheme, options).map_err(SubdivError::Refine)?;
    let level = req.level as usize;
    refiner.refine_uniform(UniformOptions::new(level));

    // Positions: interpolate level by level, then snap the last level to the
    // limit surface. (For Bilinear the limit is the refined mesh itself;
    // limit_level handles that uniformly.)
    let primvar = PrimvarRefiner::new(&refiner);
    let mut verts: Vec<[f32; 3]> = points.iter().map(|p| [p.x, p.y, p.z]).collect();
    for l in 1..=level {
        let mut refined = vec![[0.0f32; 3]; refiner.level(l).num_vertices()];
        primvar.interpolate(l, &verts, &mut refined);
        verts = refined;
    }
    let mut limit = vec![[0.0f32; 3]; verts.len()];
    primvar.limit(&verts, &mut limit);

    // Topology of the last level, back in the importer's array shapes.
    let last = refiner.level(level);
    let n_faces = last.num_faces();
    let mut out_counts = Vec::with_capacity(n_faces);
    let mut out_indices = Vec::with_capacity(last.num_face_vertices_total());
    for f in 0..n_faces {
        let fv = last.face_vertices(f);
        out_counts.push(fv.len() as i32);
        out_indices.extend(fv.iter().map(|&v| v as i32));
    }

    let faces = want_uvs.then(|| {
        // Base-cage face per refined face: compose the one-step
        // child-to-parent maps from the last refinement down to level 0.
        let mut base_face: Vec<u32> = (0..n_faces as u32).collect();
        for l in (1..=level).rev() {
            let refinement = refiner.refinement(l);
            for f in &mut base_face {
                *f = refinement.child_face_parent_face(*f as usize);
            }
        }
        for f in &mut base_face {
            if counts[*f as usize] != 4 {
                *f = u32::MAX;
            }
        }

        // Sub-face corner UVs: refine the synthetic channel the same way the
        // positions were refined, then read each face's four values.
        let mut uvs = fvar_uvs.clone();
        for l in 1..=level {
            let mut refined = vec![[0.0f32; 2]; refiner.level(l).num_fvar_values(0)];
            primvar.interpolate_face_varying(l, 0, &uvs, &mut refined);
            uvs = refined;
        }
        let corner_uvs = (0..n_faces)
            .map(|f| {
                let fv = last.face_fvar_values(f, 0);
                debug_assert_eq!(fv.len(), 4, "quad-split schemes only refine into quads");
                [
                    uvs[fv[0] as usize],
                    uvs[fv[1] as usize],
                    uvs[fv[2] as usize],
                    uvs[fv[3] as usize],
                ]
            })
            .collect();
        SubdivFaces {
            base_face,
            corner_uvs,
        }
    });

    let points: Vec<Vec3f> = limit
        .iter()
        .map(|p| Vec3f::from([p[0], p[1], p[2]]))
        .collect();
    let verts_a: Vec<Vec3A> = limit.iter().map(|p| Vec3A::from_array(*p)).collect();
    let normals = smooth_normals(&verts_a, &out_counts, &out_indices);

    Ok(SubdividedMesh {
        points,
        counts: out_counts,
        indices: out_indices,
        normals,
        faces,
    })
}

/// Smooth per-vertex normals for a polygon mesh: each face accumulates its
/// *unnormalized* area vector (the sum of its fan's cross products — twice
/// the face normal scaled by area, so larger faces weigh more) onto **every**
/// vertex of the face — per-fan-triangle accumulation would weigh a vertex
/// by where it happens to sit in the fan. Every sum is then normalized
/// (zero-length sums fall back to +Y rather than yield NaNs; the kernel
/// treats shading normals as directions only).
pub(crate) fn smooth_normals(verts: &[Vec3A], counts: &[i32], indices: &[i32]) -> Vec<Vec3A> {
    let mut sums = vec![Vec3A::ZERO; verts.len()];
    let mut off = 0usize;
    for &fc in counts {
        let fc = fc as usize;
        let face = &indices[off..off + fc];
        off += fc;
        let v0 = verts[face[0] as usize];
        let mut area = Vec3A::ZERO;
        for k in 1..fc - 1 {
            let (i1, i2) = (face[k] as usize, face[k + 1] as usize);
            area += (verts[i1] - v0).cross(verts[i2] - v0);
        }
        for &i in face {
            sums[i as usize] += area;
        }
    }
    sums.iter()
        .map(|n| {
            if n.length_squared() > 1e-20 {
                n.normalize()
            } else {
                Vec3A::Y
            }
        })
        .collect()
}

/// The refiner indexes with `usize` counts and `u32` indices, and it cannot
/// skip a malformed face the way `triangulate` does — so the cage is checked
/// whole, up front.
fn validate_cage(
    n_verts: usize,
    counts: &[i32],
    indices: &[i32],
) -> Result<(Vec<usize>, Vec<u32>), SubdivError> {
    let mut total = 0usize;
    let mut counts_us = Vec::with_capacity(counts.len());
    for (face, &fc) in counts.iter().enumerate() {
        if fc < 3 {
            return Err(SubdivError::BadTopology(format!(
                "face {face} has {fc} vertices (need at least 3)"
            )));
        }
        counts_us.push(fc as usize);
        total += fc as usize;
    }
    if total != indices.len() {
        return Err(SubdivError::BadTopology(format!(
            "faceVertexCounts sums to {total} but faceVertexIndices has {} entries",
            indices.len()
        )));
    }
    let mut indices_u32 = Vec::with_capacity(indices.len());
    for &i in indices {
        if i < 0 || i as usize >= n_verts {
            return Err(SubdivError::BadTopology(format!(
                "face vertex index {i} out of range (mesh has {n_verts} points)"
            )));
        }
        indices_u32.push(i as u32);
    }
    Ok((counts_us, indices_u32))
}

/// Expands USD crease runs into the per-edge vertex pairs the refiner wants.
/// A run of `n` vertices contributes `n - 1` edges; `sharpnesses` carries
/// either one value per run or one per edge. Sharpness 10 is USD's "as sharp
/// as possible", which is exactly `sdc::SHARPNESS_INFINITE`; anything at or
/// above it is clamped there.
fn expand_crease_runs(
    indices: &[i32],
    lengths: &[i32],
    sharpnesses: &[f32],
) -> Result<(Vec<[u32; 2]>, Vec<f32>), SubdivError> {
    if lengths.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut n_edges = 0usize;
    let mut n_verts = 0usize;
    for (run, &len) in lengths.iter().enumerate() {
        if len < 2 {
            return Err(SubdivError::BadTopology(format!(
                "crease run {run} has length {len} (need at least 2 vertices)"
            )));
        }
        n_edges += len as usize - 1;
        n_verts += len as usize;
    }
    if n_verts != indices.len() {
        return Err(SubdivError::BadTopology(format!(
            "creaseLengths sums to {n_verts} but creaseIndices has {} entries",
            indices.len()
        )));
    }
    let per_run = sharpnesses.len() == lengths.len();
    if !per_run && sharpnesses.len() != n_edges {
        return Err(SubdivError::BadTopology(format!(
            "creaseSharpnesses has {} entries (want {} per-run or {n_edges} per-edge)",
            sharpnesses.len(),
            lengths.len()
        )));
    }
    let clamp = |s: f32| {
        if s >= sdc::SHARPNESS_INFINITE {
            sdc::SHARPNESS_INFINITE
        } else {
            s.max(0.0)
        }
    };
    let mut pairs = Vec::with_capacity(n_edges);
    let mut weights = Vec::with_capacity(n_edges);
    let mut off = 0usize;
    let mut edge = 0usize;
    for (run, &len) in lengths.iter().enumerate() {
        for k in 0..len as usize - 1 {
            let (a, b) = (indices[off + k], indices[off + k + 1]);
            if a < 0 || b < 0 {
                return Err(SubdivError::BadTopology(format!(
                    "crease run {run} has a negative vertex index"
                )));
            }
            pairs.push([a as u32, b as u32]);
            weights.push(clamp(if per_run {
                sharpnesses[run]
            } else {
                sharpnesses[edge]
            }));
            edge += 1;
        }
        off += len as usize;
    }
    Ok((pairs, weights))
}

fn validate_corners(
    indices: &[i32],
    sharpnesses: &[f32],
) -> Result<(Vec<u32>, Vec<f32>), SubdivError> {
    if indices.len() != sharpnesses.len() {
        return Err(SubdivError::BadTopology(format!(
            "cornerIndices has {} entries but cornerSharpnesses has {}",
            indices.len(),
            sharpnesses.len()
        )));
    }
    let mut out = Vec::with_capacity(indices.len());
    for &i in indices {
        if i < 0 {
            return Err(SubdivError::BadTopology(
                "cornerIndices has a negative vertex index".into(),
            ));
        }
        out.push(i as u32);
    }
    let weights = sharpnesses
        .iter()
        .map(|&s| {
            if s >= sdc::SHARPNESS_INFINITE {
                sdc::SHARPNESS_INFINITE
            } else {
                s.max(0.0)
            }
        })
        .collect();
    Ok((out, weights))
}

/// The synthetic face-varying channel carrying each cage face's Ptex
/// parameterization: one value per face-vertex (`0..sum(counts)` in authored
/// order, so no value is shared across faces), quads seeded with the four
/// Ptex corners. Non-quad faces get zeros — their descendants are marked
/// unmappable regardless, the channel just has to be well-formed.
fn ptex_fvar_channel(counts: &[usize]) -> (Vec<[f32; 2]>, Vec<u32>) {
    const QUAD: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let total: usize = counts.iter().sum();
    let mut values = Vec::with_capacity(total);
    for &fc in counts {
        if fc == 4 {
            values.extend_from_slice(&QUAD);
        } else {
            values.extend(std::iter::repeat_n([0.0f32; 2], fc));
        }
    }
    let indices = (0..total as u32).collect();
    (values, indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ±1 cube authored as six quads (the winding matches
    /// `samples/subdivision.usda`).
    fn cube() -> (Vec<Vec3f>, Vec<i32>, Vec<i32>) {
        let points = vec![
            Vec3f::from([-1.0, -1.0, 1.0]),
            Vec3f::from([1.0, -1.0, 1.0]),
            Vec3f::from([1.0, 1.0, 1.0]),
            Vec3f::from([-1.0, 1.0, 1.0]),
            Vec3f::from([-1.0, -1.0, -1.0]),
            Vec3f::from([1.0, -1.0, -1.0]),
            Vec3f::from([1.0, 1.0, -1.0]),
            Vec3f::from([-1.0, 1.0, -1.0]),
        ];
        let counts = vec![4; 6];
        let indices = vec![
            0, 1, 2, 3, // +Z
            5, 4, 7, 6, // -Z
            4, 0, 3, 7, // -X
            1, 5, 6, 2, // +X
            3, 2, 6, 7, // +Y
            4, 5, 1, 0, // -Y
        ];
        (points, counts, indices)
    }

    fn request(level: u32) -> SubdivRequest<'static> {
        SubdivRequest {
            scheme: SubdivScheme::CatmullClark,
            level,
            boundary: sdc::VtxBoundaryInterpolation::EdgeAndCorner,
            crease_indices: &[],
            crease_lengths: &[],
            crease_sharpnesses: &[],
            corner_indices: &[],
            corner_sharpnesses: &[],
            want_face_uvs: false,
        }
    }

    #[test]
    fn cube_level_one_topology() {
        let (points, counts, indices) = cube();
        let out = subdivide(&points, &counts, &indices, &request(1)).unwrap();
        assert_eq!(out.points.len(), 26, "8 corners + 12 edge + 6 face points");
        assert_eq!(out.counts.len(), 24, "each quad splits in four");
        assert!(out.counts.iter().all(|&c| c == 4));
        assert_eq!(out.indices.len(), 96);
        assert_eq!(out.normals.len(), out.points.len());
    }

    #[test]
    fn limit_shrinks_strictly_inside_the_cage() {
        let (points, counts, indices) = cube();
        let out = subdivide(&points, &counts, &indices, &request(2)).unwrap();
        for p in &out.points {
            for c in [p.x, p.y, p.z] {
                assert!(c.abs() < 1.0, "limit point {p:?} not inside the cage");
            }
        }
        let max = out
            .points
            .iter()
            .flat_map(|p| [p.x.abs(), p.y.abs(), p.z.abs()])
            .fold(0.0f32, f32::max);
        assert!(max > 0.5, "limit surface collapsed too far ({max})");
    }

    #[test]
    fn fully_creased_cube_keeps_its_cage() {
        let (points, counts, indices) = cube();
        // All 12 edges as runs of 2 vertices, one sharpness per run.
        let crease_indices: Vec<i32> = vec![
            0, 1, 1, 2, 2, 3, 3, 0, // +Z ring
            4, 5, 5, 6, 6, 7, 7, 4, // -Z ring
            0, 4, 1, 5, 2, 6, 3, 7, // connecting edges
        ];
        let crease_lengths = vec![2; 12];
        let crease_sharpnesses = vec![10.0f32; 12];
        let req = SubdivRequest {
            crease_indices: &crease_indices,
            crease_lengths: &crease_lengths,
            crease_sharpnesses: &crease_sharpnesses,
            ..request(2)
        };
        let out = subdivide(&points, &counts, &indices, &req).unwrap();
        for axis in 0..3 {
            let coords = out.points.iter().map(|p| [p.x, p.y, p.z][axis]);
            let max = coords.clone().fold(f32::MIN, f32::max);
            let min = coords.fold(f32::MAX, f32::min);
            assert!((max - 1.0).abs() < 1e-5, "axis {axis} max {max}");
            assert!((min + 1.0).abs() < 1e-5, "axis {axis} min {min}");
        }
    }

    #[test]
    fn crease_runs_expand_per_run_and_per_edge() {
        // One run of 3 vertices = 2 edges.
        let (pairs, w) = expand_crease_runs(&[0, 1, 2], &[3], &[10.0]).unwrap();
        assert_eq!(pairs, vec![[0, 1], [1, 2]]);
        assert_eq!(w, vec![10.0, 10.0], "per-run sharpness covers every edge");

        let (_, w) = expand_crease_runs(&[0, 1, 2], &[3], &[2.0, 4.0]).unwrap();
        assert_eq!(w, vec![2.0, 4.0], "per-edge sharpness passes through");

        assert!(
            expand_crease_runs(&[0, 1, 2], &[3], &[1.0, 2.0, 3.0]).is_err(),
            "3 sharpnesses fit neither 1 run nor 2 edges"
        );
        assert!(expand_crease_runs(&[0, 1], &[3], &[1.0]).is_err());
        assert!(expand_crease_runs(&[0], &[1], &[1.0]).is_err());
    }

    #[test]
    fn malformed_cages_are_rejected_whole() {
        let (points, mut counts, indices) = cube();
        counts[0] = 2;
        assert!(matches!(
            subdivide(&points, &counts, &indices, &request(1)),
            Err(SubdivError::BadTopology(_))
        ));

        let (points, counts, mut indices) = cube();
        indices[0] = 8;
        assert!(subdivide(&points, &counts, &indices, &request(1)).is_err());

        let (points, counts, _) = cube();
        assert!(subdivide(&points, &counts, &[0, 1, 2], &request(1)).is_err());
    }

    #[test]
    fn level_one_face_uvs_tile_the_quadrants() {
        let (points, counts, indices) = cube();
        let req = SubdivRequest {
            want_face_uvs: true,
            ..request(1)
        };
        let out = subdivide(&points, &counts, &indices, &req).unwrap();
        let faces = out.faces.expect("face UVs were requested");
        assert_eq!(faces.base_face.len(), 24);
        assert_eq!(faces.corner_uvs.len(), 24);
        // Children of one parent are contiguous and in corner order, so the
        // base_face map is 4 children per cage face...
        for (child, &base) in faces.base_face.iter().enumerate() {
            assert_eq!(base, (child / 4) as u32);
        }
        // ...and each cage face's four children tile its unit square: every
        // child covers a quarter, together they cover the whole, and every
        // Ptex corner of the parent appears in exactly one child.
        for parent in 0..6 {
            let children = &faces.corner_uvs[parent * 4..parent * 4 + 4];
            let mut corner_hits = 0;
            for quad in children {
                let (mut umin, mut umax) = (f32::MAX, f32::MIN);
                let (mut vmin, mut vmax) = (f32::MAX, f32::MIN);
                for [u, v] in quad {
                    umin = umin.min(*u);
                    umax = umax.max(*u);
                    vmin = vmin.min(*v);
                    vmax = vmax.max(*v);
                }
                assert!((umax - umin - 0.5).abs() < 1e-6, "child spans half of u");
                assert!((vmax - vmin - 0.5).abs() < 1e-6, "child spans half of v");
                for corner in [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]] {
                    if quad
                        .iter()
                        .any(|c| (c[0] - corner[0]).abs() < 1e-6 && (c[1] - corner[1]).abs() < 1e-6)
                    {
                        corner_hits += 1;
                    }
                }
            }
            assert_eq!(corner_hits, 4, "parent {parent}'s corners split 1:1");
        }
    }

    #[test]
    fn non_quad_base_faces_are_unmappable() {
        // A quad with one corner cut off: one triangle + one pentagon.
        let points = vec![
            Vec3f::from([0.0, 0.0, 0.0]),
            Vec3f::from([2.0, 0.0, 0.0]),
            Vec3f::from([2.0, 1.0, 0.0]),
            Vec3f::from([1.0, 2.0, 0.0]),
            Vec3f::from([0.0, 2.0, 0.0]),
            Vec3f::from([2.0, 2.0, 0.0]),
        ];
        let counts = vec![5, 3];
        let indices = vec![0, 1, 2, 3, 4, 2, 5, 3];
        let req = SubdivRequest {
            want_face_uvs: true,
            ..request(1)
        };
        let out = subdivide(&points, &counts, &indices, &req).unwrap();
        let faces = out.faces.unwrap();
        assert_eq!(faces.base_face.len(), 8, "5 + 3 children");
        assert!(faces.base_face.iter().all(|&f| f == u32::MAX));
    }

    #[test]
    fn smooth_cube_normals_point_along_the_corner_diagonals() {
        let (points, counts, indices) = cube();
        let verts: Vec<Vec3A> = points.iter().map(|p| Vec3A::new(p.x, p.y, p.z)).collect();
        let normals = smooth_normals(&verts, &counts, &indices);
        for (v, n) in verts.iter().zip(&normals) {
            let expect = v.normalize();
            assert!(
                n.dot(expect) > 0.99,
                "corner {v:?} normal {n:?} not along its diagonal"
            );
        }
    }

    #[test]
    fn loop_refines_triangles() {
        let points = vec![
            Vec3f::from([0.0, 0.0, 0.0]),
            Vec3f::from([1.0, 0.0, 0.0]),
            Vec3f::from([0.0, 1.0, 0.0]),
            Vec3f::from([1.0, 1.0, 1.0]),
        ];
        let counts = vec![3, 3];
        let indices = vec![0, 1, 2, 1, 3, 2];
        let req = SubdivRequest {
            scheme: SubdivScheme::Loop,
            ..request(1)
        };
        let out = subdivide(&points, &counts, &indices, &req).unwrap();
        assert_eq!(out.counts.len(), 8, "each triangle splits in four");
        assert!(out.counts.iter().all(|&c| c == 3));
    }
}
