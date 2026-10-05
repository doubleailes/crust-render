use glam::Vec3A;
use opensubdiv_rs::far::{PrimvarRefiner, TopologyDescriptor, TopologyRefinerFactory};

use super::adaptive::*;
use super::topology::*;
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
        let t =
            tessellate_adaptive(&points, &counts, &indices, &request(0), 3, &constant(1)).unwrap();
        assert_closed_and_consistent(&t.indices, &format!("{name} at rate 1"));
    }
}

/// A face whose every edge is split once is not refined at all: it renders
/// its cage, smooth-shaded, as level 0 does — and no patch is built for it.
#[test]
fn rate_one_faces_render_their_smooth_cage() {
    let (points, counts, indices) = cube();
    let t = tessellate_adaptive(&points, &counts, &indices, &request(0), 3, &constant(1)).unwrap();
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
fn uniform_points(points: &[Vec3f], counts: &[i32], indices: &[i32], level: u32) -> Vec<[f32; 3]> {
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
        let all =
            tessellate_adaptive(&points, &counts, &indices, &request(0), 3, &constant(6)).unwrap();
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
        let t = tessellate_adaptive(&points, &counts, &indices, &request(0), 2, &mixed).unwrap();
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
