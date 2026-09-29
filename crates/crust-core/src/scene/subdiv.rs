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
    fn value_index(&self, i: usize) -> usize {
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
    /// Smooth per-vertex shading normals, parallel to `points`.
    pub normals: Vec<Vec3A>,
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

/// Uniformly refines the cage to `req.level` and snaps the result to the
/// limit surface. See the module docs for the shape of the answer.
pub(crate) fn subdivide(
    points: &[Vec3f],
    counts: &[i32],
    indices: &[i32],
    req: &SubdivRequest,
) -> Result<SubdividedMesh, SubdivError> {
    // `none` is the one face-varying rule that smooths face-varying corners,
    // which would slide the Ptex channel sharing its refiner off its
    // sub-faces. Refine the face table on its own, chartless, and the rest
    // without it — twice the refinement, for a material that reads Ptex *and*
    // a `none` chart. (Pinned by `ptex_channel_is_invariant_under_every_fvar_rule`.)
    if req.want_face_uvs
        && req
            .uvs
            .is_some_and(|c| c.face_varying && c.linear == sdc::FVarLinearInterpolation::None)
    {
        let faces = subdivide(
            points,
            counts,
            indices,
            &SubdivRequest { uvs: None, ..*req },
        )?
        .faces;
        let mut out = subdivide(
            points,
            counts,
            indices,
            &SubdivRequest {
                want_face_uvs: false,
                ..*req
            },
        )?;
        out.faces = faces;
        return Ok(out);
    }
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
    // The face-varying rule is one per refiner, not per channel, so the
    // authored chart's rule wins. The synthetic Ptex channel must refine
    // bilinearly, and does under every rule but `none` (handled above) —
    // each of its values is private to one face, so every one of its edges
    // is a face-varying boundary, and its data is affine. Without a chart,
    // `All`.
    let chart = req.uvs.as_ref().filter(|c| c.face_varying);
    let options = sdc::Options::default()
        .with_vtx_boundary_interpolation(req.boundary)
        .with_fvar_linear_interpolation(
            chart.map_or(sdc::FVarLinearInterpolation::All, |c| c.linear),
        );

    // Loop cannot refine quads at all, and the Ptex channel only makes sense
    // for the quad-split schemes.
    let want_uvs = req.want_face_uvs && req.scheme != SubdivScheme::Loop;
    let (fvar_uvs, fvar_indices) = if want_uvs {
        ptex_fvar_channel(&counts_us)
    } else {
        (Vec::new(), Vec::new())
    };
    // A face-varying chart's per-face-vertex indices are its channel's
    // topology; its values seed the refinement.
    let chart_indices: Vec<u32> = match chart {
        Some(c) => (0..indices.len())
            .map(|fv| c.value_index(fv) as u32)
            .collect(),
        None => Vec::new(),
    };
    let mut channels = Vec::with_capacity(2);
    if want_uvs {
        channels.push(FVarChannelDescriptor::new(fvar_uvs.len(), &fvar_indices));
    }
    let chart_channel = chart.map(|c| {
        channels.push(FVarChannelDescriptor::new(c.values.len(), &chart_indices));
        channels.len() - 1
    });

    let mut descriptor = TopologyDescriptor::new(points.len(), &counts_us, &indices_u32)
        .with_creases(&crease_pairs, &crease_weights)
        .with_corners(&corners.0, &corners.1);
    if !channels.is_empty() {
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
        let base_face: Vec<Option<u32>> = base_face
            .into_iter()
            .map(|f| (counts[f as usize] == 4).then_some(f))
            .collect();

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

    let uvs = req.uvs.as_ref().map(|c| match chart_channel {
        Some(ch) => {
            // Face-varying: refine the values level by level, snap them to
            // the limit, then read each refined face's entries.
            let mut values = c.values.to_vec();
            for l in 1..=level {
                let mut refined = vec![[0.0f32; 2]; refiner.level(l).num_fvar_values(ch)];
                primvar.interpolate_face_varying(l, ch, &values, &mut refined);
                values = refined;
            }
            let mut limit_uvs = vec![[0.0f32; 2]; values.len()];
            primvar.limit_face_varying(ch, &values, &mut limit_uvs);
            let mut fv_indices = Vec::with_capacity(out_indices.len());
            for f in 0..n_faces {
                fv_indices.extend(last.face_fvar_values(f, ch).iter().map(|&v| v as i32));
            }
            RefinedUvs {
                values: limit_uvs,
                indices: Some(fv_indices),
                face_varying: true,
            }
        }
        None => {
            // Vertex: one value per point, refined and limited exactly like
            // the positions.
            let mut values: Vec<[f32; 2]> = (0..points.len())
                .map(|p| c.values[c.value_index(p)])
                .collect();
            for l in 1..=level {
                let mut refined = vec![[0.0f32; 2]; refiner.level(l).num_vertices()];
                primvar.interpolate(l, &values, &mut refined);
                values = refined;
            }
            let mut limit_uvs = vec![[0.0f32; 2]; values.len()];
            primvar.limit(&values, &mut limit_uvs);
            RefinedUvs {
                values: limit_uvs,
                indices: None,
                face_varying: false,
            }
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
        uvs,
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

/// Smooth per-vertex normals for an *unrefined* subdivision cage — what a
/// subdivision surface shades with at refinement level 0, as Hydra's Storm
/// does at low complexity: the cage's silhouette, the surface's shading.
/// `None` for a cage [`subdivide`] would refuse too; it then renders faceted.
pub(crate) fn smooth_cage_normals(
    points: &[Vec3f],
    counts: &[i32],
    indices: &[i32],
) -> Option<Vec<Vec3A>> {
    validate_cage(points.len(), counts, indices).ok()?;
    let verts: Vec<Vec3A> = points.iter().map(|p| Vec3A::new(p.x, p.y, p.z)).collect();
    Some(smooth_normals(&verts, counts, indices))
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
            uvs: None,
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
            assert_eq!(base, Some((child / 4) as u32));
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
        assert!(faces.base_face.iter().all(Option::is_none));
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

    // -------------------------------------------------------------------
    // Memory probe
    // -------------------------------------------------------------------

    /// `System` wrapped in two counters, so the probe below measures
    /// *requested* bytes — deterministic across platforms and allocators,
    /// unlike RSS. Registered for the whole `crust_core` test binary (a
    /// `#[global_allocator]` cannot be scoped tighter), which costs every
    /// other test two relaxed atomics per allocation and changes nothing
    /// else.
    struct CountingAlloc;

    static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static PEAK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    // The crate is `deny(unsafe_code)`; this is the one exception, and it is
    // scoped to the impl rather than to the module so anything else added
    // nearby is still caught. `GlobalAlloc` cannot be implemented safely —
    // that is the trait's contract, not a shortcut taken here — and the whole
    // construct is `#[cfg(test)]`, so no `unsafe` reaches a shipped build.
    #[allow(unsafe_code)]
    unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
        unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
            use std::sync::atomic::Ordering::Relaxed;
            let now = LIVE.fetch_add(layout.size(), Relaxed) + layout.size();
            PEAK.fetch_max(now, Relaxed);
            unsafe { std::alloc::System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
            LIVE.fetch_sub(layout.size(), std::sync::atomic::Ordering::Relaxed);
            unsafe { std::alloc::System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static COUNTING_ALLOC: CountingAlloc = CountingAlloc;

    /// An open N×N quad grid on the z = 0 plane — (N+1)² points, N² quads.
    const ALL_FVAR_RULES: [sdc::FVarLinearInterpolation; 6] = [
        sdc::FVarLinearInterpolation::None,
        sdc::FVarLinearInterpolation::CornersOnly,
        sdc::FVarLinearInterpolation::CornersPlus1,
        sdc::FVarLinearInterpolation::CornersPlus2,
        sdc::FVarLinearInterpolation::Boundaries,
        sdc::FVarLinearInterpolation::All,
    ];

    /// A flat 2×2 quad on z = 0 — one face, sharp corners. Catmull-Clark
    /// reproduces affine data on it, positions and charts alike, so a chart
    /// that is `point / 2` on the cage must still be `point / 2` at every
    /// refined vertex.
    fn flat_quad() -> (Vec<Vec3f>, Vec<i32>, Vec<i32>) {
        let points = vec![
            Vec3f::from([0.0, 0.0, 0.0]),
            Vec3f::from([2.0, 0.0, 0.0]),
            Vec3f::from([2.0, 2.0, 0.0]),
            Vec3f::from([0.0, 2.0, 0.0]),
        ];
        (points, vec![4], vec![0, 1, 2, 3])
    }

    const UNIT_SQUARE: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

    /// The refined chart's value at refined face-vertex `fv`.
    fn uv_at(uvs: &RefinedUvs, fv: usize, point: usize) -> [f32; 2] {
        match &uvs.indices {
            Some(idx) => uvs.values[idx[fv] as usize],
            None => uvs.values[point],
        }
    }

    /// Asserts every refined face-vertex's UV is `f(its point)`.
    fn assert_chart(out: &SubdividedMesh, f: impl Fn(Vec3f) -> [f32; 2], what: &str) {
        let uvs = out.uvs.as_ref().expect("a chart was requested");
        for (fv, &p) in out.indices.iter().enumerate() {
            let got = uv_at(uvs, fv, p as usize);
            let want = f(out.points[p as usize]);
            assert!(
                (got[0] - want[0]).abs() < 1e-5 && (got[1] - want[1]).abs() < 1e-5,
                "{what}: face-vertex {fv} at {:?} has uv {got:?}, expected {want:?}",
                out.points[p as usize]
            );
        }
    }

    #[test]
    fn face_varying_chart_refines_with_the_surface() {
        let (points, counts, indices) = flat_quad();
        for linear in ALL_FVAR_RULES {
            let req = SubdivRequest {
                uvs: Some(UvChannel {
                    values: &UNIT_SQUARE,
                    indices: None,
                    face_varying: true,
                    linear,
                }),
                ..request(2)
            };
            let out = subdivide(&points, &counts, &indices, &req).unwrap();
            let uvs = out.uvs.as_ref().unwrap();
            assert!(uvs.face_varying);
            assert_eq!(uvs.indices.as_ref().unwrap().len(), out.indices.len());
            assert_chart(&out, |p| [p.x / 2.0, p.y / 2.0], &format!("{linear:?}"));
        }
    }

    /// Two quads sharing an edge, charted into two disjoint islands — the
    /// shared edge is a UV seam. Each refined face must read its own island:
    /// the seam vertices carry one value per side, never a blend of both.
    #[test]
    fn face_varying_seam_keeps_each_side_on_its_island() {
        let points = vec![
            Vec3f::from([0.0, 0.0, 0.0]),
            Vec3f::from([1.0, 0.0, 0.0]),
            Vec3f::from([2.0, 0.0, 0.0]),
            Vec3f::from([0.0, 1.0, 0.0]),
            Vec3f::from([1.0, 1.0, 0.0]),
            Vec3f::from([2.0, 1.0, 0.0]),
        ];
        let counts = vec![4, 4];
        let indices = vec![0, 1, 4, 3, 1, 2, 5, 4];
        // Left island u = x, right island u = x + 4 (so [5, 6]).
        let values = [
            [0.0, 0.0],
            [1.0, 0.0],
            [1.0, 1.0],
            [0.0, 1.0],
            [5.0, 0.0],
            [6.0, 0.0],
            [6.0, 1.0],
            [5.0, 1.0],
        ];
        let req = SubdivRequest {
            uvs: Some(UvChannel {
                values: &values,
                indices: None,
                face_varying: true,
                linear: sdc::FVarLinearInterpolation::Boundaries,
            }),
            ..request(2)
        };
        let out = subdivide(&points, &counts, &indices, &req).unwrap();
        let uvs = out.uvs.as_ref().unwrap();
        let mut offset = 0;
        for &n in &out.counts {
            let face = &out.indices[offset..offset + n as usize];
            let centre_x = face.iter().map(|&p| out.points[p as usize].x).sum::<f32>() / n as f32;
            let shift = if centre_x < 1.0 { 0.0 } else { 4.0 };
            for (k, &p) in face.iter().enumerate() {
                let got = uv_at(uvs, offset + k, p as usize);
                let pt = out.points[p as usize];
                assert!(
                    (got[0] - (pt.x + shift)).abs() < 1e-5 && (got[1] - pt.y).abs() < 1e-5,
                    "face at x ~ {centre_x}: {pt:?} has uv {got:?}"
                );
            }
            offset += n as usize;
        }
    }

    #[test]
    fn vertex_chart_refines_like_the_points() {
        let (points, counts, indices) = flat_quad();
        // Authored through `:indices`, reversed, so the indirection is
        // exercised: point p reads values[3 - p].
        let values = [
            UNIT_SQUARE[3],
            UNIT_SQUARE[2],
            UNIT_SQUARE[1],
            UNIT_SQUARE[0],
        ];
        let req = SubdivRequest {
            uvs: Some(UvChannel {
                values: &values,
                indices: Some(&[3, 2, 1, 0]),
                face_varying: false,
                linear: sdc::FVarLinearInterpolation::CornersPlus1,
            }),
            ..request(2)
        };
        let out = subdivide(&points, &counts, &indices, &req).unwrap();
        let uvs = out.uvs.as_ref().unwrap();
        assert!(!uvs.face_varying && uvs.indices.is_none());
        assert_eq!(uvs.values.len(), out.points.len());
        assert_chart(&out, |p| [p.x / 2.0, p.y / 2.0], "vertex");
    }

    #[test]
    fn chart_indices_are_checked_against_the_values() {
        let chart = |indices: Option<&'static [i32]>| UvChannel {
            values: &UNIT_SQUARE,
            indices,
            face_varying: true,
            linear: sdc::FVarLinearInterpolation::All,
        };
        assert!(chart(None).is_well_formed(4));
        assert!(!chart(None).is_well_formed(5), "too few values");
        assert!(chart(Some(&[0, 1, 2, 3])).is_well_formed(4));
        assert!(
            !chart(Some(&[0, 1, 2, 4])).is_well_formed(4),
            "out of range"
        );
        assert!(!chart(Some(&[0, 1, -1, 3])).is_well_formed(4), "negative");
        assert!(
            !chart(Some(&[0, 1, 2])).is_well_formed(4),
            "too few indices"
        );
    }

    /// The face-varying rule is one per refiner, and an authored chart's rule
    /// wins — so the Ptex face table of a UV-charted mesh must come out
    /// bit-identical under every rule, or its lookups would drift off their
    /// sub-face. Five rules get there by sharing the refiner (the channel is
    /// invariant under them); `none`, which smooths face-varying corners,
    /// gets there through its own chartless refinement.
    #[test]
    fn ptex_channel_is_invariant_under_every_fvar_rule() {
        let (points, counts, indices) = cube();
        let reference = subdivide(
            &points,
            &counts,
            &indices,
            &SubdivRequest {
                want_face_uvs: true,
                ..request(2)
            },
        )
        .unwrap()
        .faces
        .unwrap();
        let chart: Vec<[f32; 2]> = (0..6).flat_map(|_| UNIT_SQUARE).collect();
        for linear in ALL_FVAR_RULES {
            let req = SubdivRequest {
                want_face_uvs: true,
                uvs: Some(UvChannel {
                    values: &chart,
                    indices: None,
                    face_varying: true,
                    linear,
                }),
                ..request(2)
            };
            let faces = subdivide(&points, &counts, &indices, &req)
                .unwrap()
                .faces
                .unwrap();
            assert_eq!(faces.base_face, reference.base_face, "{linear:?}");
            let bits = |f: &SubdivFaces| -> Vec<u32> {
                f.corner_uvs
                    .iter()
                    .flatten()
                    .flatten()
                    .map(|c| c.to_bits())
                    .collect()
            };
            assert_eq!(bits(&faces), bits(&reference), "{linear:?}");
        }
    }

    fn quad_grid(n: usize) -> (Vec<Vec3f>, Vec<i32>, Vec<i32>) {
        let mut points = Vec::with_capacity((n + 1) * (n + 1));
        for j in 0..=n {
            for i in 0..=n {
                points.push(Vec3f::from([i as f32, j as f32, 0.0]));
            }
        }
        let mut counts = Vec::with_capacity(n * n);
        let mut indices = Vec::with_capacity(4 * n * n);
        for j in 0..n {
            for i in 0..n {
                let v0 = (j * (n + 1) + i) as i32;
                let v1 = v0 + 1;
                let v2 = v1 + (n + 1) as i32;
                let v3 = v0 + (n + 1) as i32;
                counts.push(4);
                indices.extend_from_slice(&[v0, v1, v2, v3]);
            }
        }
        (points, counts, indices)
    }

    /// What subdivision costs in memory, measured at the `subdivide()`
    /// boundary: `transient` is the peak of live requested bytes while it
    /// runs (dominated by the refiner, which retains every level 0..L —
    /// a ×4/3 geometric series over the last level — plus the position
    /// copies at the tail of the function), `resident` is what the returned
    /// [`SubdividedMesh`] itself holds. The ceilings pin the per-face costs
    /// so a regression (say, an accidentally retained per-level buffer)
    /// fails loudly; they sit ~25% above the values measured at the time of
    /// writing, printed by the table for recalibration.
    ///
    /// Ignored because the counters are process-global: run it alone —
    /// `cargo test -p crust-core --lib subdivision_memory_probe -- --ignored --nocapture --test-threads=1`
    #[test]
    #[ignore = "allocation probe; run alone with --ignored --nocapture --test-threads=1"]
    fn subdivision_memory_probe() {
        use std::sync::atomic::Ordering::Relaxed;

        const GRID: usize = 64; // 4096 cage quads
        let (points, counts, indices) = quad_grid(GRID);
        // A continuous chart, shared across faces the way an unwrapped UV set
        // is: one value per point, addressed per face-vertex.
        let chart_values: Vec<[f32; 2]> = points.iter().map(|p| [p.x, p.y]).collect();
        let chart = UvChannel {
            values: &chart_values,
            indices: Some(&indices),
            face_varying: true,
            linear: sdc::FVarLinearInterpolation::Boundaries,
        };

        println!(
            "\n{:>5} {:>5} {:>14} {:>14} {:>10} {:>10}",
            "level", "uvs", "faces", "transient", "B/face", "resid B/f"
        );
        for level in 1..=4u32 {
            for mode in ["no", "ptex", "chart"] {
                let want_uvs = mode == "ptex";
                let req = SubdivRequest {
                    want_face_uvs: want_uvs,
                    uvs: (mode == "chart").then_some(chart),
                    ..request(level)
                };

                let before = LIVE.load(Relaxed);
                PEAK.store(before, Relaxed);
                let out = subdivide(&points, &counts, &indices, &req).unwrap();
                let peak = PEAK.load(Relaxed);
                let after = LIVE.load(Relaxed);

                let n_faces = out.counts.len();
                let transient = peak - before;
                let resident = after - before;
                let per_face = transient as f64 / n_faces as f64;
                let res_per_face = resident as f64 / n_faces as f64;
                println!(
                    "{:>5} {:>5} {:>14} {:>14} {:>10.1} {:>10.1}",
                    level, mode, n_faces, transient, per_face, res_per_face
                );

                assert_eq!(n_faces, counts.len() * 4usize.pow(level));
                // Ceilings only bind once the per-cage-face constants have
                // amortized away; shallow levels are all fixed overhead.
                if level >= 3 {
                    // Measured on a 64×64 cage: ~313 B/face with no
                    // face-varying channel, ~568 with the Ptex one, ~536 with
                    // a shared UV chart; resident 48.1 / 88.1 / 72.1 (the
                    // Ptex table's 40 B/face: four corner UVs plus an
                    // `Option<u32>` base face). Ceilings ~25% above those,
                    // per mode, so no mode can hide under another's.
                    let (ceiling, resident_ceiling) = match mode {
                        "no" => (400.0, 60.0),
                        "ptex" => (700.0, 105.0),
                        _ => (700.0, 90.0),
                    };
                    assert!(
                        per_face < ceiling,
                        "transient {per_face:.1} B/face at level {level} \
                         (uvs: {mode}) exceeds the {ceiling} B/face ceiling"
                    );
                    assert!(
                        res_per_face < resident_ceiling,
                        "resident {res_per_face:.1} B/face at level {level} \
                         (uvs: {mode}) exceeds the {resident_ceiling} B/face ceiling"
                    );
                }
                drop(out);
            }
        }
    }
}
