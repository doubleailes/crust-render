//! Free-standing volume regions: density fields, region geometry, phase
//! mixtures, transmittance and the delta-tracking interaction sampler.

use crust_core::{DensityField, PhaseMix, Ray, Vec3A, VolumeEvent, VolumeRegion, Volumes};
use glam::{Mat4, Vec3};
use openqmc::pcg::Rng;

fn approx(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

fn region(l2w: Mat4, half: f32, sigma_s: f32, sigma_a: f32, field: DensityField) -> VolumeRegion {
    VolumeRegion::new(
        l2w,
        Vec3A::splat(half),
        Vec3A::splat(sigma_s),
        Vec3A::splat(sigma_a),
        0.0,
        Vec3A::ZERO,
        1.0,
        field,
    )
}

fn unit_box(sigma_s: f32, sigma_a: f32) -> VolumeRegion {
    region(
        Mat4::IDENTITY,
        1.0,
        sigma_s,
        sigma_a,
        DensityField::Homogeneous,
    )
}

// ---------------------------------------------------------------------------
// Density fields
// ---------------------------------------------------------------------------

#[test]
fn homogeneous_field_is_one_everywhere() {
    let f = DensityField::Homogeneous;
    for p in [
        Vec3A::ZERO,
        Vec3A::ONE,
        Vec3A::splat(0.5),
        Vec3A::new(0.1, 0.9, 0.3),
    ] {
        assert_eq!(f.density(p), 1.0);
    }
    assert_eq!(f.max_value(), 1.0);
}

#[test]
fn grid_field_interpolates_between_voxel_centres() {
    let f = DensityField::Grid {
        nx: 2,
        ny: 1,
        nz: 1,
        data: vec![0.0, 1.0],
    };
    let at = |x: f32| f.density(Vec3A::new(x, 0.5, 0.5));
    assert!(approx(at(0.25), 0.0, 1e-6), "voxel 0 centre");
    assert!(approx(at(0.75), 1.0, 1e-6), "voxel 1 centre");
    assert!(approx(at(0.5), 0.5, 1e-6), "midway");
    assert!(approx(at(0.625), 0.75, 1e-6));
    // Edge clamp: beyond the outer voxel centres the edge value holds.
    assert!(approx(at(0.0), 0.0, 1e-6));
    assert!(approx(at(1.0), 1.0, 1e-6));
    assert!(approx(at(0.1), 0.0, 1e-6));
    assert_eq!(f.max_value(), 1.0);
}

#[test]
fn grid_field_uses_x_fastest_layout() {
    // 2×2×1: index = x + 2·y.
    let f = DensityField::Grid {
        nx: 2,
        ny: 2,
        nz: 1,
        data: vec![1.0, 2.0, 3.0, 4.0],
    };
    assert!(approx(f.density(Vec3A::new(0.25, 0.25, 0.5)), 1.0, 1e-6));
    assert!(approx(f.density(Vec3A::new(0.75, 0.25, 0.5)), 2.0, 1e-6));
    assert!(approx(f.density(Vec3A::new(0.25, 0.75, 0.5)), 3.0, 1e-6));
    assert!(approx(f.density(Vec3A::new(0.75, 0.75, 0.5)), 4.0, 1e-6));
    // The centre averages all four.
    assert!(approx(f.density(Vec3A::splat(0.5)), 2.5, 1e-6));
    assert_eq!(f.max_value(), 4.0);
}

#[test]
fn grid_field_along_z_and_max_value() {
    let f = DensityField::Grid {
        nx: 1,
        ny: 1,
        nz: 3,
        data: vec![0.2, 0.8, 0.5],
    };
    assert!(approx(
        f.density(Vec3A::new(0.5, 0.5, 1.0 / 6.0)),
        0.2,
        1e-5
    ));
    assert!(approx(f.density(Vec3A::new(0.5, 0.5, 0.5)), 0.8, 1e-5));
    assert!(approx(
        f.density(Vec3A::new(0.5, 0.5, 5.0 / 6.0)),
        0.5,
        1e-5
    ));
    assert_eq!(f.max_value(), 0.8);
}

#[test]
fn noise_field_stays_in_range_and_is_deterministic() {
    let f = DensityField::Noise {
        scale: 4.0,
        octaves: 4,
        gain: 0.5,
        lacunarity: 2.0,
        threshold: 0.0,
        seed: 7,
    };
    let g = DensityField::Noise {
        scale: 4.0,
        octaves: 4,
        gain: 0.5,
        lacunarity: 2.0,
        threshold: 0.0,
        seed: 7,
    };
    let mut rng = Rng::new(3);
    let mut sum = 0.0;
    for _ in 0..2000 {
        let p = Vec3A::new(rng.next_f32(), rng.next_f32(), rng.next_f32());
        let d = f.density(p);
        assert!((0.0..=1.0).contains(&d), "{d}");
        assert_eq!(d, g.density(p), "same parameters, same value");
        sum += d;
    }
    let mean = sum / 2000.0;
    assert!(
        mean > 0.2 && mean < 0.8,
        "value noise averages toward the middle: {mean}"
    );
    assert_eq!(f.max_value(), 1.0);
}

#[test]
fn noise_seed_and_threshold_change_the_field() {
    let a = DensityField::Noise {
        scale: 3.0,
        octaves: 3,
        gain: 0.5,
        lacunarity: 2.0,
        threshold: 0.0,
        seed: 1,
    };
    let b = DensityField::Noise {
        scale: 3.0,
        octaves: 3,
        gain: 0.5,
        lacunarity: 2.0,
        threshold: 0.0,
        seed: 2,
    };
    let t = DensityField::Noise {
        scale: 3.0,
        octaves: 3,
        gain: 0.5,
        lacunarity: 2.0,
        threshold: 0.6,
        seed: 1,
    };
    let mut rng = Rng::new(5);
    let (mut differ, mut thresholded_lower) = (0, 0);
    for _ in 0..500 {
        let p = Vec3A::new(rng.next_f32(), rng.next_f32(), rng.next_f32());
        if (a.density(p) - b.density(p)).abs() > 1e-4 {
            differ += 1;
        }
        // Thresholding carves holes: never higher than the raw noise
        // where the raw noise is below one.
        if t.density(p) <= a.density(p) + 1e-6 {
            thresholded_lower += 1;
        }
    }
    assert!(
        differ > 400,
        "different seeds should differ almost everywhere: {differ}"
    );
    assert_eq!(thresholded_lower, 500);
    // A threshold near one leaves almost nothing.
    let sparse = DensityField::Noise {
        scale: 3.0,
        octaves: 3,
        gain: 0.5,
        lacunarity: 2.0,
        threshold: 0.999,
        seed: 1,
    };
    assert!(sparse.density(Vec3A::splat(0.3)) < 0.5);
}

// ---------------------------------------------------------------------------
// Regions
// ---------------------------------------------------------------------------

#[test]
fn region_density_is_zero_outside_its_box() {
    let r = unit_box(0.5, 0.1);
    assert_eq!(r.density(Vec3A::ZERO), 1.0);
    assert_eq!(r.density(Vec3A::splat(0.99)), 1.0);
    assert_eq!(r.density(Vec3A::new(1.01, 0.0, 0.0)), 0.0);
    assert_eq!(r.density(Vec3A::new(0.0, -1.5, 0.0)), 0.0);
    assert!(r.is_homogeneous());
}

#[test]
fn region_intersect_gives_entry_and_exit_distances() {
    let r = unit_box(0.5, 0.1);
    let (t0, t1) = r
        .intersect(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z))
        .unwrap();
    assert!(approx(t0, 4.0, 1e-5) && approx(t1, 6.0, 1e-5));
    // Unnormalized direction: the interval is in ray-parameter units.
    let (t0, t1) = r
        .intersect(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z * 2.0))
        .unwrap();
    assert!(approx(t0, 2.0, 1e-5) && approx(t1, 3.0, 1e-5));
    // Starting inside: the interval begins at zero.
    let (t0, t1) = r.intersect(&Ray::new(Vec3A::ZERO, Vec3A::X)).unwrap();
    assert_eq!(t0, 0.0);
    assert!(approx(t1, 1.0, 1e-5));
    // Missing, and pointing away.
    assert!(
        r.intersect(&Ray::new(Vec3A::new(0.0, 2.0, -5.0), Vec3A::Z))
            .is_none()
    );
    assert!(
        r.intersect(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), -Vec3A::Z))
            .is_none()
    );
    // Axis-parallel ray outside the slab.
    assert!(
        r.intersect(&Ray::new(Vec3A::new(3.0, 0.0, -5.0), Vec3A::Z))
            .is_none()
    );
}

#[test]
fn region_transform_places_and_scales_the_box() {
    let r = region(
        Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0)) * Mat4::from_scale(Vec3::splat(2.0)),
        0.5,
        0.5,
        0.0,
        DensityField::Homogeneous,
    );
    // Local half 0.5 scaled by 2 → world half 1, centred at x = 10.
    assert_eq!(r.density(Vec3A::new(10.0, 0.0, 0.0)), 1.0);
    assert_eq!(r.density(Vec3A::new(10.9, 0.0, 0.0)), 1.0);
    assert_eq!(r.density(Vec3A::new(11.1, 0.0, 0.0)), 0.0);
    assert_eq!(r.density(Vec3A::ZERO), 0.0);
    let (t0, t1) = r
        .intersect(&Ray::new(Vec3A::new(10.0, 0.0, -5.0), Vec3A::Z))
        .unwrap();
    assert!(approx(t0, 4.0, 1e-4) && approx(t1, 6.0, 1e-4));
}

#[test]
fn a_rotated_region_is_hit_on_its_diagonal() {
    // Rotate the unit box 45° about Y: along X its extent is now √2.
    let r = region(
        Mat4::from_rotation_y(std::f32::consts::FRAC_PI_4),
        1.0,
        1.0,
        0.0,
        DensityField::Homogeneous,
    );
    let (t0, t1) = r
        .intersect(&Ray::new(Vec3A::new(-5.0, 0.0, 0.0), Vec3A::X))
        .unwrap();
    let s2 = 2f32.sqrt();
    assert!(
        approx(t0, 5.0 - s2, 1e-4) && approx(t1, 5.0 + s2, 1e-4),
        "{t0} {t1}"
    );
    assert_eq!(
        r.density(Vec3A::new(1.3, 0.0, 0.0)),
        1.0,
        "inside the rotated corner"
    );
    assert_eq!(
        r.density(Vec3A::new(1.3, 0.0, 1.3)),
        0.0,
        "outside the rotated side"
    );
}

#[test]
fn density_scale_folds_into_the_coefficients_and_g_is_clamped() {
    let r = VolumeRegion::new(
        Mat4::IDENTITY,
        Vec3A::ONE,
        Vec3A::new(1.0, 2.0, 3.0),
        Vec3A::new(0.1, 0.2, 0.3),
        5.0,
        Vec3A::new(1.0, 1.0, 1.0),
        2.0,
        DensityField::Homogeneous,
    );
    assert_eq!(r.sigma_s, Vec3A::new(2.0, 4.0, 6.0));
    assert_eq!(r.sigma_a, Vec3A::new(0.2, 0.4, 0.6));
    assert_eq!(r.g, 0.99);
    assert_eq!(r.emission, Vec3A::ONE);
    let neg = region(Mat4::IDENTITY, 1.0, 0.0, 0.0, DensityField::Homogeneous);
    assert_eq!(neg.g, 0.0);
}

#[test]
fn grid_region_reports_non_homogeneous() {
    let r = region(
        Mat4::IDENTITY,
        1.0,
        1.0,
        0.0,
        DensityField::Grid {
            nx: 1,
            ny: 1,
            nz: 1,
            data: vec![0.5],
        },
    );
    assert!(!r.is_homogeneous());
    assert_eq!(r.density(Vec3A::ZERO), 0.5);
    let n = region(
        Mat4::IDENTITY,
        1.0,
        1.0,
        0.0,
        DensityField::Noise {
            scale: 2.0,
            octaves: 2,
            gain: 0.5,
            lacunarity: 2.0,
            threshold: 0.0,
            seed: 0,
        },
    );
    assert!(!n.is_homogeneous());
}

// ---------------------------------------------------------------------------
// Phase mixture
// ---------------------------------------------------------------------------

#[test]
fn isotropic_phase_is_one_over_four_pi() {
    let p = PhaseMix::single(0.0);
    let expected = 1.0 / (4.0 * std::f32::consts::PI);
    for c in [-1.0f32, -0.3, 0.0, 0.7, 1.0] {
        assert!(approx(p.pdf(c), expected, 1e-6));
    }
}

#[test]
fn phase_pdf_integrates_to_one() {
    for g in [-0.6f32, 0.0, 0.3, 0.85] {
        let p = PhaseMix::single(g);
        let n = 20_000;
        let mut sum = 0.0f64;
        for i in 0..n {
            let mu = -1.0 + 2.0 * (i as f32 + 0.5) / n as f32;
            sum += p.pdf(mu) as f64;
        }
        let integral = 2.0 * std::f64::consts::PI * sum * (2.0 / n as f64);
        assert!((integral - 1.0).abs() < 1e-3, "g={g}: {integral}");
    }
}

#[test]
fn phase_samples_are_unit_and_follow_the_anisotropy() {
    let wi = Vec3A::new(0.2, -0.3, 0.9).normalize();
    for g in [-0.5f32, 0.0, 0.7] {
        let p = PhaseMix::single(g);
        let mut rng = Rng::new(11);
        let n = 20_000;
        let mut mean_cos = 0.0f64;
        for _ in 0..n {
            let d = p.sample(wi, rng.next_f32(), [rng.next_f32(), rng.next_f32()]);
            assert!(approx(d.length(), 1.0, 1e-4));
            mean_cos += wi.dot(d) as f64;
        }
        // E[cos θ] under Henyey-Greenstein is exactly g.
        let mean_cos = mean_cos / n as f64;
        assert!(
            (mean_cos - g as f64).abs() < 0.02,
            "g={g}: mean cos {mean_cos}"
        );
    }
}

#[test]
fn phase_sample_agrees_with_its_pdf_histogram() {
    let g = 0.6f32;
    let p = PhaseMix::single(g);
    let wi = Vec3A::Z;
    let mut rng = Rng::new(2);
    let bins = 10;
    let n = 100_000;
    let mut hist = vec![0u32; bins];
    for _ in 0..n {
        let d = p.sample(wi, rng.next_f32(), [rng.next_f32(), rng.next_f32()]);
        let mu = d.z.clamp(-1.0, 1.0);
        let b = (((mu + 1.0) * 0.5) * bins as f32).min(bins as f32 - 1.0) as usize;
        hist[b] += 1;
    }
    for (b, &count) in hist.iter().enumerate() {
        // Expected mass from a fine quadrature of the pdf over the bin.
        let lo = -1.0 + 2.0 * b as f32 / bins as f32;
        let mut mass = 0.0f64;
        let steps = 200;
        for k in 0..steps {
            let mu = lo + (k as f32 + 0.5) * (2.0 / bins as f32) / steps as f32;
            mass += p.pdf(mu) as f64;
        }
        let expected = mass * 2.0 * std::f64::consts::PI * (2.0 / bins as f64) / steps as f64;
        let observed = count as f64 / n as f64;
        let tol = (0.05 * expected).max(4.0 * (expected / n as f64).sqrt());
        assert!(
            (observed - expected).abs() < tol,
            "bin {b}: {observed} vs {expected}"
        );
    }
}

// ---------------------------------------------------------------------------
// Volumes aggregate
// ---------------------------------------------------------------------------

#[test]
fn empty_volumes_are_transparent() {
    let v = Volumes::new(Vec::new());
    assert!(v.is_empty());
    assert!(v.regions().is_empty());
    let mut rng = Rng::new(1);
    let ray = Ray::new(Vec3A::ZERO, Vec3A::Z);
    assert_eq!(v.transmittance(&ray, 0.0, 10.0, &mut rng), Vec3A::ONE);
    match v.sample_interaction(&ray, 0.0, 10.0, &mut rng) {
        VolumeEvent::Passthrough {
            transmittance,
            emitted,
        } => {
            assert_eq!(transmittance, Vec3A::ONE);
            assert_eq!(emitted, Vec3A::ZERO);
        }
        VolumeEvent::Scatter { .. } => panic!("nothing to scatter in"),
    }
    let d = Volumes::default();
    assert!(d.is_empty());
}

#[test]
fn homogeneous_transmittance_is_exact_beer_lambert() {
    let v = Volumes::new(vec![unit_box(0.3, 0.2)]);
    assert!(!v.is_empty());
    assert_eq!(v.regions().len(), 1);
    let mut rng = Rng::new(1);
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    // The full crossing is 2 units at σₜ = 0.5.
    let tr = v.transmittance(&ray, 1e-4, 100.0, &mut rng);
    assert!(tr.abs_diff_eq(Vec3A::splat((-1.0f32).exp()), 1e-5), "{tr}");
    // Cut short inside the box: only the crossed half counts.
    let tr = v.transmittance(&ray, 1e-4, 5.0, &mut rng);
    assert!(tr.abs_diff_eq(Vec3A::splat((-0.5f32).exp()), 1e-5), "{tr}");
    // Stopping before the box: nothing.
    assert_eq!(v.transmittance(&ray, 1e-4, 3.0, &mut rng), Vec3A::ONE);
    // Missing the box entirely.
    let miss = Ray::new(Vec3A::new(0.0, 3.0, -5.0), Vec3A::Z);
    assert_eq!(v.transmittance(&miss, 1e-4, 100.0, &mut rng), Vec3A::ONE);
}

#[test]
fn transmittance_is_per_channel() {
    let r = VolumeRegion::new(
        Mat4::IDENTITY,
        Vec3A::ONE,
        Vec3A::ZERO,
        Vec3A::new(0.1, 0.5, 1.0),
        0.0,
        Vec3A::ZERO,
        1.0,
        DensityField::Homogeneous,
    );
    let v = Volumes::new(vec![r]);
    let mut rng = Rng::new(1);
    let tr = v.transmittance(
        &Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z),
        1e-4,
        100.0,
        &mut rng,
    );
    assert!(approx(tr.x, (-0.2f32).exp(), 1e-5));
    assert!(approx(tr.y, (-1.0f32).exp(), 1e-5));
    assert!(approx(tr.z, (-2.0f32).exp(), 1e-5));
}

#[test]
fn overlapping_homogeneous_regions_multiply_their_transmittance() {
    let a = unit_box(0.25, 0.0);
    let b = region(
        Mat4::from_translation(Vec3::new(0.0, 0.0, 1.0)),
        1.0,
        0.0,
        0.5,
        DensityField::Homogeneous,
    );
    let v = Volumes::new(vec![a, b]);
    let mut rng = Rng::new(1);
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    // a spans z ∈ [-1, 1] (σ 0.25 → e^-0.5); b spans [0, 2] (σ 0.5 → e^-1).
    let tr = v.transmittance(&ray, 1e-4, 100.0, &mut rng);
    assert!(tr.abs_diff_eq(Vec3A::splat((-1.5f32).exp()), 1e-5), "{tr}");
}

#[test]
fn a_pure_absorber_never_scatters_and_dims_in_expectation() {
    let sigma_a = 0.4;
    let v = Volumes::new(vec![unit_box(0.0, sigma_a)]);
    let mut rng = Rng::new(21);
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    let n = 40_000;
    let mut sum = 0.0f64;
    for _ in 0..n {
        match v.sample_interaction(&ray, 1e-4, 100.0, &mut rng) {
            VolumeEvent::Scatter { .. } => panic!("an absorber has no scattering events"),
            VolumeEvent::Passthrough {
                transmittance,
                emitted,
            } => {
                assert_eq!(emitted, Vec3A::ZERO);
                sum += transmittance.x as f64;
            }
        }
    }
    let mean = sum / n as f64;
    let expected = (-2.0 * sigma_a as f64).exp();
    assert!((mean - expected).abs() < 0.01, "mean {mean} vs {expected}");
}

#[test]
fn a_pure_scatterer_collides_with_the_right_probability() {
    let sigma_s = 0.6;
    let v = Volumes::new(vec![unit_box(sigma_s, 0.0)]);
    let mut rng = Rng::new(22);
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    let n = 40_000;
    let mut scatters = 0;
    for _ in 0..n {
        match v.sample_interaction(&ray, 1e-4, 100.0, &mut rng) {
            VolumeEvent::Scatter {
                t,
                p,
                weight,
                phase,
                emitted,
            } => {
                scatters += 1;
                // The walk starts exactly at the entry distance; an
                // exponential step can round to zero.
                assert!((4.0..6.0).contains(&t), "scatter outside the box at t={t}");
                assert!(p.abs_diff_eq(ray.at(t), 1e-4));
                assert!(weight.is_finite() && weight.min_element() > 0.0);
                assert!(
                    approx(weight.x, 1.0, 1e-4),
                    "a pure scatterer's collision carries unit weight: {weight}"
                );
                assert_eq!(emitted, Vec3A::ZERO);
                assert!(approx(
                    phase.pdf(0.3),
                    1.0 / (4.0 * std::f32::consts::PI),
                    1e-6
                ));
            }
            VolumeEvent::Passthrough { transmittance, .. } => {
                assert!(
                    approx(transmittance.x, 1.0, 1e-5),
                    "no absorption, no null collisions: {transmittance}"
                );
            }
        }
    }
    let frac = scatters as f64 / n as f64;
    let expected = 1.0 - (-2.0 * sigma_s as f64).exp();
    assert!(
        (frac - expected).abs() < 0.01,
        "scatter fraction {frac} vs {expected}"
    );
}

#[test]
fn a_segment_ending_before_the_box_passes_through_untouched() {
    let v = Volumes::new(vec![unit_box(5.0, 5.0)]);
    let mut rng = Rng::new(3);
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    for _ in 0..100 {
        match v.sample_interaction(&ray, 1e-4, 3.5, &mut rng) {
            VolumeEvent::Passthrough {
                transmittance,
                emitted,
            } => {
                assert_eq!(transmittance, Vec3A::ONE);
                assert_eq!(emitted, Vec3A::ZERO);
            }
            VolumeEvent::Scatter { .. } => panic!("scattered before reaching the box"),
        }
    }
}

#[test]
fn an_emissive_absorber_adds_emission_along_the_walk() {
    let r = VolumeRegion::new(
        Mat4::IDENTITY,
        Vec3A::ONE,
        Vec3A::ZERO,
        Vec3A::splat(0.5),
        0.0,
        Vec3A::new(1.0, 2.0, 3.0),
        1.0,
        DensityField::Homogeneous,
    );
    let v = Volumes::new(vec![r]);
    let mut rng = Rng::new(4);
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    let n = 20_000;
    let mut emitted_sum = Vec3A::ZERO;
    let mut any = false;
    for _ in 0..n {
        if let VolumeEvent::Passthrough { emitted, .. } =
            v.sample_interaction(&ray, 1e-4, 100.0, &mut rng)
        {
            any |= emitted.x > 0.0;
            emitted_sum += emitted;
        }
    }
    assert!(any, "an emissive region must contribute emission");
    let mean = emitted_sum / n as f32;
    // Emission is colour-proportional to the authored value.
    assert!(approx(mean.y / mean.x, 2.0, 0.05), "{mean}");
    assert!(approx(mean.z / mean.x, 3.0, 0.05), "{mean}");
    // Analytic: ∫₀² σₐ Lₑ e^{-σₐ s} ds = Lₑ (1 − e^{-1}).
    let expected = 1.0 - (-1.0f32).exp();
    assert!(
        approx(mean.x, expected, 0.03),
        "mean emission {} vs {expected}",
        mean.x
    );
}

#[test]
fn heterogeneous_transmittance_is_unbiased_against_the_analytic_answer() {
    // A grid that is dense on the far half only, so ratio tracking has
    // real null collisions to handle.
    let r = region(
        Mat4::IDENTITY,
        1.0,
        0.0,
        1.0,
        DensityField::Grid {
            nx: 1,
            ny: 1,
            nz: 2,
            data: vec![0.0, 1.0],
        },
    );
    let v = Volumes::new(vec![r]);
    let mut rng = Rng::new(9);
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    let n = 40_000;
    let mut sum = 0.0f64;
    for _ in 0..n {
        let tr = v.transmittance(&ray, 1e-4, 100.0, &mut rng);
        assert!(tr.min_element() >= 0.0 && tr.max_element() <= 1.0 + 1e-5);
        sum += tr.x as f64;
    }
    let mean = sum / n as f64;
    // Density ramps linearly from 0 at z=-0.5 to 1 at z=+0.5 (voxel
    // centres), clamped outside: optical depth = 0·0.5 + 0.5 + 1·0.5 = 1.
    let expected = (-1.0f64).exp();
    assert!((mean - expected).abs() < 0.015, "mean {mean} vs {expected}");
}

#[test]
fn scatter_events_in_overlapping_regions_mix_their_phase_lobes() {
    let a = VolumeRegion::new(
        Mat4::IDENTITY,
        Vec3A::ONE,
        Vec3A::splat(1.0),
        Vec3A::ZERO,
        0.8,
        Vec3A::ZERO,
        1.0,
        DensityField::Homogeneous,
    );
    let b = VolumeRegion::new(
        Mat4::IDENTITY,
        Vec3A::ONE,
        Vec3A::splat(1.0),
        Vec3A::ZERO,
        -0.8,
        Vec3A::ZERO,
        1.0,
        DensityField::Homogeneous,
    );
    let v = Volumes::new(vec![a, b]);
    let mut rng = Rng::new(6);
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    let mut seen = false;
    for _ in 0..200 {
        if let VolumeEvent::Scatter { phase, .. } =
            v.sample_interaction(&ray, 1e-4, 100.0, &mut rng)
        {
            seen = true;
            // Equal σₛ: the mixture is symmetric, so forward and backward
            // agree and both exceed the sideways value.
            assert!(approx(phase.pdf(1.0), phase.pdf(-1.0), 1e-5));
            assert!(phase.pdf(1.0) > phase.pdf(0.0));
        }
    }
    assert!(seen);
}
