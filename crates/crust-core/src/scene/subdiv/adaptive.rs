//! Per-face adaptive tessellation: only the cage faces with an edge rated
//! above 1 are evaluated on the limit surface, the rest render their cage.

use glam::Vec3A;
use opensubdiv_rs::far::{
    FVarChannelDescriptor, PrimvarRefiner, TopologyDescriptor, TopologyRefinerFactory,
};
use opensubdiv_rs::sdc;
use openusd::gf::Vec3f;

use super::normals::smooth_cage_normals;
use super::topology::{expand_crease_runs, validate_cage, validate_corners};
use super::{
    QUALITY_BINS, SegmentSize, SubdivError, SubdivRequest, SubdivScheme, TessellatedFaces,
    TessellatedMesh,
};

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
pub(super) const ADAPTIVE_ISOLATION: usize = 1;

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
    use crate::scene::tessellate::{PointKey, edge_rate, tessellate_quad};
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
