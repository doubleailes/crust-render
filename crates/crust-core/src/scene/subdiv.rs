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

    // Everything that needed the refiner is extracted; drop it — every
    // level's topology, a ×4/3 of the last — before the result's own copies
    // of the last level are made, so the two never coexist. This is the
    // third of the transient the kernel design record costed. (`primvar`
    // only borrows it; its last use is above.)
    drop(refiner);

    let normals = smooth_normals(&limit, &out_counts, &out_indices);
    let points: Vec<Vec3f> = limit
        .into_iter()
        .map(|p| Vec3f::from([p[0], p[1], p[2]]))
        .collect();

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

// ---------------------------------------------------------------------------
// Per-face adaptive tessellation
// ---------------------------------------------------------------------------

/// Feature-adaptive isolation depth of per-face tessellation's patch table.
///
/// Shallow on purpose. Under Catmull-Clark an all-triangle cage is irregular
/// everywhere (every split triangle's centre is a valence-3 vertex), so
/// isolation at depth `d` refines every face `d` times: at 3 the Moana ocean's
/// 684 416-triangle cage cost a 19.9 GiB transient. Gregory patches cover what
/// isolation leaves irregular. Measured on the all-extraordinary cube (edges of
/// length 2): exact at sampling depths up to the isolation depth, and within
/// 0.0185 of the uniform limit (0.9% of an edge) at a rate of 4, next to an
/// extraordinary vertex only; regular faces are exact B-spline patches at any
/// depth.
const ADAPTIVE_ISOLATION: usize = 1;

/// Per triangle of a per-face tessellation: the cage face Ptex addresses and
/// the triangle's corners in that face's unit square. `None` for a triangle of
/// an `n`-gon, which Ptex does not address here (as for uniform refinement).
pub(crate) struct TessellatedFaces {
    pub base_face: Vec<Option<u32>>,
    pub corner_uvs: Vec<[[f32; 2]; 3]>,
}

/// What [`tessellate_adaptive`] needs to size one segment of the cage: the two
/// points that span it — for a cage edge its two cage vertices (the edges are
/// rated before any patch exists, so there is no limit point yet), for a spoke
/// of a refined `n`-gon its two limit end points — and it answers the segment's
/// projected length over the target (`ℓ · σ / t`). The distance and the frustum
/// test use the box around exactly these points; `ScreenRate::segment_at` pads
/// it by its own diagonal before culling, so a limit curve straying off its
/// chord is not culled at the frustum's edge.
pub(crate) type SegmentSize<'a> = dyn Fn(&[[f32; 3]]) -> f32 + 'a;

/// A limit-surface tessellation, in [`SubdividedMesh`]'s shapes (all
/// triangles), with Ptex corners as [`TessellatedFaces`].
pub(crate) struct TessellatedMesh {
    pub points: Vec<Vec3f>,
    pub indices: Vec<i32>,
    pub normals: Vec<[f32; 3]>,
    pub faces: Option<TessellatedFaces>,
    /// A `vertex` chart evaluated at every vertex.
    pub uvs: Option<Vec<[f32; 2]>>,
    /// A `faceVarying` chart evaluated per Ptex face, so each side of a seam
    /// keeps its own values: `values`, and per triangle corner (parallel to
    /// `indices`) an index into them.
    pub face_varying_uvs: Option<(Vec<[f32; 2]>, Vec<i32>)>,
    /// The smallest and largest edge rate used, for the debug line.
    pub rate_range: (u32, u32),
    /// Cage edges and spokes by rate, binned by `ceil(log2(rate))`: 1, 2,
    /// 3–4, 5–8, …
    pub rate_bins: Vec<u64>,
    /// Ptex faces tessellated.
    pub ptex_faces: usize,
    /// The shape of the selected faces' triangles: [`TriangleQuality`] bins
    /// for the interior grids' and for the stitched rings'.
    pub quality: [[u64; QUALITY_BINS]; 2],
}

/// Bins of [`triangle_quality`]: `[0.9, 1]`, `[0.5, 0.9)`, `[0.1, 0.5)`,
/// `[0.01, 0.1)`, `[0, 0.01)`.
pub(crate) const QUALITY_BINS: usize = 5;

/// A triangle's shape, `4√3 · area / Σ edge²`: 1 for an equilateral
/// triangle, toward 0 for a sliver.
pub(crate) fn triangle_quality(a: Vec3A, b: Vec3A, c: Vec3A) -> f32 {
    let area2 = (b - a).cross(c - a).length(); // twice the area
    let sum = (b - a).length_squared() + (c - b).length_squared() + (a - c).length_squared();
    if sum <= 0.0 {
        return 0.0;
    }
    (2.0 * 3f32.sqrt() * area2 / sum).clamp(0.0, 1.0)
}

/// The [`QUALITY_BINS`] bin of a quality.
pub(crate) fn quality_bin(q: f32) -> usize {
    match q {
        q if q >= 0.9 => 0,
        q if q >= 0.5 => 1,
        q if q >= 0.1 => 2,
        q if q >= 0.01 => 3,
        _ => 4,
    }
}

/// A point on an unrefined face's boundary: its vertex, its Ptex coordinate and its
/// face-varying chart value.
type RingPoint = (u32, [f32; 2], [f32; 2]);

/// Who owns a vertex of the tessellation, so the faces that meet there share it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum VertexKey {
    Cage(u32),
    /// Point `i` of cage edge `edge`, counted from its lower vertex.
    Edge(u32, u32),
    /// The centre of `n`-gon `face`.
    Centre(u32),
    /// Point `i` of spoke `k` of `n`-gon `face`, counted from the edge midpoint.
    Spoke(u32, u32, u32),
}

/// Tessellates a Catmull-Clark or bilinear cage per Ptex face.
///
/// Every cage edge is rated from its two cage vertices (`segment_size`, then
/// [`tessellate::edge_rate`] under `max_level`). A face with an edge rated above
/// 1 is *selected*: only selected faces get limit patches
/// (`refine_adaptive_selected`, `create_with_options_selected`), and each of
/// their Ptex quads is gridded and stitched to its edges
/// ([`tessellate::tessellate_quad`]) on the limit surface. Every other face
/// renders its cage, smooth-shaded, as level 0 does — what MoonRay does with a
/// face whose tessellation factor is 0 — so the cost grows with the refined
/// area, not the cage. A cage vertex a selected face touches takes that face's
/// limit point, and every corner and edge point is evaluated once and shared,
/// so a selected and an unselected face meet without a crack.
///
/// `req.level` is ignored. A `vertex` chart is evaluated with the positions'
/// basis; a `faceVarying` one with the patch table's face-varying patches,
/// under its `faceVaryingLinearInterpolation` (linear across an unselected
/// face).
pub(crate) fn tessellate_adaptive(
    points: &[Vec3f],
    counts: &[i32],
    indices: &[i32],
    req: &SubdivRequest,
    max_level: u32,
    segment_size: &SegmentSize<'_>,
) -> Result<TessellatedMesh, SubdivError> {
    use super::tessellate::{PointKey, edge_rate, tessellate_quad};
    use opensubdiv_rs::far::{AdaptiveOptions, PatchMap, PatchTableFactory, PatchTableOptions};
    use std::collections::HashMap;

    debug_assert!(
        req.scheme != SubdivScheme::Loop,
        "Loop is the caller's to route"
    );
    let (counts_us, indices_u32) = validate_cage(points.len(), counts, indices)?;
    let (crease_pairs, crease_weights) = expand_crease_runs(
        req.crease_indices,
        req.crease_lengths,
        req.crease_sharpnesses,
    )?;
    let corners = validate_corners(req.corner_indices, req.corner_sharpnesses)?;
    let scheme = match req.scheme {
        SubdivScheme::Bilinear => sdc::SchemeType::Bilinear,
        _ => sdc::SchemeType::Catmark,
    };
    let chart = req.uvs.filter(|c| c.face_varying);
    let vertex_chart = req.uvs.filter(|c| !c.face_varying);

    // Faces, and the cage edges.
    let n_faces = counts_us.len();
    let mut starts = Vec::with_capacity(n_faces);
    let mut at = 0usize;
    for &n in &counts_us {
        starts.push(at);
        at += n;
    }
    let face_verts = |f: usize| &indices_u32[starts[f]..starts[f] + counts_us[f]];
    let mut edge_ids: HashMap<(u32, u32), u32> = HashMap::new();
    let mut edges: Vec<(u32, u32)> = Vec::new();
    let mut face_edges: Vec<u32> = Vec::with_capacity(indices_u32.len());
    for f in 0..n_faces {
        let fv = face_verts(f);
        for k in 0..fv.len() {
            let (a, b) = (fv[k], fv[(k + 1) % fv.len()]);
            let key = (a.min(b), a.max(b));
            let id = *edge_ids.entry(key).or_insert_with(|| {
                edges.push(key);
                (edges.len() - 1) as u32
            });
            face_edges.push(id);
        }
    }
    let edges_of = |f: usize| &face_edges[starts[f]..starts[f] + counts_us[f]];

    // Rates from the cage, then the selection: faces with a finer edge.
    let base_rates: Vec<u32> = edges
        .iter()
        .map(|&(a, b)| {
            let (pa, pb) = (base_point(points, a), base_point(points, b));
            edge_rate(segment_size(&[pa, pb]), max_level, false)
        })
        .collect();
    let selected: Vec<bool> = (0..n_faces)
        .map(|f| edges_of(f).iter().any(|&e| base_rates[e as usize] > 1))
        .collect();
    // A selected `n`-gon's Ptex quads split its edges at their midpoints, so
    // those edges take an even rate — on both sides.
    let mut edge_rates = base_rates.clone();
    for f in (0..n_faces).filter(|&f| selected[f] && counts_us[f] != 4) {
        for &e in edges_of(f) {
            let r = &mut edge_rates[e as usize];
            *r = (*r).max(2).next_multiple_of(2);
        }
    }
    let rate_of = |a: u32, b: u32| {
        let id = edge_ids[&(a.min(b), a.max(b))];
        (id, edge_rates[id as usize])
    };
    let canonical = |a: u32, b: u32, i: u32, n: u32| if a < b { i } else { n - i };

    let mut rate_bins: Vec<u64> = Vec::new();
    let mut bin = |r: u32| {
        let b = (32 - (r.max(1) - 1).leading_zeros()) as usize;
        if rate_bins.len() <= b {
            rate_bins.resize(b + 1, 0);
        }
        rate_bins[b] += 1;
    };
    for &r in &edge_rates {
        bin(r);
    }
    let selected_faces: Vec<u32> = (0..n_faces as u32)
        .filter(|&f| selected[f as usize])
        .collect();
    let ptex_faces: usize = counts_us.iter().map(|&n| if n == 4 { 1 } else { n }).sum();

    let mut vertex_of: HashMap<VertexKey, u32> = HashMap::new();
    let mut out_points: Vec<Vec3f> = Vec::new();
    let mut out_normals: Vec<[f32; 3]> = Vec::new();
    let mut out_uvs: Vec<[f32; 2]> = Vec::new();
    let mut out_indices: Vec<i32> = Vec::new();
    let mut base_face: Vec<Option<u32>> = Vec::new();
    let mut corner_uvs: Vec<[[f32; 2]; 3]> = Vec::new();
    let mut fv_values: Vec<[f32; 2]> = Vec::new();
    let mut fv_indices: Vec<i32> = Vec::new();
    // The face-varying limit value a refined face gave each boundary point, per
    // chart side: a corner keyed by its cage vertex and chart value, an edge
    // point by its key and the chart values at the edge's ends. An unrefined
    // neighbour on the same side takes it, so the chart is continuous where the
    // two meet.
    let mut fvar_at: HashMap<(VertexKey, u32, u32), [f32; 2]> = HashMap::new();
    let chart_value =
        |f: usize, k: usize| -> u32 { chart.map_or(0, |c| c.value_index(starts[f] + k) as u32) };
    let side = |a: u32, b: u32| (a.min(b), a.max(b));
    let (mut min_rate, mut max_rate) = (u32::MAX, 0u32);
    let mut quality = [[0u64; QUALITY_BINS]; 2];

    // --- Selected faces, on the limit surface -----------------------------
    if !selected_faces.is_empty() {
        let options = sdc::Options::default()
            .with_vtx_boundary_interpolation(req.boundary)
            .with_fvar_linear_interpolation(
                chart.map_or(sdc::FVarLinearInterpolation::All, |c| c.linear),
            );
        let chart_indices: Vec<u32> = match chart {
            Some(c) => (0..indices.len())
                .map(|fv| c.value_index(fv) as u32)
                .collect(),
            None => Vec::new(),
        };
        let channels: Vec<FVarChannelDescriptor> = chart
            .map(|c| FVarChannelDescriptor::new(c.values.len(), &chart_indices))
            .into_iter()
            .collect();
        let mut descriptor = TopologyDescriptor::new(points.len(), &counts_us, &indices_u32)
            .with_creases(&crease_pairs, &crease_weights)
            .with_corners(&corners.0, &corners.1);
        if !channels.is_empty() {
            descriptor = descriptor.with_fvar_channels(&channels);
        }
        let mut refiner = TopologyRefinerFactory::create(descriptor, scheme, options)
            .map_err(SubdivError::Refine)?;
        // A face regular in the vertex topology can be irregular in the
        // chart's: isolate it too rather than capping it a level up.
        let mut adaptive =
            AdaptiveOptions::new(ADAPTIVE_ISOLATION).with_consider_fvar_channels(chart.is_some());
        adaptive.use_single_crease_patch = true;
        refiner.refine_adaptive_selected(adaptive, &selected_faces);
        // Smooth face-varying patches that follow the chart's own topology
        // and rule, not OpenSubdiv's legacy linear ones.
        let table_options = PatchTableOptions::new()
            .with_fvar_tables(chart.is_some())
            .with_fvar_legacy_linear_patches(false);
        let table = PatchTableFactory::create_with_options_selected(
            &refiner,
            &table_options,
            &selected_faces,
        )
        .map_err(SubdivError::Refine)?;
        let map = PatchMap::new(&table);
        let ptex_of = table.ptex_indices();

        // Control values: every level's vertices, base first.
        let primvar = PrimvarRefiner::new(&refiner);
        let base: Vec<[f32; 3]> = points.iter().map(|p| [p.x, p.y, p.z]).collect();
        let mut control = base.clone();
        let mut level_vals = base;
        for l in 1..=refiner.max_level() {
            let mut refined = vec![[0.0f32; 3]; refiner.level(l).num_vertices()];
            primvar.interpolate(l, &level_vals, &mut refined);
            control.extend_from_slice(&refined);
            level_vals = refined;
        }
        let fvar_values: Option<Vec<[f32; 2]>> = chart.map(|c| {
            let mut values = c.values.to_vec();
            for level in primvar.interpolate_face_varying_all(0, c.values) {
                values.extend_from_slice(&level);
            }
            values
        });
        let uv_control: Option<Vec<[f32; 2]>> = vertex_chart.map(|c| {
            let mut control: Vec<[f32; 2]> = (0..points.len())
                .map(|v| c.values[c.value_index(v)])
                .collect();
            let mut level_vals = control.clone();
            for l in 1..=refiner.max_level() {
                let mut refined = vec![[0.0f32; 2]; refiner.level(l).num_vertices()];
                primvar.interpolate(l, &level_vals, &mut refined);
                control.extend_from_slice(&refined);
                level_vals = refined;
            }
            control
        });
        let eval = |ptex: usize, u: f32, v: f32| -> Option<([f32; 3], [f32; 3])> {
            let patch = map.find_patch(ptex, u, v)?;
            let (p, du, dv) = table.evaluate(patch, u, v, &control);
            let n = Vec3A::from(du).cross(Vec3A::from(dv));
            let n = if n.length_squared() > 1e-24 {
                n.normalize()
            } else {
                // A degenerate parameterization (a pole): the normal a hair inside.
                let (u2, v2) = (u + (0.5 - u) * 1e-3, v + (0.5 - v) * 1e-3);
                let patch = map.find_patch(ptex, u2, v2)?;
                let (_, du, dv) = table.evaluate(patch, u2, v2, &control);
                Vec3A::from(du).cross(Vec3A::from(dv)).normalize_or_zero()
            };
            Some((p, n.to_array()))
        };
        let eval_fvar = |ptex: usize, u: f32, v: f32| -> Option<[f32; 2]> {
            let values = fvar_values.as_ref()?;
            let patch = map.find_patch(ptex, u, v)?;
            Some(table.evaluate_face_varying(patch, u, v, values, 0).0)
        };
        let eval_uv = |ptex: usize, u: f32, v: f32| -> Option<[f32; 2]> {
            let control = uv_control.as_ref()?;
            let patch = map.find_patch(ptex, u, v)?;
            Some(table.evaluate(patch, u, v, control).0)
        };
        let mut emit = |key: Option<VertexKey>,
                        ptex: usize,
                        uv: [f32; 2],
                        out_points: &mut Vec<Vec3f>,
                        out_normals: &mut Vec<[f32; 3]>,
                        out_uvs: &mut Vec<[f32; 2]>|
         -> Result<u32, SubdivError> {
            if let Some(key) = key
                && let Some(&v) = vertex_of.get(&key)
            {
                return Ok(v);
            }
            let (p, n) = eval(ptex, uv[0], uv[1]).ok_or_else(|| {
                SubdivError::BadTopology(format!("no limit patch under Ptex face {ptex} at {uv:?}"))
            })?;
            let v = out_points.len() as u32;
            out_points.push(Vec3f {
                x: p[0],
                y: p[1],
                z: p[2],
            });
            out_normals.push(n);
            if uv_control.is_some() {
                out_uvs.push(eval_uv(ptex, uv[0], uv[1]).unwrap_or([0.0, 0.0]));
            }
            if let Some(key) = key {
                vertex_of.insert(key, v);
            }
            Ok(v)
        };

        for &f in &selected_faces {
            let f = f as usize;
            let fv = face_verts(f);
            let first = ptex_of.face_id(f) as usize;
            let n = fv.len();
            let quads: usize = if n == 4 { 1 } else { n };
            // Spoke rates of an `n`-gon: from the limit midpoint of edge k to
            // the limit centre.
            let spoke_rates: Vec<u32> = if n == 4 {
                Vec::new()
            } else {
                let centre = eval(first, 1.0, 1.0).map(|e| e.0);
                (0..n)
                    .map(|k| {
                        let mid = eval(first + k, 1.0, 0.0).map(|e| e.0);
                        match (mid, centre) {
                            (Some(m), Some(c)) => {
                                edge_rate(segment_size(&[m, c]), max_level, false)
                            }
                            _ => 1,
                        }
                    })
                    .collect()
            };
            for &r in &spoke_rates {
                bin(r);
            }
            for k in 0..quads {
                let ptex = first + k;
                let mut rates = [0u32; 4];
                let mut edge_key: [Box<dyn Fn(u32) -> VertexKey>; 4] = std::array::from_fn(|_| {
                    Box::new(|_| VertexKey::Cage(0)) as Box<dyn Fn(u32) -> VertexKey>
                });
                let corner_key: [VertexKey; 4] = if n == 4 {
                    for e in 0..4 {
                        let (a, b) = (fv[e], fv[(e + 1) % 4]);
                        let (id, r) = rate_of(a, b);
                        rates[e] = r;
                        edge_key[e] = Box::new(move |i| VertexKey::Edge(id, canonical(a, b, i, r)));
                    }
                    [0, 1, 2, 3].map(|c| VertexKey::Cage(fv[c]))
                } else {
                    let (vk, vnext, vprev) = (fv[k], fv[(k + 1) % n], fv[(k + n - 1) % n]);
                    let (e_next, r_next) = rate_of(vk, vnext);
                    let (e_prev, r_prev) = rate_of(vprev, vk);
                    let (s_k, s_prev) = (spoke_rates[k], spoke_rates[(k + n - 1) % n]);
                    let (fi, ki, kp) = (f as u32, k as u32, ((k + n - 1) % n) as u32);
                    rates = [r_next / 2, s_k, s_prev, r_prev / 2];
                    edge_key[0] =
                        Box::new(move |i| VertexKey::Edge(e_next, canonical(vk, vnext, i, r_next)));
                    edge_key[1] = Box::new(move |i| VertexKey::Spoke(fi, ki, i));
                    edge_key[2] = Box::new(move |i| VertexKey::Spoke(fi, kp, s_prev - i));
                    // From the midpoint toward vk: `half − i` segments from vk.
                    let half = r_prev / 2;
                    edge_key[3] = Box::new(move |i| {
                        let from_vk = half - i;
                        VertexKey::Edge(e_prev, canonical(vk, vprev, from_vk, r_prev))
                    });
                    [
                        VertexKey::Cage(vk),
                        VertexKey::Edge(e_next, r_next / 2),
                        VertexKey::Centre(fi),
                        VertexKey::Edge(e_prev, r_prev / 2),
                    ]
                };
                for &r in &rates {
                    min_rate = min_rate.min(r);
                    max_rate = max_rate.max(r);
                }
                let t = tessellate_quad(rates);
                let mut local = Vec::with_capacity(t.points.len());
                for (uv, key) in t.points.iter().zip(&t.keys) {
                    let key = match *key {
                        PointKey::Corner(c) => Some(corner_key[c as usize]),
                        PointKey::Edge { edge, i } => Some(edge_key[edge as usize](i)),
                        PointKey::Interior => None,
                    };
                    local.push(emit(
                        key,
                        ptex,
                        *uv,
                        &mut out_points,
                        &mut out_normals,
                        &mut out_uvs,
                    )?);
                }
                // The chart once per point of this Ptex face: a seam vertex
                // shared with a face on the chart's other side takes this
                // side's value.
                let fv_first = fv_values.len() as i32;
                if fvar_values.is_some() {
                    // The chart values at this Ptex quad's corners and along its
                    // cage-edge sides, for `fvar_at`.
                    type Side = Option<(u32, u32)>;
                    let (corner_side, edge_side): ([Side; 4], [Side; 4]) = if n == 4 {
                        let v = |c: usize| chart_value(f, c);
                        (
                            [0, 1, 2, 3].map(|c| Some((v(c), v(c)))),
                            [0, 1, 2, 3].map(|e| Some(side(v(e), v((e + 1) % 4)))),
                        )
                    } else {
                        let vk = chart_value(f, k);
                        let vn = chart_value(f, (k + 1) % n);
                        let vp = chart_value(f, (k + n - 1) % n);
                        (
                            [Some((vk, vk)), Some(side(vk, vn)), None, Some(side(vp, vk))],
                            [Some(side(vk, vn)), None, None, Some(side(vp, vk))],
                        )
                    };
                    for (uv, key) in t.points.iter().zip(&t.keys) {
                        let value = eval_fvar(ptex, uv[0], uv[1]).unwrap_or([0.0, 0.0]);
                        fv_values.push(value);
                        let record = match *key {
                            PointKey::Corner(c) => {
                                corner_side[c as usize].map(|sd| (corner_key[c as usize], sd))
                            }
                            PointKey::Edge { edge, i } => {
                                edge_side[edge as usize].map(|sd| (edge_key[edge as usize](i), sd))
                            }
                            PointKey::Interior => None,
                        };
                        if let Some((key, (a, b))) = record {
                            fvar_at.entry((key, a, b)).or_insert(value);
                        }
                    }
                }
                for (k, tri) in t.tris.iter().enumerate() {
                    let [a, b, c] = tri.map(|c| {
                        let p = out_points[local[c as usize] as usize];
                        Vec3A::new(p.x, p.y, p.z)
                    });
                    quality[usize::from(k >= t.stitched_from)]
                        [quality_bin(triangle_quality(a, b, c))] += 1;
                    for &c in tri {
                        out_indices.push(local[c as usize] as i32);
                        if fvar_values.is_some() {
                            fv_indices.push(fv_first + c as i32);
                        }
                    }
                    base_face.push((n == 4).then_some(f as u32));
                    corner_uvs.push(tri.map(|c| t.points[c as usize]));
                }
            }
        }
    }

    // --- Unselected faces: the smooth cage --------------------------------
    if selected_faces.len() < n_faces {
        let cage_normals = smooth_cage_normals(points, counts, indices)
            .ok_or_else(|| SubdivError::BadTopology("cage normals".into()))?;
        let corner_param = [[0.0f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        for f in (0..n_faces).filter(|&f| !selected[f]) {
            let fv = face_verts(f);
            let n = fv.len();
            // The face's boundary, corner by corner and along each edge's
            // points, with its Ptex coordinate (quads) and chart value.
            let mut ring: Vec<RingPoint> = Vec::new();
            let chart_at = |k: usize| -> [f32; 2] {
                chart.map_or([0.0, 0.0], |c| c.values[c.value_index(starts[f] + k)])
            };
            for k in 0..n {
                let (a, b) = (fv[k], fv[(k + 1) % n]);
                let v = match vertex_of.get(&VertexKey::Cage(a)) {
                    Some(&v) => v,
                    None => {
                        let v = out_points.len() as u32;
                        out_points.push(points[a as usize]);
                        out_normals.push(cage_normals[a as usize]);
                        if let Some(c) = vertex_chart {
                            out_uvs.push(c.values[c.value_index(a as usize)]);
                        }
                        vertex_of.insert(VertexKey::Cage(a), v);
                        v
                    }
                };
                let (pa, pb) = if n == 4 {
                    (corner_param[k], corner_param[(k + 1) % 4])
                } else {
                    ([0.0, 0.0], [0.0, 0.0])
                };
                let (ia, ib) = (chart_value(f, k), chart_value(f, (k + 1) % n));
                let (ca, cb) = (chart_at(k), chart_at((k + 1) % n));
                let ca_limit = fvar_at
                    .get(&(VertexKey::Cage(a), ia, ia))
                    .copied()
                    .unwrap_or(ca);
                ring.push((v, pa, ca_limit));
                // An edge point here was placed by a selected neighbour (a
                // midpoint its `n`-gon forced): use it, or the faces would
                // meet at a T-junction.
                let (id, r) = rate_of(a, b);
                for i in 1..r {
                    let s = i as f32 / r as f32;
                    let key = VertexKey::Edge(id, canonical(a, b, i, r));
                    let v = match vertex_of.get(&key) {
                        Some(&v) => v,
                        None => {
                            // Not reached: an edge above rate 1 has a selected
                            // face. Placed on the cage edge all the same.
                            let (p, q) = (points[a as usize], points[b as usize]);
                            let v = out_points.len() as u32;
                            out_points.push(Vec3f {
                                x: p.x + (q.x - p.x) * s,
                                y: p.y + (q.y - p.y) * s,
                                z: p.z + (q.z - p.z) * s,
                            });
                            let (na, nb) = (cage_normals[a as usize], cage_normals[b as usize]);
                            out_normals.push(
                                Vec3A::from(na)
                                    .lerp(Vec3A::from(nb), s)
                                    .normalize_or_zero()
                                    .to_array(),
                            );
                            if let Some(c) = vertex_chart {
                                let (ua, ub) = (
                                    c.values[c.value_index(a as usize)],
                                    c.values[c.value_index(b as usize)],
                                );
                                out_uvs.push([
                                    ua[0] + (ub[0] - ua[0]) * s,
                                    ua[1] + (ub[1] - ua[1]) * s,
                                ]);
                            }
                            vertex_of.insert(key, v);
                            v
                        }
                    };
                    let lerp2 = |x: [f32; 2], y: [f32; 2]| {
                        [x[0] + (y[0] - x[0]) * s, x[1] + (y[1] - x[1]) * s]
                    };
                    let (sa, sb) = side(ia, ib);
                    let value = fvar_at
                        .get(&(key, sa, sb))
                        .copied()
                        .unwrap_or_else(|| lerp2(ca, cb));
                    ring.push((v, lerp2(pa, pb), value));
                }
            }
            min_rate = min_rate.min(1);
            max_rate = max_rate.max(1);
            let mut push_tri = |tri: [&RingPoint; 3],
                                out_indices: &mut Vec<i32>,
                                fv_values: &mut Vec<[f32; 2]>,
                                fv_indices: &mut Vec<i32>| {
                for c in tri {
                    out_indices.push(c.0 as i32);
                    if chart.is_some() {
                        fv_indices.push(fv_values.len() as i32);
                        fv_values.push(c.2);
                    }
                }
                base_face.push((n == 4).then_some(f as u32));
                corner_uvs.push(tri.map(|c| c.1));
            };
            if ring.len() == n {
                // The cage polygon, fanned from its first corner as the
                // importer triangulates.
                for k in 1..n - 1 {
                    push_tri(
                        [&ring[0], &ring[k], &ring[k + 1]],
                        &mut out_indices,
                        &mut fv_values,
                        &mut fv_indices,
                    );
                }
            } else {
                // Edge points on the boundary: fan from the cage centroid.
                let m = ring.len() as f32;
                let centre_p = fv.iter().fold(Vec3A::ZERO, |acc, &v| {
                    let p = points[v as usize];
                    acc + Vec3A::new(p.x, p.y, p.z)
                }) / n as f32;
                let centre_n = fv
                    .iter()
                    .fold(Vec3A::ZERO, |acc, &v| {
                        acc + Vec3A::from(cage_normals[v as usize])
                    })
                    .normalize_or_zero();
                let c = out_points.len() as u32;
                out_points.push(Vec3f {
                    x: centre_p.x,
                    y: centre_p.y,
                    z: centre_p.z,
                });
                out_normals.push(centre_n.to_array());
                if let Some(ch) = vertex_chart {
                    let mut uv = [0.0f32, 0.0];
                    for &v in fv {
                        let x = ch.values[ch.value_index(v as usize)];
                        uv[0] += x[0] / n as f32;
                        uv[1] += x[1] / n as f32;
                    }
                    out_uvs.push(uv);
                }
                let avg = |pick: fn(&RingPoint) -> [f32; 2]| {
                    let mut a = [0.0f32, 0.0];
                    for r in &ring {
                        let x = pick(r);
                        a[0] += x[0] / m;
                        a[1] += x[1] / m;
                    }
                    a
                };
                let centre = (
                    c,
                    if n == 4 { [0.5, 0.5] } else { [0.0, 0.0] },
                    avg(|r| r.2),
                );
                for k in 0..ring.len() {
                    let next = &ring[(k + 1) % ring.len()];
                    push_tri(
                        [&centre, &ring[k], next],
                        &mut out_indices,
                        &mut fv_values,
                        &mut fv_indices,
                    );
                }
            }
        }
    }

    Ok(TessellatedMesh {
        points: out_points,
        indices: out_indices,
        normals: out_normals,
        faces: req.want_face_uvs.then_some(TessellatedFaces {
            base_face,
            corner_uvs,
        }),
        uvs: vertex_chart.is_some().then_some(out_uvs),
        face_varying_uvs: chart.is_some().then_some((fv_values, fv_indices)),
        rate_range: (min_rate.min(max_rate), max_rate),
        rate_bins,
        ptex_faces,
        quality,
    })
}

fn base_point(points: &[Vec3f], v: u32) -> [f32; 3] {
    let p = points[v as usize];
    [p.x, p.y, p.z]
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

    // --- Per-face adaptive tessellation -----------------------------------

    /// Every undirected edge of a closed tessellation, used once each way.
    fn assert_closed_and_consistent(indices: &[i32], what: &str) {
        let mut directed = std::collections::HashMap::<(i32, i32), u32>::new();
        for t in indices.chunks(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *directed.entry((a, b)).or_default() += 1;
            }
        }
        for (&(a, b), &n) in &directed {
            assert_eq!(n, 1, "{what}: edge {a}->{b} used {n} times");
            assert!(
                directed.contains_key(&(b, a)),
                "{what}: edge {a}->{b} has no twin: a crack or a winding flip"
            );
        }
    }

    /// A segment-size closure giving every segment `rate` segments, whatever
    /// its length (`rate − 0.5` rounds up to `rate`).
    fn constant(rate: u32) -> impl Fn(&[[f32; 3]]) -> f32 {
        move |_| rate as f32 - 0.5
    }

    /// Rates that vary across the mesh, from each segment's first point: 1 to
    /// 7 segments, so neighbouring faces disagree on their other edges.
    fn mixed(p: &[[f32; 3]]) -> f32 {
        let h = (p[0][0] * 3.1 + p[0][1] * 1.7 + p[0][2] * 2.3).abs();
        (h * 10.0) % 7.0 + 0.5
    }

    /// A pentagonal prism: two pentagons and five quads, closed.
    fn prism() -> (Vec<Vec3f>, Vec<i32>, Vec<i32>) {
        let mut points = Vec::new();
        for z in [-1.0f32, 1.0] {
            for k in 0..5 {
                let a = k as f32 * std::f32::consts::TAU / 5.0;
                points.push(Vec3f::from([a.cos(), a.sin(), z]));
            }
        }
        let mut counts = vec![5, 5];
        let mut indices = vec![4, 3, 2, 1, 0, 5, 6, 7, 8, 9];
        for k in 0..5 {
            let k1 = (k + 1) % 5;
            counts.push(4);
            indices.extend_from_slice(&[k, k1, 5 + k1, 5 + k]);
        }
        (points, counts, indices)
    }

    #[test]
    fn a_closed_cage_tessellates_closed_at_mixed_rates() {
        for (name, (points, counts, indices)) in [("cube", cube()), ("prism", prism())] {
            for max in [1u32, 2, 3] {
                let t = tessellate_adaptive(&points, &counts, &indices, &request(0), max, &mixed)
                    .unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_closed_and_consistent(&t.indices, &format!("{name} at max {max}"));
                if max == 3 {
                    assert!(t.rate_range.0 < t.rate_range.1, "{name}: rates should vary");
                }
            }
            let t = tessellate_adaptive(&points, &counts, &indices, &request(0), 3, &constant(1))
                .unwrap();
            assert_closed_and_consistent(&t.indices, &format!("{name} at rate 1"));
        }
    }

    /// A face whose every edge is split once is not refined at all: it renders
    /// its cage, smooth-shaded, as level 0 does — and no patch is built for it.
    #[test]
    fn rate_one_faces_render_their_smooth_cage() {
        let (points, counts, indices) = cube();
        let t =
            tessellate_adaptive(&points, &counts, &indices, &request(0), 3, &constant(1)).unwrap();
        assert_eq!(t.points.len(), 8, "one vertex per cage corner");
        assert_eq!(t.indices.len(), 6 * 2 * 3);
        // The cage's own positions and level 0's smooth normals, in whatever
        // order the faces emitted them.
        let smooth = smooth_cage_normals(&points, &counts, &indices).unwrap();
        for (p, n) in t.points.iter().zip(&t.normals) {
            let k = points.iter().position(|q| q == p).expect("a cage position");
            assert_eq!(*n, smooth[k], "the smooth cage normal at {p:?}");
        }
    }

    /// The limit points of uniform level `level`, as a list.
    fn uniform_points(
        points: &[Vec3f],
        counts: &[i32],
        indices: &[i32],
        level: u32,
    ) -> Vec<[f32; 3]> {
        let m = subdivide(points, counts, indices, &request(level)).unwrap();
        m.points.iter().map(|p| [p.x, p.y, p.z]).collect()
    }

    /// For each point, the distance to the nearest of `to`.
    fn worst_distance(from: &[Vec3f], to: &[[f32; 3]]) -> f32 {
        from.iter()
            .map(|p| {
                to.iter()
                    .map(|q| Vec3A::new(p.x - q[0], p.y - q[1], p.z - q[2]).length())
                    .fold(f32::MAX, f32::min)
            })
            .fold(0.0, f32::max)
    }

    /// Where a refined face meets a face left at its cage, the surface stays
    /// closed: the shared corners take the refined face's limit points, and an
    /// edge point a refined `n`-gon forced is used by its unrefined neighbour.
    #[test]
    fn refined_and_cage_faces_meet_closed() {
        // Only edges touching the +X side are rated above 1.
        let near_x = |p: &[[f32; 3]]| {
            if p[0][0] > 0.5 && p[1][0] > 0.5 {
                5.5
            } else {
                0.5
            }
        };
        for (name, (points, counts, indices)) in [("cube", cube()), ("prism", prism())] {
            let t = tessellate_adaptive(&points, &counts, &indices, &request(0), 3, &near_x)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_closed_and_consistent(&t.indices, &format!("{name}, partly refined"));
            assert!(t.rate_range.1 > 1, "{name}: some face is refined");
            let all = tessellate_adaptive(&points, &counts, &indices, &request(0), 3, &constant(6))
                .unwrap();
            assert!(
                t.indices.len() < all.indices.len(),
                "{name}: the faces left at their cage cost fewer triangles"
            );
        }
    }

    #[test]
    fn a_uniform_rate_is_uniform_refinement_on_regular_faces() {
        // A 6×6 grid of quads with a bump: interior faces are regular.
        let g = 6;
        let mut points = Vec::new();
        for j in 0..=g {
            for i in 0..=g {
                let (x, y) = (i as f32, j as f32);
                points.push(Vec3f::from([
                    x,
                    y,
                    ((x * 0.9).sin() * (y * 0.7).cos()) * 0.5,
                ]));
            }
        }
        let mut counts = Vec::new();
        let mut indices = Vec::new();
        for j in 0..g {
            for i in 0..g {
                let a = j * (g + 1) + i;
                counts.push(4);
                indices.extend_from_slice(&[a, a + 1, a + g + 2, a + g + 1]);
            }
        }
        for level in [1u32, 2, 3] {
            let t = tessellate_adaptive(
                &points,
                &counts,
                &indices,
                &request(0),
                level,
                &constant(1 << level),
            )
            .unwrap();
            let uniform = uniform_points(&points, &counts, &indices, level);
            assert_eq!(t.points.len(), uniform.len(), "level {level}: vertex count");
            let d = worst_distance(&t.points, &uniform);
            assert!(
                d < 1e-4,
                "level {level}: a vertex is {d} from the uniform limit"
            );
        }
    }

    #[test]
    fn near_extraordinary_vertices_the_patches_approximate_the_limit() {
        // (The bound below and ADAPTIVE_ISOLATION's doc are the measurement.)
        let (points, counts, indices) = cube();
        for level in [1u32, 2, 3] {
            let t = tessellate_adaptive(
                &points,
                &counts,
                &indices,
                &request(0),
                level,
                &constant(1 << level),
            )
            .unwrap();
            let uniform = uniform_points(&points, &counts, &indices, level);
            assert_eq!(t.points.len(), uniform.len());
            let d = worst_distance(&t.points, &uniform);
            // Measured and pinned: the cube is all extraordinary corners, the
            // Gregory patches' worst case. Down to the isolation depth the
            // samples are refined vertices, exact limit points; below it the
            // Gregory patches approximate the limit (see ADAPTIVE_ISOLATION).
            let bound = if level as usize <= ADAPTIVE_ISOLATION {
                1e-6
            } else {
                0.03
            };
            assert!(d < bound, "level {level}: {d}");
        }
    }

    #[test]
    fn ptex_corners_land_on_their_vertices() {
        let (points, counts, indices) = cube();
        let req = SubdivRequest {
            want_face_uvs: true,
            ..request(0)
        };
        let t = tessellate_adaptive(&points, &counts, &indices, &req, 3, &mixed).unwrap();
        let faces = t.faces.as_ref().expect("face table");
        assert_eq!(faces.base_face.len() * 3, t.indices.len());
        // Re-evaluate each corner through the patch table of its own face: it
        // must be the vertex the triangle indexes.
        let (counts_us, indices_u32) = validate_cage(points.len(), &counts, &indices).unwrap();
        let descriptor = TopologyDescriptor::new(points.len(), &counts_us, &indices_u32);
        let mut refiner = TopologyRefinerFactory::create(
            descriptor,
            sdc::SchemeType::Catmark,
            sdc::Options::default()
                .with_vtx_boundary_interpolation(sdc::VtxBoundaryInterpolation::EdgeAndCorner),
        )
        .unwrap();
        let mut adaptive = opensubdiv_rs::far::AdaptiveOptions::new(ADAPTIVE_ISOLATION);
        adaptive.use_single_crease_patch = true;
        refiner.refine_adaptive(adaptive);
        let table = opensubdiv_rs::far::PatchTableFactory::create(&refiner).unwrap();
        let map = opensubdiv_rs::far::PatchMap::new(&table);
        let primvar = PrimvarRefiner::new(&refiner);
        let mut control: Vec<[f32; 3]> = points.iter().map(|p| [p.x, p.y, p.z]).collect();
        let mut vals = control.clone();
        for l in 1..=refiner.max_level() {
            let mut r = vec![[0.0f32; 3]; refiner.level(l).num_vertices()];
            primvar.interpolate(l, &vals, &mut r);
            control.extend_from_slice(&r);
            vals = r;
        }
        let mut worst = 0.0f32;
        for (tri, (face, corners)) in t
            .indices
            .chunks(3)
            .zip(faces.base_face.iter().zip(&faces.corner_uvs))
        {
            let face = face.expect("every cube face is a quad") as usize;
            let ptex = table.ptex_indices().face_id(face) as usize;
            for (&v, uv) in tri.iter().zip(corners) {
                let patch = map.find_patch(ptex, uv[0], uv[1]).unwrap();
                let (p, _, _) = table.evaluate(patch, uv[0], uv[1], &control);
                let q = t.points[v as usize];
                worst = worst.max(Vec3A::new(p[0] - q.x, p[1] - q.y, p[2] - q.z).length());
            }
        }
        assert!(worst < 1e-5, "a Ptex corner is {worst} off its vertex");
    }

    /// A face-varying chart giving every cube face its own unit square (every
    /// edge a seam): each triangle corner's UV is its own Ptex coordinate,
    /// whichever face shares the vertex — at mixed rates, where a shared seam
    /// vertex would otherwise take the other side's value.
    #[test]
    fn a_seamed_face_varying_chart_keeps_each_side() {
        let (points, counts, indices) = cube();
        let square = [[0.0f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let values: Vec<[f32; 2]> = (0..6).flat_map(|_| square).collect();
        let chart_indices: Vec<i32> = (0..24).collect();
        for linear in [
            sdc::FVarLinearInterpolation::All,
            sdc::FVarLinearInterpolation::Boundaries,
        ] {
            let req = SubdivRequest {
                want_face_uvs: true,
                uvs: Some(UvChannel {
                    values: &values,
                    indices: Some(&chart_indices),
                    face_varying: true,
                    linear,
                }),
                ..request(0)
            };
            let t = tessellate_adaptive(&points, &counts, &indices, &req, 3, &mixed).unwrap();
            let (fv, corners) = t.face_varying_uvs.as_ref().expect("a face-varying chart");
            let ptex = t.faces.as_ref().unwrap();
            assert_eq!(corners.len(), t.indices.len());
            let mut worst = 0.0f32;
            for (k, &c) in corners.iter().enumerate() {
                let want = ptex.corner_uvs[k / 3][k % 3];
                let got = fv[c as usize];
                worst = worst.max((got[0] - want[0]).abs().max((got[1] - want[1]).abs()));
            }
            // The chart is affine on each face, so even a smooth rule
            // reproduces it.
            assert!(
                worst < 1e-5,
                "{linear:?}: a corner's UV is {worst} off its Ptex coordinate"
            );
        }
    }

    #[test]
    fn triangle_quality_is_one_for_equilateral_and_falls_for_slivers() {
        let (a, b) = (Vec3A::ZERO, Vec3A::X);
        let apex = Vec3A::new(0.5, 3f32.sqrt() / 2.0, 0.0);
        assert!((triangle_quality(a, b, apex) - 1.0).abs() < 1e-5);
        assert!(triangle_quality(a, b, Vec3A::new(0.5, 0.01, 0.0)) < 0.05);
        assert_eq!(triangle_quality(a, a, a), 0.0);
        assert_eq!(quality_bin(1.0), 0);
        assert_eq!(quality_bin(0.005), 4);
    }

    /// A smooth, non-affine face-varying chart with no seam, on a curved grid
    /// whose +X half is refined: every vertex carries one chart value, whichever
    /// face — refined or left at its cage — uses it. Under `cornersPlus1` the
    /// limit chart differs from the authored values at interior vertices, so an
    /// unrefined face must take its refined neighbour's value where they meet.
    #[test]
    fn a_smooth_chart_is_continuous_where_refined_and_cage_faces_meet() {
        let g = 6;
        let mut points = Vec::new();
        let mut values = Vec::new();
        for j in 0..=g {
            for i in 0..=g {
                let (x, y) = (i as f32, j as f32);
                points.push(Vec3f::from([
                    x,
                    y,
                    ((x * 0.9).sin() * (y * 0.7).cos()) * 0.5,
                ]));
                values.push([0.1 * x * x, (0.4 * y).sin()]);
            }
        }
        let (mut counts, mut indices) = (Vec::new(), Vec::new());
        for j in 0..g {
            for i in 0..g {
                let a = j * (g + 1) + i;
                counts.push(4);
                indices.extend_from_slice(&[a, a + 1, a + g + 2, a + g + 1]);
            }
        }
        let near_x = |p: &[[f32; 3]]| {
            if p[0][0] > 3.5 || p[1][0] > 3.5 {
                3.5
            } else {
                0.5
            }
        };
        for linear in [
            sdc::FVarLinearInterpolation::CornersPlus1,
            sdc::FVarLinearInterpolation::All,
        ] {
            let req = SubdivRequest {
                uvs: Some(UvChannel {
                    values: &values,
                    indices: Some(&indices),
                    face_varying: true,
                    linear,
                }),
                ..request(0)
            };
            let t = tessellate_adaptive(&points, &counts, &indices, &req, 3, &near_x).unwrap();
            let (fv, corners) = t.face_varying_uvs.as_ref().unwrap();
            let mut at: std::collections::HashMap<i32, [f32; 2]> = Default::default();
            let mut worst = 0.0f32;
            for (&v, &c) in t.indices.iter().zip(corners) {
                let uv = fv[c as usize];
                let first = *at.entry(v).or_insert(uv);
                worst = worst.max((first[0] - uv[0]).abs().max((first[1] - uv[1]).abs()));
            }
            assert!(
                worst < 1e-5,
                "{linear:?}: a vertex's chart value jumps by {worst}"
            );
            assert!(t.rate_range.1 > 1, "some faces are refined");
        }
    }

    #[test]
    fn normals_point_out_of_a_closed_surface() {
        for (name, (points, counts, indices)) in [("cube", cube()), ("prism", prism())] {
            let t =
                tessellate_adaptive(&points, &counts, &indices, &request(0), 2, &mixed).unwrap();
            for (p, n) in t.points.iter().zip(&t.normals) {
                let out = Vec3A::new(p.x, p.y, p.z).dot(Vec3A::from(*n));
                assert!(out > 0.0, "{name}: normal {n:?} at {p:?} points inward");
            }
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
        let verts: Vec<[f32; 3]> = points.iter().map(|p| [p.x, p.y, p.z]).collect();
        let normals = smooth_normals(&verts, &counts, &indices);
        for (v, n) in verts.iter().zip(&normals) {
            let expect = Vec3A::from_array(*v).normalize();
            assert!(
                Vec3A::from_array(*n).dot(expect) > 0.99,
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

    /// `cornersPlus2` pins a concave UV corner (opensubdiv-rs ≥ 0.1.4). A 2×2
    /// grid whose faces F0-F2 form one L-shaped island and F3 another: at
    /// the centre vertex the L's value spans three faces — a reflex corner —
    /// and F3's spans one. `cornersPlus2` keeps the L's value where it was
    /// authored, `cornersPlus1` smooths it along the island boundary.
    #[test]
    fn corners_plus2_pins_a_concave_uv_corner() {
        let points: Vec<Vec3f> = (0..3)
            .flat_map(|j| (0..3).map(move |i| Vec3f::from([i as f32, j as f32, 0.0])))
            .collect();
        let counts = vec![4; 4];
        let indices = vec![0, 1, 4, 3, 1, 2, 5, 4, 3, 4, 7, 6, 4, 5, 8, 7];
        // The layout of opensubdiv-rs's own `L_ISLAND` fixture.
        let values = [
            [0.0, 0.0],
            [0.5, 0.0],
            [1.0, 0.0],
            [0.0, 0.5],
            [0.5, 0.5],
            [1.0, 0.4],
            [0.0, 1.0],
            [0.6, 1.0],
            [0.5, 0.5],
            [1.0, 0.5],
            [1.0, 1.0],
            [0.5, 1.0],
        ];
        let value_indices = [0, 1, 4, 3, 1, 2, 5, 4, 3, 4, 7, 6, 8, 9, 10, 11];
        let centre_uv_on_the_l = |linear| {
            let req = SubdivRequest {
                uvs: Some(UvChannel {
                    values: &values,
                    indices: Some(&value_indices),
                    face_varying: true,
                    linear,
                }),
                ..request(1)
            };
            let out = subdivide(&points, &counts, &indices, &req).unwrap();
            let uvs = out.uvs.as_ref().unwrap();
            // A refined face-vertex at the centre point, on a face of the L
            // (every face but F3 = [1, 2]²).
            let mut offset = 0;
            for &n in &out.counts {
                let face = &out.indices[offset..offset + n as usize];
                let centre = face.iter().fold([0.0f32; 2], |c, &p| {
                    let q = out.points[p as usize];
                    [c[0] + q.x / n as f32, c[1] + q.y / n as f32]
                });
                let on_l = !(centre[0] > 1.0 && centre[1] > 1.0);
                for (k, &p) in face.iter().enumerate() {
                    let q = out.points[p as usize];
                    if on_l && (q.x - 1.0).abs() < 1e-5 && (q.y - 1.0).abs() < 1e-5 {
                        return uv_at(uvs, offset + k, p as usize);
                    }
                }
                offset += n as usize;
            }
            panic!("no refined face-vertex at the centre on the L");
        };
        let pinned = centre_uv_on_the_l(sdc::FVarLinearInterpolation::CornersPlus2);
        assert!(
            (pinned[0] - 0.5).abs() < 1e-5 && (pinned[1] - 0.5).abs() < 1e-5,
            "cornersPlus2 keeps the concave corner at (0.5, 0.5), got {pinned:?}"
        );
        let smoothed = centre_uv_on_the_l(sdc::FVarLinearInterpolation::CornersPlus1);
        assert!(
            (smoothed[0] - 0.5).abs() > 1e-3 || (smoothed[1] - 0.5).abs() > 1e-3,
            "cornersPlus1 smooths the concave corner, got {smoothed:?}"
        );
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
                    // face-varying channel, ~540 with the Ptex one, ~517 with
                    // a shared UV chart; resident 44.0 / 84.0 / 68.1 (the
                    // Ptex table's 40 B/face: four corner UVs plus an
                    // `Option<u32>` base face; the normals are 12 B per
                    // vertex since `compact-triangle-storage`, 16 before).
                    // Ceilings ~20% above those, per mode, so no mode can
                    // hide under another's.
                    let (ceiling, resident_ceiling) = match mode {
                        "no" => (380.0, 53.0),
                        "ptex" => (650.0, 100.0),
                        _ => (620.0, 82.0),
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
