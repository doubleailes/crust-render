//! Black-box tests of the `crust-rt` kernel through its public,
//! Embree-shaped API: geometry expansion, closest-hit and occlusion
//! queries, masks, instancing, motion blur, curves, and the reporting
//! helpers. Brute-force reference intersectors check the BVH against the
//! primitives it is supposed to be equivalent to.

use crust_rt::{
    AABB, CubicCurveSegment, CurveSegment, Geometry, INVALID_ID, MASK_ALL, MASK_CAMERA,
    MASK_INDIRECT, MASK_SHADOW, Ray, Scene, SceneBuilder,
};
use glam::{Affine3A, Vec3, Vec3A};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as u32 & 0x00FF_FFFF) as f32 / 16_777_216.0
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }
    fn vec(&mut self, lo: f32, hi: f32) -> Vec3A {
        Vec3A::new(self.range(lo, hi), self.range(lo, hi), self.range(lo, hi))
    }
    fn dir(&mut self) -> Vec3A {
        loop {
            let v = self.vec(-1.0, 1.0);
            let l = v.length_squared();
            if l > 1e-3 && l < 1.0 {
                return v / l.sqrt();
            }
        }
    }
}

fn sphere(center: Vec3A, radius: f32) -> Geometry {
    Geometry::Sphere { center, radius }
}

fn unit_sphere_scene() -> Arc<Scene> {
    let mut b = SceneBuilder::new();
    b.attach(sphere(Vec3A::ZERO, 1.0));
    Arc::new(b.commit())
}

fn mesh(vertices: Vec<Vec3A>, indices: Vec<[u32; 3]>) -> Geometry {
    Geometry::TriangleMesh {
        vertices,
        indices,
        normals: None,
    }
}

/// The unit quad `[0,1]²` in the z = 0 plane, fan-triangulated.
fn unit_quad() -> Geometry {
    mesh(
        vec![
            Vec3A::new(0.0, 0.0, 0.0),
            Vec3A::new(1.0, 0.0, 0.0),
            Vec3A::new(1.0, 1.0, 0.0),
            Vec3A::new(0.0, 1.0, 0.0),
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    )
}

/// An `n × n` grid of quads over `[0,1]²` at z = 0.
fn grid(n: usize) -> (Vec<Vec3A>, Vec<[u32; 3]>) {
    let mut verts = Vec::new();
    for j in 0..=n {
        for i in 0..=n {
            verts.push(Vec3A::new(i as f32 / n as f32, j as f32 / n as f32, 0.0));
        }
    }
    let mut tris = Vec::new();
    let idx = |i: usize, j: usize| (j * (n + 1) + i) as u32;
    for j in 0..n {
        for i in 0..n {
            tris.push([idx(i, j), idx(i + 1, j), idx(i + 1, j + 1)]);
            tris.push([idx(i, j), idx(i + 1, j + 1), idx(i, j + 1)]);
        }
    }
    (verts, tris)
}

/// A lat-long tessellation of the unit sphere with outward vertex normals.
fn tessellated_sphere(rings: usize, sectors: usize) -> (Vec<Vec3A>, Vec<[u32; 3]>, Vec<Vec3A>) {
    let mut verts = Vec::new();
    for r in 0..=rings {
        let theta = std::f32::consts::PI * r as f32 / rings as f32;
        for s in 0..=sectors {
            let phi = std::f32::consts::TAU * s as f32 / sectors as f32;
            verts.push(Vec3A::new(
                theta.sin() * phi.cos(),
                theta.cos(),
                theta.sin() * phi.sin(),
            ));
        }
    }
    let mut tris = Vec::new();
    let idx = |r: usize, s: usize| (r * (sectors + 1) + s) as u32;
    for r in 0..rings {
        for s in 0..sectors {
            tris.push([idx(r, s), idx(r + 1, s), idx(r + 1, s + 1)]);
            tris.push([idx(r, s), idx(r + 1, s + 1), idx(r, s + 1)]);
        }
    }
    let normals = verts.clone();
    (verts, tris, normals)
}

/// Möller–Trumbore, the reference the kernel's watertight intersector is
/// compared against (agreement to a few ulps away from edges).
fn moller_trumbore(ray: &Ray, v0: Vec3A, v1: Vec3A, v2: Vec3A) -> Option<f32> {
    let e1 = v1 - v0;
    let e2 = v2 - v0;
    let p = ray.dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = ray.origin - v0;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = ray.dir.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some(t)
}

fn analytic_sphere_t(ray: &Ray, center: Vec3A, radius: f32, t_min: f32, t_max: f32) -> Option<f32> {
    let oc = ray.origin - center;
    let a = ray.dir.length_squared();
    let half_b = oc.dot(ray.dir);
    let c = oc.length_squared() - radius * radius;
    let disc = half_b * half_b - a * c;
    if disc < 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    for root in [(-half_b - sq) / a, (-half_b + sq) / a] {
        if root > t_min && root < t_max {
            return Some(root);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Ray and AABB
// ---------------------------------------------------------------------------

#[test]
fn ray_defaults_are_time_zero_and_visible_to_all() {
    let r = Ray::new(Vec3A::new(1.0, 2.0, 3.0), Vec3A::X);
    assert_eq!(r.time, 0.0);
    assert_eq!(r.mask, MASK_ALL);
    assert_eq!(r.origin, Vec3A::new(1.0, 2.0, 3.0));
    assert_eq!(r.dir, Vec3A::X);
    let d = Ray::default();
    assert_eq!(d.origin, Vec3A::ZERO);
    assert_eq!(d.dir, Vec3A::ZERO);
}

#[test]
fn ray_builders_replace_only_their_field() {
    let r = Ray::new(Vec3A::ZERO, Vec3A::Z)
        .with_time(0.25)
        .with_mask(MASK_SHADOW);
    assert_eq!(r.time, 0.25);
    assert_eq!(r.mask, MASK_SHADOW);
    assert_eq!(r.origin, Vec3A::ZERO);
    assert_eq!(r.dir, Vec3A::Z);
}

#[test]
fn ray_at_walks_an_unnormalized_direction() {
    let r = Ray::new(Vec3A::new(1.0, 0.0, 0.0), Vec3A::new(0.0, 2.0, 0.0));
    assert_eq!(r.at(0.0), Vec3A::new(1.0, 0.0, 0.0));
    assert_eq!(r.at(1.5), Vec3A::new(1.0, 3.0, 0.0));
    assert_eq!(r.at(-1.0), Vec3A::new(1.0, -2.0, 0.0));
}

#[test]
fn mask_constants_are_distinct_bits() {
    assert_eq!(MASK_CAMERA & MASK_SHADOW, 0);
    assert_eq!(MASK_CAMERA & MASK_INDIRECT, 0);
    assert_eq!(MASK_SHADOW & MASK_INDIRECT, 0);
    assert_eq!(MASK_ALL & MASK_CAMERA, MASK_CAMERA);
    assert_eq!(INVALID_ID, u32::MAX);
}

#[test]
fn aabb_slab_test_hits_and_misses() {
    let b = AABB::new(Vec3A::splat(-1.0), Vec3A::splat(1.0));
    assert!(b.hit(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z), 0.0, 100.0));
    assert!(b.hit(&Ray::new(Vec3A::new(0.0, 0.0, 5.0), -Vec3A::Z), 0.0, 100.0));
    assert!(!b.hit(&Ray::new(Vec3A::new(3.0, 0.0, -5.0), Vec3A::Z), 0.0, 100.0));
    // Pointing away.
    assert!(!b.hit(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), -Vec3A::Z), 0.0, 100.0));
    // Range too short to reach the box.
    assert!(!b.hit(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z), 0.0, 3.0));
    // From inside.
    assert!(b.hit(
        &Ray::new(Vec3A::ZERO, Vec3A::new(0.3, 0.4, 0.5)),
        0.0,
        100.0
    ));
}

#[test]
fn aabb_diagonal_ray_and_axis_parallel_ray() {
    let b = AABB::new(Vec3A::ZERO, Vec3A::ONE);
    assert!(b.hit(&Ray::new(Vec3A::splat(-1.0), Vec3A::ONE), 0.0, 100.0));
    // Parallel to X, outside the y range: misses however far it goes.
    assert!(!b.hit(&Ray::new(Vec3A::new(-5.0, 2.0, 0.5), Vec3A::X), 0.0, 1e9));
    // Parallel to X, inside the y and z ranges: hits.
    assert!(b.hit(&Ray::new(Vec3A::new(-5.0, 0.5, 0.5), Vec3A::X), 0.0, 1e9));
}

#[test]
fn aabb_surrounding_box_is_the_union() {
    let a = AABB::new(Vec3A::new(-1.0, 0.0, 0.0), Vec3A::new(0.0, 1.0, 2.0));
    let b = AABB::new(Vec3A::new(0.5, -3.0, 1.0), Vec3A::new(4.0, 0.5, 1.5));
    let u = AABB::surrounding_box(a, b);
    assert_eq!(u.minimum, Vec3A::new(-1.0, -3.0, 0.0));
    assert_eq!(u.maximum, Vec3A::new(4.0, 1.0, 2.0));
    // Commutative.
    let v = AABB::surrounding_box(b, a);
    assert_eq!(v.minimum, u.minimum);
    assert_eq!(v.maximum, u.maximum);
}

// ---------------------------------------------------------------------------
// Empty and trivial scenes
// ---------------------------------------------------------------------------

#[test]
fn empty_scene_answers_every_query_negatively() {
    let scene = SceneBuilder::new().commit();
    let ray = Ray::new(Vec3A::ZERO, Vec3A::Z);
    assert!(scene.intersect(&ray, 0.0, f32::INFINITY).is_none());
    assert!(!scene.occluded(&ray, 0.0, f32::INFINITY));
    assert!(scene.bounds().is_none());
    assert_eq!(scene.geometry_count(), 0);
    assert_eq!(scene.primitive_count(), 0);
    assert!(!scene.has_motion());
    let (n, diag, mean, max) = scene.primitive_extents();
    assert_eq!((n, diag, mean, max), (0, 0.0, 0.0, 0.0));
}

#[test]
fn builder_count_tracks_attachments_and_reserve_is_a_hint() {
    let mut b = SceneBuilder::new();
    assert_eq!(b.count(), 0);
    b.reserve(1000);
    assert_eq!(b.count(), 0, "reserve must not attach anything");
    let a = b.attach(sphere(Vec3A::ZERO, 1.0));
    let c = b.attach(sphere(Vec3A::X * 5.0, 1.0));
    assert_eq!((a, c), (0, 1));
    assert_eq!(b.count(), 2);
    let scene = b.commit();
    assert_eq!(scene.geometry_count(), 2);
    assert_eq!(scene.primitive_count(), 2);
}

#[test]
fn set_geometry_keeps_the_original_mask() {
    let mut b = SceneBuilder::new();
    let slot = b.attach_masked(SceneBuilder::empty_geometry(), MASK_SHADOW);
    b.set_geometry(slot, sphere(Vec3A::ZERO, 1.0));
    let scene = b.commit();
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    assert!(
        scene
            .intersect(&ray.with_mask(MASK_CAMERA), 0.001, 100.0)
            .is_none()
    );
    assert!(
        scene
            .intersect(&ray.with_mask(MASK_SHADOW), 0.001, 100.0)
            .is_some()
    );
}

// ---------------------------------------------------------------------------
// Spheres
// ---------------------------------------------------------------------------

#[test]
fn sphere_distances_from_outside_and_inside() {
    let scene = unit_sphere_scene();
    let out = scene
        .intersect(
            &Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z),
            0.001,
            100.0,
        )
        .unwrap();
    assert!((out.t - 4.0).abs() < 1e-5);
    assert!((out.normal - (-Vec3A::Z)).length() < 1e-5);
    let inside = scene
        .intersect(
            &Ray::new(Vec3A::ZERO, Vec3A::new(0.0, 3.0, 0.0)),
            0.001,
            100.0,
        )
        .unwrap();
    // Unnormalized direction: t carries the direction's scale.
    assert!((inside.t - 1.0 / 3.0).abs() < 1e-5);
    assert!(!inside.front_face);
}

#[test]
fn sphere_misses_when_pointing_away_or_offset() {
    let scene = unit_sphere_scene();
    assert!(
        scene
            .intersect(
                &Ray::new(Vec3A::new(0.0, 0.0, -5.0), -Vec3A::Z),
                0.001,
                100.0
            )
            .is_none()
    );
    assert!(
        scene
            .intersect(
                &Ray::new(Vec3A::new(0.0, 1.5, -5.0), Vec3A::Z),
                0.001,
                100.0
            )
            .is_none()
    );
}

#[test]
fn t_range_clips_a_sphere_hit() {
    let scene = unit_sphere_scene();
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    assert!(scene.intersect(&ray, 0.001, 3.9).is_none());
    assert!(scene.intersect(&ray, 0.001, 4.1).is_some());
    // Skipping the near root with t_min picks the far one.
    let far = scene.intersect(&ray, 4.5, 100.0).unwrap();
    assert!((far.t - 6.0).abs() < 1e-5);
    assert!(!far.front_face, "the far side is hit from inside");
    assert!(scene.intersect(&ray, 6.5, 100.0).is_none());
}

#[test]
fn t_min_avoids_self_intersection_at_the_origin() {
    let scene = unit_sphere_scene();
    // Starting exactly on the surface and travelling inward.
    let hit = scene
        .intersect(&Ray::new(Vec3A::new(0.0, 0.0, -1.0), Vec3A::Z), 1e-3, 100.0)
        .unwrap();
    assert!(
        (hit.t - 2.0).abs() < 1e-4,
        "hit the far side, not the origin: {}",
        hit.t
    );
}

#[test]
fn sphere_bounds_are_center_plus_minus_radius() {
    let mut b = SceneBuilder::new();
    b.attach(sphere(Vec3A::new(1.0, 2.0, 3.0), 0.5));
    let bb = b.commit().bounds().unwrap();
    assert!(bb.minimum.abs_diff_eq(Vec3A::new(0.5, 1.5, 2.5), 1e-6));
    assert!(bb.maximum.abs_diff_eq(Vec3A::new(1.5, 2.5, 3.5), 1e-6));
}

#[test]
fn closest_of_two_overlapping_spheres_wins() {
    let mut b = SceneBuilder::new();
    let near = b.attach(sphere(Vec3A::new(0.0, 0.0, 0.0), 1.0));
    let far = b.attach(sphere(Vec3A::new(0.0, 0.0, 3.0), 1.0));
    let scene = b.commit();
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    assert_eq!(scene.intersect(&ray, 0.001, 100.0).unwrap().geom_id, near);
    let from_behind = Ray::new(Vec3A::new(0.0, 0.0, 8.0), -Vec3A::Z);
    assert_eq!(
        scene.intersect(&from_behind, 0.001, 100.0).unwrap().geom_id,
        far
    );
    // Occlusion with a range stopping between the two sees only the near.
    assert!(scene.occluded(&ray, 0.001, 5.0));
    assert!(
        !scene.occluded(&ray, 4.5, 5.5),
        "the gap between the spheres is clear"
    );
}

#[test]
fn a_thousand_spheres_map_back_to_their_ids() {
    let mut b = SceneBuilder::new();
    for i in 0..1000 {
        b.attach(sphere(Vec3A::new(i as f32 * 3.0, 0.0, 0.0), 1.0));
    }
    let scene = b.commit();
    assert_eq!(scene.primitive_count(), 1000);
    for i in (0..1000).step_by(37) {
        let ray = Ray::new(Vec3A::new(i as f32 * 3.0, 0.0, -10.0), Vec3A::Z);
        let hit = scene.intersect(&ray, 0.001, 100.0).unwrap();
        assert_eq!(hit.geom_id, i);
        assert_eq!(hit.prim_id, 0);
        assert!((hit.t - 9.0).abs() < 1e-4);
    }
}

#[test]
fn random_spheres_agree_with_the_analytic_closest_hit() {
    let mut rng = Lcg::new(11);
    let spheres: Vec<(Vec3A, f32)> = (0..200)
        .map(|_| (rng.vec(-10.0, 10.0), rng.range(0.2, 1.5)))
        .collect();
    let mut b = SceneBuilder::new();
    for (c, r) in &spheres {
        b.attach(sphere(*c, *r));
    }
    let scene = b.commit();
    let mut hits = 0;
    for _ in 0..1000 {
        let ray = Ray::new(rng.vec(-15.0, 15.0), rng.dir());
        let (t_min, t_max) = (1e-3, 60.0);
        let mut best: Option<(f32, u32)> = None;
        for (i, (c, r)) in spheres.iter().enumerate() {
            if let Some(t) = analytic_sphere_t(&ray, *c, *r, t_min, t_max)
                && best.is_none_or(|(bt, _)| t < bt)
            {
                best = Some((t, i as u32));
            }
        }
        let got = scene.intersect(&ray, t_min, t_max);
        match (best, got) {
            (None, None) => {}
            (Some((t, id)), Some(h)) => {
                hits += 1;
                assert!((h.t - t).abs() < 1e-3 * t.max(1.0), "t {} vs {}", h.t, t);
                assert_eq!(h.geom_id, id);
            }
            (b, g) => panic!(
                "brute force {b:?} vs kernel {:?}",
                g.map(|h| (h.t, h.geom_id))
            ),
        }
        assert_eq!(scene.occluded(&ray, t_min, t_max), best.is_some());
    }
    assert!(hits > 100, "the probe rays should hit often: {hits}");
}

// ---------------------------------------------------------------------------
// Triangles
// ---------------------------------------------------------------------------

#[test]
fn triangle_barycentrics_weight_the_second_and_third_vertices() {
    let mut b = SceneBuilder::new();
    b.attach(mesh(vec![Vec3A::ZERO, Vec3A::X, Vec3A::Y], vec![[0, 1, 2]]));
    let scene = b.commit();
    let hit = scene
        .intersect(
            &Ray::new(Vec3A::new(0.25, 0.5, -1.0), Vec3A::Z),
            0.001,
            10.0,
        )
        .unwrap();
    assert!((hit.t - 1.0).abs() < 1e-5);
    assert!((hit.u - 0.25).abs() < 1e-4, "u = {}", hit.u);
    assert!((hit.v - 0.5).abs() < 1e-4, "v = {}", hit.v);
    // Corners.
    let at_v1 = scene
        .intersect(
            &Ray::new(Vec3A::new(0.999, 0.0005, -1.0), Vec3A::Z),
            0.001,
            10.0,
        )
        .unwrap();
    assert!(at_v1.u > 0.99 && at_v1.v < 0.01);
}

#[test]
fn triangle_facing_follows_the_ray_side() {
    let mut b = SceneBuilder::new();
    b.attach(mesh(vec![Vec3A::ZERO, Vec3A::X, Vec3A::Y], vec![[0, 1, 2]]));
    let scene = b.commit();
    // Geometric normal (v1-v0)×(v2-v0) = +Z. Arriving along +Z is the back.
    let back = scene
        .intersect(&Ray::new(Vec3A::new(0.2, 0.2, -1.0), Vec3A::Z), 0.001, 10.0)
        .unwrap();
    assert!(!back.front_face);
    assert!(back.normal.abs_diff_eq(-Vec3A::Z, 1e-5));
    let front = scene
        .intersect(&Ray::new(Vec3A::new(0.2, 0.2, 1.0), -Vec3A::Z), 0.001, 10.0)
        .unwrap();
    assert!(front.front_face);
    assert!(front.normal.abs_diff_eq(Vec3A::Z, 1e-5));
}

#[test]
fn triangle_misses_outside_its_edges() {
    let mut b = SceneBuilder::new();
    b.attach(mesh(vec![Vec3A::ZERO, Vec3A::X, Vec3A::Y], vec![[0, 1, 2]]));
    let scene = b.commit();
    for (x, y) in [(0.6, 0.6), (-0.1, 0.5), (0.5, -0.1), (1.1, 0.0)] {
        let ray = Ray::new(Vec3A::new(x, y, -1.0), Vec3A::Z);
        assert!(scene.intersect(&ray, 0.001, 10.0).is_none(), "({x}, {y})");
        assert!(!scene.occluded(&ray, 0.001, 10.0), "({x}, {y})");
    }
    // Parallel to the plane.
    assert!(
        scene
            .intersect(&Ray::new(Vec3A::new(-1.0, 0.2, 0.0), Vec3A::X), 0.001, 10.0)
            .is_none()
    );
}

#[test]
fn degenerate_triangles_never_hit_and_never_panic() {
    let mut b = SceneBuilder::new();
    // Collinear.
    b.attach(mesh(
        vec![Vec3A::ZERO, Vec3A::X, Vec3A::X * 2.0],
        vec![[0, 1, 2]],
    ));
    // Repeated vertex.
    b.attach(mesh(vec![Vec3A::Y, Vec3A::Y, Vec3A::Z], vec![[0, 1, 2]]));
    let scene = b.commit();
    let mut rng = Lcg::new(3);
    for _ in 0..200 {
        let ray = Ray::new(rng.vec(-3.0, 3.0), rng.dir());
        assert!(scene.intersect(&ray, 1e-4, 100.0).is_none());
        assert!(!scene.occluded(&ray, 1e-4, 100.0));
    }
}

#[test]
fn out_of_range_indices_are_skipped_not_panicked() {
    let mut b = SceneBuilder::new();
    b.attach(mesh(
        vec![Vec3A::ZERO, Vec3A::X, Vec3A::Y],
        vec![[0, 1, 2], [0, 1, 7], [9, 9, 9]],
    ));
    let scene = b.commit();
    assert_eq!(
        scene.primitive_count(),
        1,
        "only the valid triangle survives"
    );
    let hit = scene
        .intersect(&Ray::new(Vec3A::new(0.2, 0.2, -1.0), Vec3A::Z), 0.001, 10.0)
        .unwrap();
    assert_eq!(hit.prim_id, 0);
}

#[test]
fn short_normal_arrays_fall_back_to_the_face_normal() {
    let mut b = SceneBuilder::new();
    b.attach(Geometry::TriangleMesh {
        vertices: vec![Vec3A::ZERO, Vec3A::X, Vec3A::Y],
        indices: vec![[0, 1, 2]],
        // Two normals for three vertices: unusable, must be ignored.
        normals: Some(vec![Vec3A::X, Vec3A::X]),
    });
    let scene = b.commit();
    let hit = scene
        .intersect(&Ray::new(Vec3A::new(0.2, 0.2, 1.0), -Vec3A::Z), 0.001, 10.0)
        .unwrap();
    assert!(hit.normal.abs_diff_eq(Vec3A::Z, 1e-5));
}

#[test]
fn interpolated_shading_normals_are_unit_length() {
    let (v, t, n) = tessellated_sphere(12, 24);
    let mut b = SceneBuilder::new();
    b.attach(Geometry::TriangleMesh {
        vertices: v,
        indices: t,
        normals: Some(n),
    });
    let scene = b.commit();
    let mut rng = Lcg::new(5);
    for _ in 0..300 {
        let d = rng.dir();
        let ray = Ray::new(d * 5.0, -d);
        let hit = scene
            .intersect(&ray, 0.001, 100.0)
            .expect("aimed at the centre");
        assert!((hit.normal.length() - 1.0).abs() < 1e-4);
        // Shading normal of a sphere points back along the ray direction
        // (toward the origin of the probe), within tessellation error.
        assert!(
            hit.normal.dot(d) > 0.95,
            "normal {} vs dir {}",
            hit.normal,
            d
        );
        assert!(hit.front_face);
    }
}

#[test]
fn tessellated_sphere_matches_the_analytic_radius() {
    let (v, t, _) = tessellated_sphere(64, 128);
    let mut b = SceneBuilder::new();
    b.attach(mesh(v, t));
    let scene = b.commit();
    let mut rng = Lcg::new(6);
    for _ in 0..300 {
        let d = rng.dir();
        let hit = scene
            .intersect(&Ray::new(d * 4.0, -d), 0.001, 100.0)
            .unwrap();
        // Chordal error for a 64x128 tessellation is well under 1%.
        assert!((hit.t - 3.0).abs() < 0.01, "t = {}", hit.t);
    }
}

#[test]
fn random_triangle_soup_agrees_with_moller_trumbore() {
    let mut rng = Lcg::new(21);
    let mut verts = Vec::new();
    let mut tris = Vec::new();
    for i in 0..5000u32 {
        let c = rng.vec(-10.0, 10.0);
        for _ in 0..3 {
            verts.push(c + rng.vec(-0.6, 0.6));
        }
        tris.push([3 * i, 3 * i + 1, 3 * i + 2]);
    }
    let mut b = SceneBuilder::new();
    b.attach(mesh(verts.clone(), tris.clone()));
    let scene = b.commit();
    assert_eq!(scene.primitive_count(), 5000);

    let mut hits = 0;
    for _ in 0..600 {
        let ray = Ray::new(rng.vec(-12.0, 12.0), rng.dir());
        let (t_min, t_max) = (1e-3f32, 50.0f32);
        let mut best: Option<(f32, u32)> = None;
        for (i, tri) in tris.iter().enumerate() {
            if let Some(t) = moller_trumbore(
                &ray,
                verts[tri[0] as usize],
                verts[tri[1] as usize],
                verts[tri[2] as usize],
            ) && t > t_min
                && t < t_max
                && best.is_none_or(|(bt, _)| t < bt)
            {
                best = Some((t, i as u32));
            }
        }
        let got = scene.intersect(&ray, t_min, t_max);
        match (best, got) {
            (None, None) => {}
            (Some((t, id)), Some(h)) => {
                hits += 1;
                assert!((h.t - t).abs() < 1e-3 * t.max(1.0), "t {} vs {}", h.t, t);
                if (h.t - t).abs() < 1e-5 {
                    assert_eq!(h.prim_id, id);
                }
            }
            // A hit landing within a few ulps of an edge can legitimately
            // differ between the two intersectors; anything else is a bug.
            (Some((t, _)), None) | (None, Some(crust_rt::RayHit { t, .. })) => {
                panic!("disagreement on ray {:?} at t={t}", (ray.origin, ray.dir));
            }
        }
    }
    assert!(hits > 50, "expected many hits, got {hits}");
}

#[test]
fn grid_is_watertight_along_shared_edges_and_vertices() {
    let n = 8;
    let (v, t) = grid(n);
    let mut b = SceneBuilder::new();
    b.attach(mesh(v, t));
    let scene = b.commit();
    // Every interior grid vertex, every interior edge midpoint, and every
    // diagonal midpoint: each must hit (Some), never fall through. The
    // mesh's outer boundary is excluded — a ray exactly on an *outer* edge
    // is a legitimate tie the intersector may resolve either way.
    for j in 1..n {
        for i in 1..n {
            let (x, y) = (i as f32 / n as f32, j as f32 / n as f32);
            let probes = [
                (x, y),
                (x + 0.5 / n as f32, y),
                (x, y + 0.5 / n as f32),
                (x + 0.5 / n as f32, y + 0.5 / n as f32),
            ];
            for (px, py) in probes {
                if px <= 0.0 || px >= 1.0 || py <= 0.0 || py >= 1.0 {
                    continue;
                }
                let ray = Ray::new(Vec3A::new(px, py, 1.0), -Vec3A::Z);
                let hit = scene.intersect(&ray, 1e-4, 10.0);
                assert!(hit.is_some(), "pinhole at ({px}, {py})");
                assert!((hit.unwrap().t - 1.0).abs() < 1e-5);
                assert!(scene.occluded(&ray, 1e-4, 10.0));
            }
        }
    }
}

#[test]
fn axis_aligned_quads_survive_the_slab_test() {
    // Zero-thickness geometry in each of the three axis planes: the bounds
    // padding must keep every one hittable by a ray along its normal.
    let planes = [
        (Vec3A::X, Vec3A::Y, Vec3A::Z), // z = 0 plane
        (Vec3A::Y, Vec3A::Z, Vec3A::X), // x = 0 plane
        (Vec3A::Z, Vec3A::X, Vec3A::Y), // y = 0 plane
    ];
    for (a, bb, normal) in planes {
        let mut b = SceneBuilder::new();
        b.attach(mesh(
            vec![-a - bb, a - bb, a + bb, -a + bb],
            vec![[0, 1, 2], [0, 2, 3]],
        ));
        let scene = b.commit();
        let ray = Ray::new(normal * 3.0 + 0.1 * a, -normal);
        let hit = scene.intersect(&ray, 1e-4, 10.0);
        assert!(hit.is_some(), "quad in plane normal to {normal} was missed");
        assert!((hit.unwrap().t - 3.0).abs() < 1e-5);
    }
}

#[test]
fn quad_hits_report_the_right_fan_triangle() {
    let mut b = SceneBuilder::new();
    b.attach(unit_quad());
    let scene = b.commit();
    let lower = scene
        .intersect(
            &Ray::new(Vec3A::new(0.75, 0.25, -1.0), Vec3A::Z),
            1e-4,
            10.0,
        )
        .unwrap();
    assert_eq!(lower.prim_id, 0);
    let upper = scene
        .intersect(
            &Ray::new(Vec3A::new(0.25, 0.75, -1.0), Vec3A::Z),
            1e-4,
            10.0,
        )
        .unwrap();
    assert_eq!(upper.prim_id, 1);
}

// ---------------------------------------------------------------------------
// Determinism and thread safety
// ---------------------------------------------------------------------------

#[test]
fn committing_the_same_input_twice_gives_identical_answers() {
    let build = || {
        let mut rng = Lcg::new(99);
        let mut b = SceneBuilder::new();
        for _ in 0..3000 {
            b.attach(sphere(rng.vec(-20.0, 20.0), rng.range(0.1, 1.0)));
        }
        let (v, t) = grid(20);
        b.attach(mesh(v, t));
        b.commit()
    };
    let (a, b) = (build(), build());
    assert_eq!(a.primitive_count(), b.primitive_count());
    let mut rng = Lcg::new(100);
    for _ in 0..500 {
        let ray = Ray::new(rng.vec(-25.0, 25.0), rng.dir());
        let ha = a.intersect(&ray, 1e-3, 100.0);
        let hb = b.intersect(&ray, 1e-3, 100.0);
        match (ha, hb) {
            (None, None) => {}
            (Some(x), Some(y)) => {
                assert_eq!(x.t.to_bits(), y.t.to_bits());
                assert_eq!(x.geom_id, y.geom_id);
                assert_eq!(x.prim_id, y.prim_id);
                assert_eq!(x.normal, y.normal);
            }
            _ => panic!("two builds of the same input disagree"),
        }
    }
    let fa = a.memory_footprint();
    let fb = b.memory_footprint();
    assert_eq!(fa, fb, "deterministic builds have identical footprints");
}

#[test]
fn scene_queries_are_safe_from_many_threads() {
    let mut rng = Lcg::new(7);
    let mut b = SceneBuilder::new();
    for _ in 0..500 {
        b.attach(sphere(rng.vec(-5.0, 5.0), rng.range(0.1, 0.5)));
    }
    let scene = Arc::new(b.commit());
    let rays: Vec<Ray> = (0..2000)
        .map(|_| Ray::new(rng.vec(-8.0, 8.0), rng.dir()))
        .collect();
    let reference: Vec<Option<(u32, u32)>> = rays
        .iter()
        .map(|r| {
            scene
                .intersect(r, 1e-3, 50.0)
                .map(|h| (h.t.to_bits(), h.geom_id))
        })
        .collect();
    std::thread::scope(|s| {
        for chunk in 0..8 {
            let scene = Arc::clone(&scene);
            let rays = &rays;
            let reference = &reference;
            s.spawn(move || {
                for (i, r) in rays.iter().enumerate().skip(chunk).step_by(8) {
                    let got = scene
                        .intersect(r, 1e-3, 50.0)
                        .map(|h| (h.t.to_bits(), h.geom_id));
                    assert_eq!(got, reference[i]);
                }
            });
        }
    });
}

// ---------------------------------------------------------------------------
// Masks
// ---------------------------------------------------------------------------

#[test]
fn indirect_only_geometry_hides_from_camera_and_shadow_rays() {
    let mut b = SceneBuilder::new();
    b.attach_masked(sphere(Vec3A::ZERO, 1.0), MASK_INDIRECT);
    let scene = b.commit();
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    assert!(
        scene
            .intersect(&ray.with_mask(MASK_CAMERA), 1e-3, 10.0)
            .is_none()
    );
    assert!(
        scene
            .intersect(&ray.with_mask(MASK_SHADOW), 1e-3, 10.0)
            .is_none()
    );
    assert!(
        scene
            .intersect(&ray.with_mask(MASK_INDIRECT), 1e-3, 10.0)
            .is_some()
    );
    // A ray carrying every bit sees it too.
    assert!(scene.intersect(&ray, 1e-3, 10.0).is_some());
}

#[test]
fn masked_geometry_does_not_shadow_the_closest_hit_behind_it() {
    // A camera-invisible sphere in front of a visible one: the camera ray
    // must reach the visible one, not stop early.
    let mut b = SceneBuilder::new();
    b.attach_masked(sphere(Vec3A::new(0.0, 0.0, -2.0), 0.5), MASK_SHADOW);
    let visible = b.attach(sphere(Vec3A::ZERO, 0.5));
    let scene = b.commit();
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -6.0), Vec3A::Z).with_mask(MASK_CAMERA);
    let hit = scene.intersect(&ray, 1e-3, 100.0).unwrap();
    assert_eq!(hit.geom_id, visible);
    assert!((hit.t - 5.5).abs() < 1e-4);
    // The shadow ray stops at the first.
    let hit = scene
        .intersect(&ray.with_mask(MASK_SHADOW), 1e-3, 100.0)
        .unwrap();
    assert!((hit.t - 3.5).abs() < 1e-4);
}

// ---------------------------------------------------------------------------
// Curves
// ---------------------------------------------------------------------------

fn segment(p0: Vec3A, p1: Vec3A, r0: f32, r1: f32) -> Geometry {
    Geometry::RoundCurves {
        segments: vec![CurveSegment { p0, p1, r0, r1 }],
    }
}

#[test]
fn round_curve_body_is_hit_like_a_cylinder() {
    let mut b = SceneBuilder::new();
    b.attach(segment(
        Vec3A::new(-1.0, 0.0, 0.0),
        Vec3A::new(1.0, 0.0, 0.0),
        0.5,
        0.5,
    ));
    let scene = b.commit();
    let hit = scene
        .intersect(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z), 1e-3, 100.0)
        .expect("ray through the axis hits the body");
    assert!((hit.t - 4.5).abs() < 1e-4, "t = {}", hit.t);
    assert!(hit.normal.abs_diff_eq(-Vec3A::Z, 1e-4));
    assert!(hit.front_face);
    // Off the body but grazing at 0.4 in y: still inside radius 0.5.
    let graze = scene
        .intersect(&Ray::new(Vec3A::new(0.0, 0.4, -5.0), Vec3A::Z), 1e-3, 100.0)
        .unwrap();
    assert!((graze.t - (5.0 - 0.3)).abs() < 1e-3);
    // Beyond the radius: miss.
    assert!(
        scene
            .intersect(&Ray::new(Vec3A::new(0.0, 0.6, -5.0), Vec3A::Z), 1e-3, 100.0)
            .is_none()
    );
}

#[test]
fn round_curve_end_caps_are_spherical() {
    let mut b = SceneBuilder::new();
    b.attach(segment(
        Vec3A::new(-1.0, 0.0, 0.0),
        Vec3A::new(1.0, 0.0, 0.0),
        0.5,
        0.5,
    ));
    let scene = b.commit();
    // Past the end point along the axis, but within the cap sphere.
    let cap = scene
        .intersect(&Ray::new(Vec3A::new(1.3, 0.0, -5.0), Vec3A::Z), 1e-3, 100.0)
        .expect("the cap sphere at (1,0,0) covers x = 1.3");
    let expected = 5.0 - (0.25f32 - 0.09).sqrt();
    assert!(
        (cap.t - expected).abs() < 1e-3,
        "t = {} want {expected}",
        cap.t
    );
    // Head-on down the axis hits the cap at x = 1.5.
    let axial = scene
        .intersect(&Ray::new(Vec3A::new(5.0, 0.0, 0.0), -Vec3A::X), 1e-3, 100.0)
        .unwrap();
    assert!((axial.t - 3.5).abs() < 1e-4);
    assert!(axial.normal.abs_diff_eq(Vec3A::X, 1e-4));
    assert!(
        scene
            .intersect(&Ray::new(Vec3A::new(1.6, 0.0, -5.0), Vec3A::Z), 1e-3, 100.0)
            .is_none()
    );
}

#[test]
fn tapered_curve_radius_shrinks_along_the_segment() {
    let mut b = SceneBuilder::new();
    b.attach(segment(
        Vec3A::new(-1.0, 0.0, 0.0),
        Vec3A::new(1.0, 0.0, 0.0),
        0.5,
        0.1,
    ));
    let scene = b.commit();
    let thick = scene
        .intersect(
            &Ray::new(Vec3A::new(-0.8, 0.0, -5.0), Vec3A::Z),
            1e-3,
            100.0,
        )
        .unwrap();
    let thin = scene
        .intersect(&Ray::new(Vec3A::new(0.8, 0.0, -5.0), Vec3A::Z), 1e-3, 100.0)
        .unwrap();
    // A larger radius means the surface is met sooner.
    assert!(thick.t < thin.t, "{} vs {}", thick.t, thin.t);
    assert!(thick.t > 4.4 && thick.t < 4.6);
    assert!(thin.t > 4.8 && thin.t < 4.95);
}

#[test]
fn curve_batches_report_prim_ids_in_order() {
    let segments: Vec<CurveSegment> = (0..5)
        .map(|i| CurveSegment {
            p0: Vec3A::new(i as f32 * 2.0, -1.0, 0.0),
            p1: Vec3A::new(i as f32 * 2.0, 1.0, 0.0),
            r0: 0.3,
            r1: 0.3,
        })
        .collect();
    let mut b = SceneBuilder::new();
    let id = b.attach(Geometry::RoundCurves { segments });
    let scene = b.commit();
    assert_eq!(scene.primitive_count(), 5);
    assert_eq!(scene.primitive_breakdown().curve_segments, 5);
    for i in 0..5 {
        let hit = scene
            .intersect(
                &Ray::new(Vec3A::new(i as f32 * 2.0, 0.0, -5.0), Vec3A::Z),
                1e-3,
                100.0,
            )
            .unwrap();
        assert_eq!(hit.geom_id, id);
        assert_eq!(hit.prim_id, i);
    }
}

#[test]
fn a_straight_cubic_span_matches_the_linear_segment() {
    let p0 = Vec3A::new(-1.0, 0.0, 0.0);
    let p1 = Vec3A::new(1.0, 0.0, 0.0);
    let mut lin = SceneBuilder::new();
    lin.attach(segment(p0, p1, 0.4, 0.4));
    let lin = lin.commit();
    let mut cub = SceneBuilder::new();
    cub.attach(Geometry::CubicCurves {
        segments: vec![CubicCurveSegment {
            cp: [p0, p0.lerp(p1, 1.0 / 3.0), p0.lerp(p1, 2.0 / 3.0), p1],
            r0: 0.4,
            r1: 0.4,
        }],
    });
    let cub = cub.commit();
    assert_eq!(cub.primitive_breakdown().cubic_curve_spans, 1);
    for x in [-0.7f32, -0.2, 0.0, 0.3, 0.9] {
        for y in [0.0f32, 0.2, -0.3] {
            let ray = Ray::new(Vec3A::new(x, y, -5.0), Vec3A::Z);
            let a = lin.intersect(&ray, 1e-3, 100.0).unwrap();
            let b = cub.intersect(&ray, 1e-3, 100.0).unwrap();
            assert!((a.t - b.t).abs() < 1e-2, "at ({x},{y}): {} vs {}", a.t, b.t);
        }
    }
    assert!(
        cub.intersect(&Ray::new(Vec3A::new(0.0, 0.6, -5.0), Vec3A::Z), 1e-3, 100.0)
            .is_none()
    );
}

#[test]
fn a_bent_cubic_span_is_hit_where_it_bends() {
    // Control points arch up to y ≈ 0.75 at the middle.
    let mut b = SceneBuilder::new();
    b.attach(Geometry::CubicCurves {
        segments: vec![CubicCurveSegment {
            cp: [
                Vec3A::new(-1.0, 0.0, 0.0),
                Vec3A::new(-0.3, 1.0, 0.0),
                Vec3A::new(0.3, 1.0, 0.0),
                Vec3A::new(1.0, 0.0, 0.0),
            ],
            r0: 0.15,
            r1: 0.15,
        }],
    });
    let scene = b.commit();
    // At x = 0 the Bézier passes through y = 0.75: a ray there hits...
    assert!(
        scene
            .intersect(
                &Ray::new(Vec3A::new(0.0, 0.75, -5.0), Vec3A::Z),
                1e-3,
                100.0
            )
            .is_some()
    );
    // ...while the straight chord (y = 0) is empty.
    assert!(
        scene
            .intersect(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z), 1e-3, 100.0)
            .is_none()
    );
    let bb = scene.bounds().unwrap();
    assert!(bb.maximum.y >= 0.75 + 0.15 - 1e-3);
}

// ---------------------------------------------------------------------------
// Instances
// ---------------------------------------------------------------------------

fn instance(scene: Arc<Scene>, transform: Affine3A) -> Geometry {
    Geometry::Instance {
        scene,
        transform,
        transform_end: None,
    }
}

#[test]
fn uniformly_scaled_instance_scales_the_radius() {
    let mut b = SceneBuilder::new();
    b.attach(instance(
        unit_sphere_scene(),
        Affine3A::from_scale(Vec3::splat(2.0)),
    ));
    let scene = b.commit();
    let hit = scene
        .intersect(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z), 1e-3, 100.0)
        .unwrap();
    assert!((hit.t - 3.0).abs() < 1e-4);
    assert!(hit.normal.abs_diff_eq(-Vec3A::Z, 1e-4));
    let bb = scene.bounds().unwrap();
    assert!(bb.minimum.abs_diff_eq(Vec3A::splat(-2.0), 1e-4));
    assert!(bb.maximum.abs_diff_eq(Vec3A::splat(2.0), 1e-4));
}

#[test]
fn instancing_an_empty_scene_adds_no_primitive() {
    let empty = Arc::new(SceneBuilder::new().commit());
    let mut b = SceneBuilder::new();
    b.attach(instance(empty, Affine3A::IDENTITY));
    let scene = b.commit();
    assert_eq!(scene.geometry_count(), 1);
    assert_eq!(scene.primitive_count(), 0);
    assert!(scene.bounds().is_none());
}

#[test]
fn instance_t_is_in_world_units_whatever_the_scale() {
    // Direction is unnormalized inside the instance; t must still be the
    // world parameter so nested and top-level hits compare directly.
    let mut b = SceneBuilder::new();
    b.attach(instance(
        unit_sphere_scene(),
        Affine3A::from_scale_rotation_translation(
            Vec3::new(0.5, 3.0, 0.5),
            glam::Quat::from_rotation_x(0.7),
            Vec3::new(1.0, 2.0, 3.0),
        ),
    ));
    let scene = b.commit();
    let ray = Ray::new(Vec3A::new(1.0, 2.0, -10.0), Vec3A::new(0.0, 0.0, 2.0));
    let hit = scene.intersect(&ray, 1e-3, 100.0).unwrap();
    let p = ray.at(hit.t);
    // Map back into local space: it must lie on the unit sphere.
    let local = Affine3A::from_scale_rotation_translation(
        Vec3::new(0.5, 3.0, 0.5),
        glam::Quat::from_rotation_x(0.7),
        Vec3::new(1.0, 2.0, 3.0),
    )
    .inverse()
    .transform_point3a(p);
    assert!((local.length() - 1.0).abs() < 1e-3, "local {local}");
    assert!((hit.normal.length() - 1.0).abs() < 1e-4);
}

#[test]
fn many_instances_of_one_prototype_hit_independently() {
    let proto = unit_sphere_scene();
    let mut b = SceneBuilder::new();
    let n = 400;
    for i in 0..n {
        let x = (i % 20) as f32 * 3.0;
        let y = (i / 20) as f32 * 3.0;
        b.attach(instance(
            Arc::clone(&proto),
            Affine3A::from_translation(Vec3::new(x, y, 0.0)),
        ));
    }
    let scene = b.commit();
    assert_eq!(scene.primitive_count(), n as usize);
    assert_eq!(scene.primitive_breakdown().instances, n as usize);
    let unique = scene.unique_primitive_breakdown();
    assert_eq!(unique.spheres, 1);
    assert_eq!(unique.instances, n as usize);
    for i in (0..n).step_by(23) {
        let x = (i % 20) as f32 * 3.0;
        let y = (i / 20) as f32 * 3.0;
        let hit = scene
            .intersect(&Ray::new(Vec3A::new(x, y, -9.0), Vec3A::Z), 1e-3, 100.0)
            .unwrap();
        assert_eq!(hit.geom_id, i);
        assert!((hit.t - 8.0).abs() < 1e-4);
    }
    // Between two spheres: nothing.
    assert!(
        scene
            .intersect(&Ray::new(Vec3A::new(1.5, 0.0, -9.0), Vec3A::Z), 1e-3, 100.0)
            .is_none()
    );
}

#[test]
fn motion_blur_lerps_at_intermediate_shutter_times() {
    let mut b = SceneBuilder::new();
    b.attach(Geometry::Instance {
        scene: unit_sphere_scene(),
        transform: Affine3A::IDENTITY,
        transform_end: Some(Box::new(Affine3A::from_translation(Vec3::new(
            8.0, 0.0, 0.0,
        )))),
    });
    let scene = b.commit();
    assert!(scene.has_motion());
    for (time, x) in [
        (0.0f32, 0.0f32),
        (0.25, 2.0),
        (0.5, 4.0),
        (0.75, 6.0),
        (1.0, 8.0),
    ] {
        let ray = Ray::new(Vec3A::new(x, 0.0, -5.0), Vec3A::Z).with_time(time);
        let hit = scene.intersect(&ray, 1e-3, 100.0);
        assert!(hit.is_some(), "time {time}: sphere should be at x = {x}");
        assert!((hit.unwrap().t - 4.0).abs() < 1e-4);
        // Where it was at another time, it is not now.
        let elsewhere = Ray::new(Vec3A::new(x + 4.0, 0.0, -5.0), Vec3A::Z).with_time(time);
        assert!(scene.intersect(&elsewhere, 1e-3, 100.0).is_none());
    }
}

#[test]
fn static_instance_bounds_equal_the_transformed_inner_bounds() {
    let mut b = SceneBuilder::new();
    b.attach(instance(
        unit_sphere_scene(),
        Affine3A::from_translation(Vec3::new(10.0, -3.0, 2.0)),
    ));
    let bb = b.commit().bounds().unwrap();
    assert!(bb.minimum.abs_diff_eq(Vec3A::new(9.0, -4.0, 1.0), 1e-4));
    assert!(bb.maximum.abs_diff_eq(Vec3A::new(11.0, -2.0, 3.0), 1e-4));
}

#[test]
fn instance_normal_maps_through_a_rotation() {
    let mut inner = SceneBuilder::new();
    inner.attach(unit_quad());
    let inner = Arc::new(inner.commit());
    let mut b = SceneBuilder::new();
    // Rotate the quad (normal +Z) by 90° about X: its normal becomes -Y.
    b.attach(instance(
        inner,
        Affine3A::from_rotation_x(std::f32::consts::FRAC_PI_2),
    ));
    let scene = b.commit();
    // Local (0.5, 0.5, 0) maps to (0.5, 0, 0.5). Probe from -Y.
    let hit = scene
        .intersect(&Ray::new(Vec3A::new(0.5, -3.0, 0.5), Vec3A::Y), 1e-3, 100.0)
        .expect("rotated quad");
    assert!((hit.t - 3.0).abs() < 1e-4);
    assert!(
        hit.normal.abs_diff_eq(-Vec3A::Y, 1e-4),
        "normal {}",
        hit.normal
    );
    assert!(hit.front_face);
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

#[test]
fn breakdown_counts_every_kind_of_top_level_primitive() {
    let (v, t) = grid(2);
    let mut b = SceneBuilder::new();
    b.attach(mesh(v, t)); // 8 triangles
    b.attach(sphere(Vec3A::ZERO, 1.0));
    b.attach(sphere(Vec3A::X, 1.0));
    b.attach(Geometry::RoundCurves {
        segments: vec![
            CurveSegment {
                p0: Vec3A::ZERO,
                p1: Vec3A::Y,
                r0: 0.1,
                r1: 0.1,
            },
            CurveSegment {
                p0: Vec3A::Y,
                p1: Vec3A::Y * 2.0,
                r0: 0.1,
                r1: 0.1,
            },
            CurveSegment {
                p0: Vec3A::X,
                p1: Vec3A::X * 2.0,
                r0: 0.1,
                r1: 0.1,
            },
        ],
    });
    b.attach(Geometry::CubicCurves {
        segments: vec![CubicCurveSegment {
            cp: [Vec3A::ZERO, Vec3A::X, Vec3A::X * 2.0, Vec3A::X * 3.0],
            r0: 0.1,
            r1: 0.1,
        }],
    });
    b.attach(instance(
        unit_sphere_scene(),
        Affine3A::from_translation(Vec3::Z * 9.0),
    ));
    let scene = b.commit();
    let br = scene.primitive_breakdown();
    assert_eq!(br.triangles, 8);
    assert_eq!(br.spheres, 2);
    assert_eq!(br.curve_segments, 3);
    assert_eq!(br.cubic_curve_spans, 1);
    assert_eq!(br.instances, 1);
    assert_eq!(scene.primitive_count(), 8 + 2 + 3 + 1 + 1);
    assert_eq!(scene.geometry_count(), 6);
    // The unique view descends into the one instance: one more sphere.
    let u = scene.unique_primitive_breakdown();
    assert_eq!(u.spheres, 3);
    assert_eq!(u.instances, 1);
    assert_eq!(u.triangles, 8);
}

#[test]
fn unique_breakdown_equals_top_level_without_instancing() {
    let (v, t) = grid(4);
    let mut b = SceneBuilder::new();
    b.attach(mesh(v, t));
    b.attach(sphere(Vec3A::ZERO, 1.0));
    let scene = b.commit();
    assert_eq!(
        scene.primitive_breakdown(),
        scene.unique_primitive_breakdown()
    );
}

#[test]
fn memory_footprint_total_sums_its_fields_and_grows_with_geometry() {
    let mut small = SceneBuilder::new();
    small.attach(sphere(Vec3A::ZERO, 1.0));
    let small = small.commit().memory_footprint();
    assert_eq!(
        small.total(),
        small.prim_nodes
            + small.boxed_prims
            + small.bvh_nodes
            + small.leaves
            + small.packets
            + small.indices
    );
    assert!(small.total() > 0);

    let (v, t) = grid(30);
    let mut big = SceneBuilder::new();
    big.attach(mesh(v, t));
    let big = big.commit().memory_footprint();
    assert!(big.total() > small.total());
    assert!(big.packets > 0, "triangles are packed into SIMD packets");
    assert_eq!(small.packets, 0, "a sphere leaf has no triangle packet");
}

#[test]
fn shared_instanced_scenes_are_counted_once_in_the_footprint() {
    let (v, t) = grid(20);
    let mut proto = SceneBuilder::new();
    proto.attach(mesh(v, t));
    let proto = Arc::new(proto.commit());
    let proto_bytes = proto.memory_footprint().total();

    let build = |n: usize| {
        let mut b = SceneBuilder::new();
        for i in 0..n {
            b.attach(instance(
                Arc::clone(&proto),
                Affine3A::from_translation(Vec3::new(i as f32 * 5.0, 0.0, 0.0)),
            ));
        }
        b.commit().memory_footprint().total()
    };
    let one = build(1);
    let hundred = build(100);
    assert!(one >= proto_bytes, "the instanced scene is included");
    // 99 more placements cost 99 instance nodes, not 99 copies of the mesh.
    assert!(
        hundred - one < proto_bytes,
        "prototype counted more than once: {one} -> {hundred}"
    );
}

#[test]
fn primitive_extents_describe_prims_relative_to_the_scene() {
    let mut b = SceneBuilder::new();
    b.attach(sphere(Vec3A::ZERO, 1.0));
    let (n, diag, mean, max) = b.commit().primitive_extents();
    assert_eq!(n, 1);
    let d = (12.0f32).sqrt(); // diagonal of the [-1,1]³ box
    assert!((diag - d).abs() < 1e-4);
    assert!((mean - d).abs() < 1e-4);
    assert!((max - d).abs() < 1e-4);

    // Two small spheres far apart: the mean prim diagonal is a small
    // fraction of the scene diagonal.
    let mut b = SceneBuilder::new();
    b.attach(sphere(Vec3A::ZERO, 0.1));
    b.attach(sphere(Vec3A::X * 100.0, 0.1));
    let (n, diag, mean, max) = b.commit().primitive_extents();
    assert_eq!(n, 2);
    assert!(diag > 100.0);
    assert!(mean / diag < 0.01);
    assert!((max - mean).abs() < 1e-5, "both prims are the same size");
}

#[test]
fn bounds_cover_every_attached_geometry() {
    let mut b = SceneBuilder::new();
    b.attach(sphere(Vec3A::new(-5.0, 0.0, 0.0), 1.0));
    b.attach(unit_quad());
    b.attach(segment(
        Vec3A::new(0.0, 0.0, 7.0),
        Vec3A::new(0.0, 0.0, 9.0),
        0.5,
        0.5,
    ));
    let bb = b.commit().bounds().unwrap();
    assert!(bb.minimum.x <= -6.0 && bb.maximum.x >= 1.0);
    assert!(bb.minimum.y <= -1.0 && bb.maximum.y >= 1.0);
    assert!(bb.minimum.z <= -1.0 && bb.maximum.z >= 9.5);
}
