//! Behavioural tests for the `utils` helpers: sampling warps, MIS heuristics
//! and the small math conveniences the renderer leans on everywhere.
//!
//! The distribution checks use a deterministic LCG rather than `rand` so a
//! failure reproduces bit for bit.

use glam::Vec3A;
use std::f32::consts::{FRAC_PI_2, PI};
use utils::*;

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
}

fn approx(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

// ---------------------------------------------------------------------------
// Scalars
// ---------------------------------------------------------------------------

#[test]
fn degrees_to_radians_hits_the_landmarks() {
    assert_eq!(degrees_to_radians(0.0), 0.0);
    assert!(approx(degrees_to_radians(180.0), PI, 1e-6));
    assert!(approx(degrees_to_radians(90.0), FRAC_PI_2, 1e-6));
    assert!(approx(degrees_to_radians(-45.0), -PI / 4.0, 1e-6));
    assert!(approx(degrees_to_radians(360.0), 2.0 * PI, 1e-5));
}

#[test]
fn clamp_respects_both_bounds() {
    assert_eq!(clamp(0.5, 0.0, 1.0), 0.5);
    assert_eq!(clamp(-3.0, 0.0, 1.0), 0.0);
    assert_eq!(clamp(7.0, 0.0, 1.0), 1.0);
    assert_eq!(clamp(0.0, 0.0, 1.0), 0.0);
    assert_eq!(clamp(1.0, 0.0, 1.0), 1.0);
    // Negative ranges work the same way.
    assert_eq!(clamp(-5.0, -2.0, -1.0), -2.0);
    assert_eq!(clamp(-1.5, -2.0, -1.0), -1.5);
}

#[test]
fn lerp_interpolates_linearly() {
    assert_eq!(2.0f32.lerp(4.0, 0.0), 2.0);
    assert_eq!(2.0f32.lerp(4.0, 1.0), 4.0);
    assert!(approx(2.0f32.lerp(4.0, 0.5), 3.0, 1e-6));
    assert!(approx(2.0f32.lerp(4.0, 0.25), 2.5, 1e-6));
    // Extrapolation is allowed and linear.
    assert!(approx(0.0f32.lerp(1.0, 2.0), 2.0, 1e-6));
}

#[test]
fn lerp_is_symmetric_under_endpoint_swap() {
    for t in [0.0f32, 0.1, 0.5, 0.9, 1.0] {
        let ab = 1.0f32.lerp(5.0, t);
        let ba = 5.0f32.lerp(1.0, 1.0 - t);
        assert!(approx(ab, ba, 1e-5), "t={t}: {ab} vs {ba}");
    }
}

// ---------------------------------------------------------------------------
// MIS heuristics
// ---------------------------------------------------------------------------

#[test]
fn balance_heuristic_splits_equal_densities_evenly() {
    assert!(approx(balance_heuristic(1.0, 1.0), 0.5, 1e-5));
    assert!(approx(balance_heuristic(3.0, 3.0), 0.5, 1e-5));
}

#[test]
fn balance_heuristic_favours_the_denser_strategy() {
    assert!(balance_heuristic(9.0, 1.0) > 0.85);
    assert!(balance_heuristic(1.0, 9.0) < 0.15);
    // A zero competitor gives (almost) full weight.
    assert!(balance_heuristic(1.0, 0.0) > 0.999);
    assert!(balance_heuristic(0.0, 1.0) < 1e-5);
}

#[test]
fn balance_weights_partition_unity() {
    for (a, b) in [(1.0f32, 1.0f32), (0.2, 5.0), (100.0, 0.01), (7.0, 3.0)] {
        let sum = balance_heuristic(a, b) + balance_heuristic(b, a);
        assert!(approx(sum, 1.0, 1e-4), "a={a} b={b}: {sum}");
    }
}

#[test]
fn power_heuristic_splits_equal_densities_evenly() {
    assert!(approx(power_heuristic(1.0, 1.0), 0.5, 1e-5));
    assert!(approx(power_heuristic(0.3, 0.3), 0.5, 1e-4));
}

#[test]
fn power_weights_partition_unity() {
    for (a, b) in [(1.0f32, 1.0f32), (0.2, 5.0), (100.0, 0.01), (7.0, 3.0)] {
        let sum = power_heuristic(a, b) + power_heuristic(b, a);
        assert!(approx(sum, 1.0, 1e-4), "a={a} b={b}: {sum}");
    }
}

#[test]
fn power_heuristic_sharpens_the_balance_heuristic() {
    // β = 2 pushes the weight further toward the denser strategy.
    let (a, b) = (4.0, 1.0);
    assert!(power_heuristic(a, b) > balance_heuristic(a, b));
    assert!(power_heuristic(b, a) < balance_heuristic(b, a));
    // Concretely: 16 / 17 vs 4 / 5.
    assert!(approx(power_heuristic(a, b), 16.0 / 17.0, 1e-4));
    assert!(approx(balance_heuristic(a, b), 0.8, 1e-4));
}

#[test]
fn heuristics_stay_finite_at_zero() {
    for f in [balance_heuristic, power_heuristic] {
        let w = f(0.0, 0.0);
        assert!(w.is_finite());
        assert!((0.0..=1.0).contains(&w));
    }
}

// ---------------------------------------------------------------------------
// Sampling warps
// ---------------------------------------------------------------------------

#[test]
fn cosine_hemisphere_is_unit_length_and_upper() {
    let n = 32;
    for i in 0..n {
        for j in 0..n {
            let uv = [(i as f32 + 0.5) / n as f32, (j as f32 + 0.5) / n as f32];
            let d = cosine_hemisphere(uv);
            assert!(approx(d.length(), 1.0, 1e-4), "{uv:?} -> {d}");
            assert!(d.z >= 0.0, "{uv:?} -> {d}");
        }
    }
}

#[test]
fn cosine_hemisphere_origin_maps_to_the_pole() {
    let d = cosine_hemisphere([0.0, 0.0]);
    assert!(d.abs_diff_eq(Vec3A::Z, 1e-6));
}

#[test]
fn cosine_hemisphere_has_the_cosine_weighted_mean() {
    // E[cos θ] under a cosine-weighted hemisphere is 2/3.
    let mut rng = Lcg::new(1);
    let n = 200_000;
    let mut sum = 0.0f64;
    for _ in 0..n {
        sum += cosine_hemisphere([rng.next(), rng.next()]).z as f64;
    }
    let mean = sum / n as f64;
    assert!((mean - 2.0 / 3.0).abs() < 0.005, "mean cos = {mean}");
}

#[test]
fn cosine_hemisphere_is_rotationally_symmetric_in_azimuth() {
    let mut rng = Lcg::new(2);
    let n = 200_000;
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    for _ in 0..n {
        let d = cosine_hemisphere([rng.next(), rng.next()]);
        sx += d.x as f64;
        sy += d.y as f64;
    }
    assert!((sx / n as f64).abs() < 0.005);
    assert!((sy / n as f64).abs() < 0.005);
}

#[test]
fn uniform_sphere_is_unit_length_everywhere() {
    let n = 40;
    for i in 0..=n {
        for j in 0..=n {
            let uv = [i as f32 / n as f32, j as f32 / n as f32];
            let d = uniform_sphere(uv);
            assert!(approx(d.length(), 1.0, 1e-4), "{uv:?} -> {d}");
        }
    }
}

#[test]
fn uniform_sphere_poles_come_from_the_ends_of_u() {
    assert!(uniform_sphere([0.0, 0.3]).abs_diff_eq(Vec3A::Z, 1e-6));
    assert!(uniform_sphere([1.0, 0.7]).abs_diff_eq(-Vec3A::Z, 1e-6));
    // Halfway in u sits on the equator.
    assert!(uniform_sphere([0.5, 0.0]).z.abs() < 1e-6);
}

#[test]
fn uniform_sphere_has_zero_mean_and_isotropic_second_moment() {
    let mut rng = Lcg::new(3);
    let n = 200_000;
    let mut mean = [0.0f64; 3];
    let mut sq = [0.0f64; 3];
    for _ in 0..n {
        let d = uniform_sphere([rng.next(), rng.next()]);
        for (k, c) in [d.x, d.y, d.z].iter().enumerate() {
            mean[k] += *c as f64;
            sq[k] += (*c as f64) * (*c as f64);
        }
    }
    for k in 0..3 {
        assert!((mean[k] / n as f64).abs() < 0.006, "axis {k} mean");
        // E[x²] = 1/3 on the unit sphere.
        assert!(
            (sq[k] / n as f64 - 1.0 / 3.0).abs() < 0.006,
            "axis {k} second moment"
        );
    }
}

#[test]
fn uniform_ball_stays_inside_the_unit_ball() {
    let mut rng = Lcg::new(4);
    for _ in 0..10_000 {
        let p = uniform_ball([rng.next(), rng.next(), rng.next()]);
        assert!(p.length() <= 1.0 + 1e-5, "{p}");
    }
}

#[test]
fn uniform_ball_radius_warp_is_volumetric() {
    // With r = w^(1/3), r³ is uniform on [0, 1] so E[r³] = 1/2, and
    // E[r] = 3/4.
    let mut rng = Lcg::new(5);
    let n = 200_000;
    let (mut r3, mut r1) = (0.0f64, 0.0f64);
    for _ in 0..n {
        let r = uniform_ball([rng.next(), rng.next(), rng.next()]).length() as f64;
        r3 += r * r * r;
        r1 += r;
    }
    assert!((r3 / n as f64 - 0.5).abs() < 0.005);
    assert!((r1 / n as f64 - 0.75).abs() < 0.005);
}

#[test]
fn uniform_ball_endpoints_of_w() {
    assert_eq!(uniform_ball([0.3, 0.6, 0.0]), Vec3A::ZERO);
    assert!(approx(uniform_ball([0.3, 0.6, 1.0]).length(), 1.0, 1e-5));
    // Negative radius input is clamped rather than producing NaN.
    let p = uniform_ball([0.3, 0.6, -1.0]);
    assert!(p.is_finite());
    assert_eq!(p, Vec3A::ZERO);
}

#[test]
fn concentric_disk_stays_in_the_unit_disk_with_zero_z() {
    let n = 50;
    for i in 0..=n {
        for j in 0..=n {
            let p = concentric_disk([i as f32 / n as f32, j as f32 / n as f32]);
            assert!(p.length() <= 1.0 + 1e-5, "{p}");
            assert_eq!(p.z, 0.0);
        }
    }
}

#[test]
fn concentric_disk_maps_the_center_and_edges() {
    assert_eq!(concentric_disk([0.5, 0.5]), Vec3A::ZERO);
    assert!(concentric_disk([1.0, 0.5]).abs_diff_eq(Vec3A::X, 1e-5));
    assert!(concentric_disk([0.0, 0.5]).abs_diff_eq(-Vec3A::X, 1e-5));
    assert!(concentric_disk([0.5, 1.0]).abs_diff_eq(Vec3A::Y, 1e-5));
    assert!(concentric_disk([0.5, 0.0]).abs_diff_eq(-Vec3A::Y, 1e-5));
}

#[test]
fn concentric_disk_is_area_preserving() {
    // A uniform square maps to a uniform disk: the fraction inside radius
    // 1/2 is 1/4 and inside radius 1/√2 is 1/2.
    let mut rng = Lcg::new(6);
    let n = 200_000;
    let (mut quarter, mut half) = (0u32, 0u32);
    for _ in 0..n {
        let r = concentric_disk([rng.next(), rng.next()]).length();
        if r < 0.5 {
            quarter += 1;
        }
        if r < std::f32::consts::FRAC_1_SQRT_2 {
            half += 1;
        }
    }
    assert!((quarter as f64 / n as f64 - 0.25).abs() < 0.005);
    assert!((half as f64 / n as f64 - 0.5).abs() < 0.005);
}

#[test]
fn concentric_disk_is_continuous_across_the_diagonal() {
    // The two branches of the warp meet at |sx| == |sy|; a sample just
    // either side must land at nearly the same point.
    let a = concentric_disk([0.9, 0.9 - 1e-4]);
    let b = concentric_disk([0.9, 0.9 + 1e-4]);
    assert!(a.abs_diff_eq(b, 1e-2), "{a} vs {b}");
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

#[test]
fn align_to_normal_sends_local_z_to_the_normal() {
    for n in [
        Vec3A::new(0.0, 1.0, 0.0),
        Vec3A::new(1.0, 0.0, 0.0),
        Vec3A::new(0.3, -0.5, 0.8).normalize(),
        Vec3A::new(-1.0, -1.0, 0.0).normalize(),
    ] {
        let d = align_to_normal(Vec3A::Z, n);
        assert!(d.abs_diff_eq(n, 1e-5), "normal {n}: {d}");
    }
}

#[test]
fn align_to_normal_handles_the_degenerate_z_axis() {
    // The frame builder switches its `up` when the normal is (nearly)
    // ±Z; the result must still be a proper rotation.
    for n in [Vec3A::Z, -Vec3A::Z] {
        let z = align_to_normal(Vec3A::Z, n);
        assert!(z.abs_diff_eq(n, 1e-5));
        let x = align_to_normal(Vec3A::X, n);
        let y = align_to_normal(Vec3A::Y, n);
        assert!(x.dot(n).abs() < 1e-5);
        assert!(y.dot(n).abs() < 1e-5);
        assert!(x.dot(y).abs() < 1e-5);
    }
}

#[test]
fn align_to_normal_is_orthonormal() {
    let n = Vec3A::new(0.2, 0.9, -0.3).normalize();
    let x = align_to_normal(Vec3A::X, n);
    let y = align_to_normal(Vec3A::Y, n);
    let z = align_to_normal(Vec3A::Z, n);
    for (a, b) in [(x, y), (y, z), (x, z)] {
        assert!(a.dot(b).abs() < 1e-5);
    }
    for v in [x, y, z] {
        assert!(approx(v.length(), 1.0, 1e-5));
    }
}

#[test]
fn align_to_normal_preserves_length_and_linearity() {
    let n = Vec3A::new(0.0, 0.6, 0.8);
    let v = Vec3A::new(0.3, -1.2, 2.0);
    let out = align_to_normal(v, n);
    assert!(approx(out.length(), v.length(), 1e-4));
    let scaled = align_to_normal(2.0 * v, n);
    assert!(scaled.abs_diff_eq(2.0 * out, 1e-4));
    // And the cosine against the normal is preserved: local z is the
    // component along `n`.
    assert!(approx(out.dot(n), v.z, 1e-4));
}

#[test]
fn hemisphere_samples_aligned_to_a_normal_stay_on_its_side() {
    let mut rng = Lcg::new(7);
    let n = Vec3A::new(-0.4, 0.2, -0.7).normalize();
    for _ in 0..5000 {
        let local = cosine_hemisphere([rng.next(), rng.next()]);
        let world = align_to_normal(local, n);
        assert!(world.dot(n) >= -1e-5, "{world} fell below the normal {n}");
    }
}

// ---------------------------------------------------------------------------
// The `rand`-backed helpers: only their ranges are testable.
// ---------------------------------------------------------------------------

#[test]
fn random_lies_in_the_unit_interval() {
    for _ in 0..1000 {
        let r = random();
        assert!((0.0..1.0).contains(&r), "{r}");
    }
}

#[test]
fn random_range_honours_its_bounds() {
    for _ in 0..1000 {
        let r = random_range(-2.0, 3.0);
        assert!((-2.0..3.0).contains(&r), "{r}");
    }
    let (a, b) = random2();
    assert!((0.0..1.0).contains(&a) && (0.0..1.0).contains(&b));
}

#[test]
fn random_vectors_honour_their_domains() {
    for _ in 0..500 {
        let v = random3();
        assert!(v.min_element() >= 0.0 && v.max_element() < 1.0);
        let r = random_range3(2.0, 4.0);
        assert!(r.min_element() >= 2.0 && r.max_element() < 4.0);
        assert!(approx(random_unit_vector().length(), 1.0, 1e-4));
        let d = random_in_unit_disk();
        assert!(d.length() < 1.0 && d.z == 0.0);
        let c = random_cosine_direction();
        assert!(c.z >= 0.0 && approx(c.length(), 1.0, 1e-4));
    }
}

#[test]
fn random_unit_sphere_points_are_inside() {
    let mut rng = rand::rng();
    for _ in 0..500 {
        assert!(random_vec3_unit_sphere(&mut rng).length_squared() < 1.0);
    }
}
