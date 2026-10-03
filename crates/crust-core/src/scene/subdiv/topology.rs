//! Cage validation: the checks the refiner cannot make itself, USD's crease
//! runs and corners in the shapes it takes, and the refiner both
//! [`subdivide`](super::subdivide) and
//! [`tessellate_adaptive`](super::tessellate_adaptive) build from them.

use opensubdiv_rs::far::{
    FVarChannelDescriptor, TopologyDescriptor, TopologyRefiner, TopologyRefinerFactory,
};
use opensubdiv_rs::sdc;

use super::{SubdivError, SubdivRequest, SubdivScheme, UvChannel};

/// A cage checked whole and converted to the refiner's shapes, with the
/// scheme and options it refines under — the setup uniform refinement and
/// per-face tessellation share.
pub(super) struct Cage {
    /// `faceVertexCounts`, every entry at least 3.
    pub(super) counts: Vec<usize>,
    /// `faceVertexIndices`, every entry a valid point.
    pub(super) indices: Vec<u32>,
    crease_pairs: Vec<[u32; 2]>,
    crease_weights: Vec<f32>,
    corners: (Vec<u32>, Vec<f32>),
    scheme: sdc::SchemeType,
    options: sdc::Options,
}

/// Validates the cage, expands its creases and corners, and maps the scheme
/// and boundary rule. The face-varying rule is one per refiner, not per
/// channel, so the authored chart's rule wins. The synthetic Ptex channel must
/// refine bilinearly, and does under every rule but `none` (which
/// [`subdivide`](super::subdivide) refines apart) — each of its values is
/// private to one face, so every one of its edges is a face-varying boundary,
/// and its data is affine. Without a chart, `All`.
pub(super) fn prepare_cage(
    n_points: usize,
    counts: &[i32],
    indices: &[i32],
    req: &SubdivRequest,
) -> Result<Cage, SubdivError> {
    let (counts, indices) = validate_cage(n_points, counts, indices)?;
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
    let chart = req.uvs.as_ref().filter(|c| c.face_varying);
    let options = sdc::Options::default()
        .with_vtx_boundary_interpolation(req.boundary)
        .with_fvar_linear_interpolation(
            chart.map_or(sdc::FVarLinearInterpolation::All, |c| c.linear),
        );
    Ok(Cage {
        counts,
        indices,
        crease_pairs,
        crease_weights,
        corners,
        scheme,
        options,
    })
}

impl Cage {
    /// A face-varying chart's per-face-vertex indices: its channel's
    /// topology, while its values seed the refinement. Empty without one.
    pub(super) fn chart_indices(&self, chart: Option<&UvChannel>) -> Vec<u32> {
        match chart {
            Some(c) => (0..self.indices.len())
                .map(|fv| c.value_index(fv) as u32)
                .collect(),
            None => Vec::new(),
        }
    }

    /// The topology refiner for this cage of `n_points` points, carrying the
    /// face-varying `channels`.
    pub(super) fn refiner(
        &self,
        n_points: usize,
        channels: &[FVarChannelDescriptor],
    ) -> Result<TopologyRefiner, SubdivError> {
        let mut descriptor = TopologyDescriptor::new(n_points, &self.counts, &self.indices)
            .with_creases(&self.crease_pairs, &self.crease_weights)
            .with_corners(&self.corners.0, &self.corners.1);
        if !channels.is_empty() {
            descriptor = descriptor.with_fvar_channels(channels);
        }
        TopologyRefinerFactory::create(descriptor, self.scheme, self.options)
            .map_err(SubdivError::Refine)
    }
}

/// The refiner indexes with `usize` counts and `u32` indices, and it cannot
/// skip a malformed face the way `triangulate` does — so the cage is checked
/// whole, up front.
pub(super) fn validate_cage(
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
pub(super) fn expand_crease_runs(
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
