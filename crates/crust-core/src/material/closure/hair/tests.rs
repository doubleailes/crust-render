//! The fibre lobe against pbrt-v3's hair tests (white furnace by quadrature
//! and by sampling, sampling weights, sampling consistency) and the
//! properties the spec names: absorption's colour and the cuticle's tilt.

use super::*;
use openqmc::pcg::Rng;

/// `chiang_hair_roughness` (genglsl) for artist β: `(v, s)` before the
/// √(π/8), what the leaf's roughness inputs hold.
fn roughness(beta_m: f32, beta_n: f32) -> (f32, f32) {
    let lr = beta_m.clamp(0.001, 1.0);
    let ar = beta_n.clamp(0.001, 1.0);
    let v = 0.726 * lr + 0.812 * lr * lr + 3.7 * lr.powi(20);
    let s = 0.265 * ar + 1.194 * ar * ar + 5.372 * ar.powi(22);
    (v * v, s)
}

fn params(beta_m: f32, beta_n: f32, absorption: Vec3A, cuticle_angle: f32) -> HairParams {
    let (v, s) = roughness(beta_m, beta_n);
    HairParams {
        tint: [Vec3A::ONE; 3],
        ior: 1.55,
        roughness: [(v, s), (v * 0.25, s), (v * 4.0, s)],
        cuticle_angle,
        absorption,
    }
}

fn luma(c: Vec3A) -> f32 {
    utils::luminance(c)
}

/// ωo at longitudinal angle `theta` (from the normal plane) and azimuth
/// `phi` (from +y toward +z).
fn dir(theta: f32, phi: f32) -> Vec3A {
    let (s, c) = theta.sin_cos();
    Vec3A::new(s, c * phi.cos(), c * phi.sin())
}

const VIEWS: [(f32, f32); 3] = [(0.0, 1.5707964), (0.5, 1.2), (-0.9, 2.4)];

/// `∫ f·|cos| dω` by the midpoint rule over the sphere, in `(sin θ, φ)`,
/// whose measure is exactly the solid angle's. `f·|cos|` is smooth and the
/// grid resolves the narrowest lobe the tests use several times over, so
/// this is far more accurate than the tolerance it is held to.
fn quadrature(h: &Hair, g: impl Fn(Vec3A) -> f32) -> (Vec3A, f32) {
    const NU: usize = 256;
    const NPHI: usize = 512;
    let (du, dphi) = (2.0 / NU as f32, 2.0 * PI / NPHI as f32);
    let mut value = Vec3A::ZERO;
    let mut pdf = 0.0;
    for i in 0..NU {
        let x = -1.0 + (i as f32 + 0.5) * du;
        let c = safe_sqrt(1.0 - x * x);
        for j in 0..NPHI {
            let phi = -PI + (j as f32 + 0.5) * dphi;
            let wi = Vec3A::new(x, c * phi.cos(), c * phi.sin());
            let (f, p) = h.eval(wi);
            value += f * wi.z.abs() * g(wi);
            pdf += p;
        }
    }
    (value * du * dphi, pdf * du * dphi)
}

/// White furnace, by quadrature: a fibre that absorbs nothing reflects or
/// transmits everything it receives, at every roughness and view — and its
/// sampling density is a probability density.
#[test]
fn a_clear_fibre_conserves_energy() {
    for beta_m in [0.1, 0.4, 0.7] {
        for beta_n in [0.1, 0.4, 0.7] {
            for cuticle in [0.5, 0.511_111] {
                for (theta, phi) in VIEWS {
                    let h = Hair::new(
                        &params(beta_m, beta_n, Vec3A::ZERO, cuticle),
                        dir(theta, phi),
                        luma,
                    );
                    let (rho, pdf) = quadrature(&h, |_| 1.0);
                    let at =
                        format!("β ({beta_m}, {beta_n}), cuticle {cuticle}, ωo ({theta}, {phi})");
                    assert!((0.95..=1.05).contains(&rho.x), "{at}: albedo {rho:?}");
                    assert!((0.98..=1.02).contains(&pdf), "{at}: ∫ pdf {pdf}");
                    // The analytic albedo is the attenuations' sum, exactly 1.
                    assert!((h.albedo().x - 1.0).abs() < 1e-5, "{at}: {:?}", h.albedo());
                }
            }
        }
    }
}

/// With absorption, the integral is the analytic albedo `Σ_p A_p`.
#[test]
fn the_albedo_is_the_integral() {
    let sigma = Vec3A::new(0.2, 0.6, 1.2);
    for (beta_m, beta_n) in [(0.2, 0.3), (0.5, 0.6)] {
        for (theta, phi) in VIEWS {
            let h = Hair::new(&params(beta_m, beta_n, sigma, 0.52), dir(theta, phi), luma);
            let (rho, _) = quadrature(&h, |_| 1.0);
            let want = h.albedo();
            assert!(
                (rho - want).abs().max_element() < 0.02 * want.max_element(),
                "β ({beta_m}, {beta_n}) ωo ({theta}, {phi}): {rho:?} vs {want:?}"
            );
        }
    }
}

/// pbrt-v3's sampling-weights test: for a clear fibre the attenuations sum to
/// one, so the lobe pdf is exactly the value's share and every sample's
/// weight `f·|cos| / pdf` is 1.
#[test]
fn every_sample_of_a_clear_fibre_weighs_one() {
    let mut rng = Rng::new(7);
    for beta_m in [0.1, 0.4, 0.7] {
        for beta_n in [0.1, 0.4, 0.7] {
            for (theta, phi) in VIEWS {
                let h = Hair::new(
                    &params(beta_m, beta_n, Vec3A::ZERO, 0.52),
                    dir(theta, phi),
                    luma,
                );
                for _ in 0..2000 {
                    let u = [rng.next_f32(), rng.next_f32()];
                    let Some(wi) = h.sample(u, rng.next_f32()) else {
                        panic!("a clear fibre always samples");
                    };
                    assert!((wi.length() - 1.0).abs() < 1e-4, "{wi:?}");
                    let (f, pdf) = h.eval(wi);
                    if pdf == 0.0 {
                        continue;
                    }
                    let w = f.x * wi.z.abs() / pdf;
                    assert!((w - 1.0).abs() < 2e-3, "β ({beta_m}, {beta_n}): weight {w}");
                }
            }
        }
    }
}

/// Sampling consistency: the importance-sampled estimate of `∫ f·|cos|·L`
/// for a non-uniform `L` matches the quadrature, so the sampled directions
/// follow the density `eval` reports.
#[test]
fn sampling_agrees_with_evaluation() {
    let sigma = Vec3A::new(0.3, 0.5, 0.9);
    let light = |w: Vec3A| 1.0 + 3.0 * (w.y * w.y) + 2.0 * w.x.max(0.0);
    let mut rng = Rng::new(11);
    for (beta_m, beta_n) in [(0.15, 0.2), (0.4, 0.5), (0.8, 0.9)] {
        for (theta, phi) in VIEWS {
            let h = Hair::new(&params(beta_m, beta_n, sigma, 0.53), dir(theta, phi), luma);
            let (want, _) = quadrature(&h, light);
            let n = 40_000;
            let mut sum = Vec3A::ZERO;
            for _ in 0..n {
                let u = [rng.next_f32(), rng.next_f32()];
                if let Some(wi) = h.sample(u, rng.next_f32()) {
                    let (f, pdf) = h.eval(wi);
                    if pdf > 0.0 {
                        sum += f * wi.z.abs() * light(wi) / pdf;
                    }
                }
            }
            let got = sum / n as f32;
            assert!(
                (got - want).abs().max_element() < 0.03 * want.max_element(),
                "β ({beta_m}, {beta_n}) ωo ({theta}, {phi}): sampled {got:?}, quadrature {want:?}"
            );
        }
    }
}

/// The histogram of sampled directions matches the reported pdf, bin by bin,
/// over the whole sphere.
#[test]
fn the_sampled_directions_follow_the_pdf() {
    const BU: usize = 8;
    const BPHI: usize = 16;
    let h = Hair::new(
        &params(0.3, 0.4, Vec3A::new(0.2, 0.3, 0.5), 0.52),
        dir(0.4, 1.3),
        luma,
    );
    let bin = |w: Vec3A| {
        let i = (((w.x + 1.0) * 0.5 * BU as f32) as usize).min(BU - 1);
        let phi = w.z.atan2(w.y);
        let j = (((phi + PI) / (2.0 * PI) * BPHI as f32) as usize).min(BPHI - 1);
        i * BPHI + j
    };
    let mut want = vec![0.0f64; BU * BPHI];
    const SUB: usize = 24;
    let (du, dphi) = (2.0 / (BU * SUB) as f32, 2.0 * PI / (BPHI * SUB) as f32);
    for i in 0..BU * SUB {
        let x = -1.0 + (i as f32 + 0.5) * du;
        let c = safe_sqrt(1.0 - x * x);
        for j in 0..BPHI * SUB {
            let phi = -PI + (j as f32 + 0.5) * dphi;
            let w = Vec3A::new(x, c * phi.cos(), c * phi.sin());
            want[bin(w)] += f64::from(h.eval(w).1 * du * dphi);
        }
    }
    let n = 200_000;
    let mut got = vec![0.0f64; BU * BPHI];
    let mut rng = Rng::new(3);
    for _ in 0..n {
        let u = [rng.next_f32(), rng.next_f32()];
        let w = h.sample(u, rng.next_f32()).unwrap();
        got[bin(w)] += 1.0 / f64::from(n);
    }
    for (k, (g, w)) in got.iter().zip(&want).enumerate() {
        // Four standard deviations of a bin's frequency, plus the
        // quadrature's own slack.
        let tol = 4.0 * (w / f64::from(n)).sqrt() + 2e-3;
        assert!((g - w).abs() < tol, "bin {k}: sampled {g:.5}, pdf {w:.5}");
    }
}

/// Absorption inside the fibre colours it: a coefficient rising from red to
/// blue leaves red > green > blue, all below the clear fibre's.
#[test]
fn absorption_colours_the_hair() {
    let h = Hair::new(
        &params(0.3, 0.3, Vec3A::new(0.2, 0.6, 1.2), 0.5),
        dir(0.2, 1.4),
        luma,
    );
    let a = h.albedo();
    assert!(a.x > a.y && a.y > a.z, "{a:?}");
    assert!(a.max_element() < 1.0 && a.min_element() > 0.0, "{a:?}");
}

/// The cuticle tilt moves R's peak along the fibre the way MaterialX's
/// genglsl does (θi rotated by +2α: the peak at θi = −θo − 2α), and TRT's the
/// opposite way, twice as far (genglsl's −4α).
#[test]
fn the_cuticle_tilt_moves_r_and_trt_opposite_ways() {
    let cuticle = 0.6; // α = 0.1π, 18°.
    let alpha = cuticle * PI - PI / 2.0;
    let mut p = params(0.15, 0.3, Vec3A::ZERO, cuticle);
    p.roughness = [(0.002, 0.3); 3];
    let h = Hair::new(&p, dir(0.0, PI / 2.0), luma);
    let peak = |lobe: usize| {
        (0..4001)
            .map(|k| -PI / 2.0 + PI * k as f32 / 4000.0)
            .max_by(|&a, &b| {
                let m = |t: f32| {
                    let (s, c) = t.sin_cos();
                    mp(c, h.cos_o[lobe], s, h.sin_o[lobe], h.v[lobe])
                };
                m(a).total_cmp(&m(b))
            })
            .unwrap()
    };
    let (r, trt) = (peak(0), peak(2));
    assert!(
        (r - -2.0 * alpha).abs() < 0.01,
        "R peak {r}, want {}",
        -2.0 * alpha
    );
    assert!(
        (trt - 4.0 * alpha).abs() < 0.01,
        "TRT peak {trt}, want {}",
        4.0 * alpha
    );
    // Untilted, both peak in the mirror direction θi = −θo = 0.
    let h = Hair::new(
        &params(0.15, 0.3, Vec3A::ZERO, 0.5),
        dir(0.0, PI / 2.0),
        luma,
    );
    let m = |lobe: usize, t: f32| {
        let (s, c) = t.sin_cos();
        mp(c, h.cos_o[lobe], s, h.sin_o[lobe], h.v[lobe])
    };
    assert!(m(0, 0.0) > m(0, 0.1) && m(0, 0.0) > m(0, -0.1));
}

/// Black tints: nothing to sample, nothing to evaluate.
#[test]
fn a_black_fibre_neither_samples_nor_scatters() {
    let mut p = params(0.3, 0.3, Vec3A::ZERO, 0.5);
    p.tint = [Vec3A::ZERO; 3];
    let h = Hair::new(&p, dir(0.3, 1.0), luma);
    assert!(h.sample([0.3, 0.6], 0.2).is_none());
    assert_eq!(h.eval(dir(-0.2, 2.0)).0, Vec3A::ZERO);
    assert_eq!(h.albedo(), Vec3A::ZERO);
}

/// Exactly grazing the tube's normal plane, `f` would be `inf · 0`: both `f`
/// and the pdf are zero there instead, and finite everywhere else.
#[test]
fn grazing_directions_are_zero_not_infinite() {
    let h = Hair::new(&params(0.3, 0.3, Vec3A::ZERO, 0.5), dir(0.3, 1.0), luma);
    assert_eq!(h.eval(Vec3A::new(0.6, 0.8, 0.0)), (Vec3A::ZERO, 0.0));
    let (f, p) = h.eval(Vec3A::new(0.6, 0.79, 0.01).normalize());
    assert!(f.is_finite() && p.is_finite() && p > 0.0);
}

/// Looking straight along the fibre has no azimuth to speak of; the lobe
/// stays finite.
#[test]
fn a_view_along_the_fibre_stays_finite() {
    let h = Hair::new(&params(0.3, 0.3, Vec3A::splat(0.4), 0.5), Vec3A::X, luma);
    assert!(h.albedo().is_finite());
    let wi = h.sample([0.4, 0.7], 0.3).unwrap();
    let (f, p) = h.eval(wi);
    assert!(wi.is_finite() && f.is_finite() && p.is_finite());
}

#[test]
fn demux_gives_two_numbers_in_the_unit_interval() {
    for f in [0.0, 0.25, 0.5, 0.999_999_9, 1.0] {
        let [a, b] = demux(f);
        assert!(
            (0.0..1.0).contains(&a) && (0.0..1.0).contains(&b),
            "{f}: {a}, {b}"
        );
    }
    // Different inputs, different pairs.
    assert_ne!(demux(0.3), demux(0.7));
}
