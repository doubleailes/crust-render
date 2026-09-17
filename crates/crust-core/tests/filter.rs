//! Pixel reconstruction filters through the public `PixelFilter` /
//! `FilterSampler` API: naming, radii, kernel shapes, and that filter
//! importance sampling actually draws from the kernel it claims to.

use crust_core::{FilterSampler, PixelFilter};

fn all_kinds() -> [PixelFilter; 5] {
    [
        PixelFilter::BoxFilter { radius: 0.5 },
        PixelFilter::Triangle { radius: 1.0 },
        PixelFilter::Gaussian { radius: 1.5 },
        PixelFilter::Blackman { radius: 1.5 },
        PixelFilter::Mitchell { radius: 2.0 },
    ]
}

#[test]
fn default_is_the_unit_triangle() {
    assert_eq!(PixelFilter::default(), PixelFilter::Triangle { radius: 1.0 });
}

#[test]
fn names_round_trip_through_from_name() {
    for f in all_kinds() {
        let back = PixelFilter::from_name(f.name()).expect("every name is known");
        assert_eq!(back, f, "{}", f.name());
    }
    assert!(PixelFilter::from_name("lanczos").is_none());
    assert!(PixelFilter::from_name("").is_none());
    assert!(PixelFilter::from_name("Box").is_none(), "names are case-sensitive tokens");
}

#[test]
fn from_name_uses_each_kinds_conventional_radius() {
    assert_eq!(PixelFilter::from_name("box").unwrap().radius(), 0.5);
    assert_eq!(PixelFilter::from_name("triangle").unwrap().radius(), 1.0);
    assert_eq!(PixelFilter::from_name("gaussian").unwrap().radius(), 1.5);
    assert_eq!(PixelFilter::from_name("blackman").unwrap().radius(), 1.5);
    assert_eq!(PixelFilter::from_name("mitchell").unwrap().radius(), 2.0);
}

#[test]
fn with_radius_keeps_the_kind_and_clamps_nonsense() {
    for f in all_kinds() {
        let r = f.with_radius(2.75);
        assert_eq!(r.name(), f.name());
        assert_eq!(r.radius(), 2.75);
        assert!(f.with_radius(0.0).radius() > 0.0, "zero radius is clamped");
        assert!(f.with_radius(-3.0).radius() > 0.0, "negative radius is clamped");
    }
}

#[test]
fn kernels_are_zero_outside_the_radius() {
    for f in all_kinds() {
        let r = f.radius();
        assert_eq!(f.eval(r + 1e-3), 0.0, "{}", f.name());
        assert_eq!(f.eval(-r - 1e-3), 0.0, "{}", f.name());
        assert_eq!(f.eval(10.0 * r), 0.0, "{}", f.name());
    }
}

#[test]
fn kernels_are_even_functions() {
    for f in all_kinds() {
        let r = f.radius();
        for i in 1..20 {
            let x = r * i as f32 / 20.0;
            assert!((f.eval(x) - f.eval(-x)).abs() < 1e-6, "{} at {x}", f.name());
        }
    }
}

#[test]
fn kernels_peak_at_the_centre() {
    for f in all_kinds() {
        let c = f.eval(0.0);
        assert!(c > 0.0, "{}", f.name());
        for i in 1..=20 {
            let x = f.radius() * i as f32 / 20.0;
            assert!(f.eval(x) <= c + 1e-6, "{} at {x}", f.name());
        }
    }
}

#[test]
fn box_is_flat_and_triangle_is_a_tent() {
    let b = PixelFilter::BoxFilter { radius: 0.5 };
    assert_eq!(b.eval(0.0), 1.0);
    assert_eq!(b.eval(0.49), 1.0);
    let t = PixelFilter::Triangle { radius: 2.0 };
    assert_eq!(t.eval(0.0), 2.0);
    assert_eq!(t.eval(1.0), 1.0);
    assert_eq!(t.eval(-1.5), 0.5);
    assert_eq!(t.eval(2.0), 0.0);
}

#[test]
fn gaussian_reaches_exactly_zero_at_its_radius() {
    let g = PixelFilter::Gaussian { radius: 1.5 };
    assert_eq!(g.eval(1.5), 0.0);
    assert!(g.eval(1.4) > 0.0);
    // σ = r/3, so at x = σ the kernel is ~e^{-1/2} of the peak (before the
    // tail subtraction, which is tiny).
    let ratio = g.eval(0.5) / g.eval(0.0);
    assert!((ratio - (-0.5f32).exp()).abs() < 0.01, "{ratio}");
}

#[test]
fn blackman_window_is_small_at_its_edges() {
    let b = PixelFilter::Blackman { radius: 1.0 };
    assert!(b.eval(0.999).abs() < 1e-3);
    assert!((b.eval(0.0) - 1.0).abs() < 1e-3, "the 4-term window peaks at ~1");
}

#[test]
fn mitchell_is_the_only_kernel_with_negative_lobes() {
    for f in all_kinds() {
        let r = f.radius();
        let has_negative = (0..200).any(|i| f.eval(r * i as f32 / 200.0) < -1e-6);
        assert_eq!(has_negative, f.name() == "mitchell", "{}", f.name());
    }
    // Mitchell B = C = 1/3: positive core (B/6 at |x'| = 1), a negative
    // lobe on (1, 2) and exactly zero at the radius.
    let m = PixelFilter::Mitchell { radius: 2.0 };
    assert!((m.eval(1.0) - 1.0 / 18.0).abs() < 1e-5, "{}", m.eval(1.0));
    assert!(m.eval(1.5) < 0.0);
    assert!(m.eval(0.5) > 0.0);
    assert!(m.eval(2.0).abs() < 1e-6);
}

#[test]
fn box_at_half_radius_is_the_identity_jitter() {
    let s = FilterSampler::new(PixelFilter::BoxFilter { radius: 0.5 });
    for u in [0.0f32, 0.1, 0.25, 0.5, 0.75, 0.999] {
        let (x, w) = s.sample(u);
        assert_eq!(x, u, "bit-identical offset at u={u}");
        assert_eq!(w, 1.0);
    }
}

#[test]
fn box_at_other_radii_spreads_uniformly() {
    let s = FilterSampler::new(PixelFilter::BoxFilter { radius: 1.5 });
    let (lo, _) = s.sample(0.0);
    let (hi, _) = s.sample(1.0);
    assert!((lo - (0.5 - 1.5)).abs() < 1e-6);
    assert!((hi - (0.5 + 1.5)).abs() < 1e-6);
    let (mid, w) = s.sample(0.5);
    assert!((mid - 0.5).abs() < 1e-6);
    assert_eq!(w, 1.0);
}

#[test]
fn samples_stay_within_the_filter_footprint() {
    for f in all_kinds() {
        let s = FilterSampler::new(f);
        let r = f.radius();
        for i in 0..=1000 {
            let u = (i as f32 / 1000.0).min(0.999_999);
            let (x, w) = s.sample(u);
            assert!(
                x >= 0.5 - r - 1e-4 && x <= 0.5 + r + 1e-4,
                "{} u={u}: offset {x} outside radius {r}",
                f.name()
            );
            assert!(w.is_finite(), "{} u={u}: weight {w}", f.name());
        }
    }
}

#[test]
fn sampling_is_monotone_in_u() {
    for f in all_kinds() {
        let s = FilterSampler::new(f);
        let mut prev = f32::NEG_INFINITY;
        for i in 0..=2000 {
            let u = (i as f32 / 2000.0).min(0.999_999);
            let (x, _) = s.sample(u);
            assert!(x >= prev - 1e-6, "{} not monotone at u={u}", f.name());
            prev = x;
        }
    }
}

#[test]
fn triangle_median_is_the_pixel_centre() {
    let s = FilterSampler::new(PixelFilter::Triangle { radius: 1.0 });
    let (x, w) = s.sample(0.5);
    assert!((x - 0.5).abs() < 1e-6);
    assert_eq!(w, 1.0);
    // The tent's inverse CDF: u = 1/8 lands at x = -1/2 offset.
    let (x, _) = s.sample(0.125);
    assert!((x - 0.0).abs() < 1e-5, "{x}");
}

#[test]
fn nonnegative_filters_have_nonnegative_weights() {
    for f in all_kinds() {
        if f.name() == "mitchell" {
            continue;
        }
        let s = FilterSampler::new(f);
        for i in 0..1000 {
            let u = (i as f32 + 0.5) / 1000.0;
            let (_, w) = s.sample(u);
            assert!(w >= 0.0 && w.is_finite(), "{} u={u}: w={w}", f.name());
            // Box and triangle are sampled analytically: the weight is
            // exactly one. Tabulated kinds vary within a bin (see the mean
            // test), but never wildly.
            if matches!(f, PixelFilter::BoxFilter { .. } | PixelFilter::Triangle { .. }) {
                assert_eq!(w, 1.0, "{} u={u}", f.name());
            } else {
                assert!(w < 3.0, "{} u={u}: w={w}", f.name());
            }
        }
    }
}

#[test]
fn weights_average_to_one_for_every_kind() {
    // E[w] = 1 is what makes filter importance sampling unbiased; for
    // Mitchell that means the negative weights must cancel exactly against
    // the positive excess.
    for f in all_kinds() {
        let s = FilterSampler::new(f);
        let n = 100_000;
        let mut sum = 0.0f64;
        for i in 0..n {
            let u = (i as f32 + 0.5) / n as f32;
            sum += s.sample(u).1 as f64;
        }
        let mean = sum / n as f64;
        assert!((mean - 1.0).abs() < 2e-3, "{}: mean weight {mean}", f.name());
    }
}

#[test]
fn mitchell_produces_negative_weights_on_its_lobes() {
    let f = PixelFilter::Mitchell { radius: 2.0 };
    let s = FilterSampler::new(f);
    let mut negatives = 0;
    for i in 0..10_000 {
        let u = (i as f32 + 0.5) / 10_000.0;
        let (x, w) = s.sample(u);
        if w < 0.0 {
            negatives += 1;
            assert!(f.eval(x - 0.5) < 0.0, "negative weight off the negative lobe at {x}");
        }
    }
    assert!(negatives > 0, "the negative lobes were never sampled");
}

#[test]
fn sampled_positions_follow_the_kernel_shape() {
    // Histogram the sampled offsets and compare against |f| integrated
    // over each bin — the sampled density must be the (absolute) kernel.
    for f in all_kinds() {
        let s = FilterSampler::new(f);
        let r = f.radius();
        let bins = 16;
        let n = 200_000;
        let mut hist = vec![0u32; bins];
        for i in 0..n {
            let u = (i as f32 + 0.5) / n as f32;
            let (x, _) = s.sample(u);
            let b = (((x - 0.5 + r) / (2.0 * r)) * bins as f32).clamp(0.0, bins as f32 - 1.0) as usize;
            hist[b] += 1;
        }
        // Reference masses from a fine quadrature of |f|.
        let mut mass = vec![0.0f64; bins];
        let steps = 4000;
        for k in 0..steps {
            let x = -r + (k as f32 + 0.5) * (2.0 * r / steps as f32);
            let b = (((x + r) / (2.0 * r)) * bins as f32).clamp(0.0, bins as f32 - 1.0) as usize;
            mass[b] += f.eval(x).abs() as f64;
        }
        let total: f64 = mass.iter().sum();
        for b in 0..bins {
            let expected = mass[b] / total;
            let observed = hist[b] as f64 / n as f64;
            let tol = (0.06 * expected).max(0.004);
            assert!(
                (observed - expected).abs() < tol,
                "{} bin {b}: observed {observed:.4} expected {expected:.4}",
                f.name()
            );
        }
    }
}

#[test]
fn sampler_can_be_rebuilt_for_any_radius() {
    for r in [0.01f32, 0.3, 1.0, 4.0] {
        for f in all_kinds() {
            let s = FilterSampler::new(f.with_radius(r));
            let (x, w) = s.sample(0.37);
            assert!(x.is_finite() && w.is_finite(), "{} r={r}", f.name());
        }
    }
}
