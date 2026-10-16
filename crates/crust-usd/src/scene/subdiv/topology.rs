//! Cage validation and the topology the refiner is handed: crease runs,
//! corners, and the synthetic Ptex face-varying channel.

use opensubdiv_rs::sdc;

use super::SubdivError;

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

pub(super) fn validate_corners(
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
pub(super) fn ptex_fvar_channel(counts: &[usize]) -> (Vec<[f32; 2]>, Vec<u32>) {
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
