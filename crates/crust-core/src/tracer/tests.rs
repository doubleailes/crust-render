use super::SamplingStrategy;

/// The clamp caps the brightest channel and scales the others with it,
/// so a saturated firefly keeps its hue; below the limit it is exact.
#[test]
fn clamp_indirect_keeps_hue_and_caps_the_peak() {
    use super::path::clamp_indirect;
    use glam::Vec3A;
    let c = clamp_indirect(Vec3A::new(40.0, 20.0, 4.0), 10.0);
    assert_eq!(c, Vec3A::new(10.0, 5.0, 1.0));
    let under = Vec3A::new(3.0, 9.0, 0.5);
    assert_eq!(clamp_indirect(under, 10.0), under);
    assert_eq!(clamp_indirect(Vec3A::ZERO, 10.0), Vec3A::ZERO);
}

/// A broken sample is left as it came, never turned into a NaN.
#[test]
fn clamp_indirect_passes_non_finite_samples_through() {
    use super::path::clamp_indirect;
    use glam::Vec3A;
    let inf = Vec3A::new(f32::INFINITY, 2.0, 1.0);
    assert_eq!(clamp_indirect(inf, 10.0), inf);
    let nan = clamp_indirect(Vec3A::new(f32::NAN, 20.0, 1.0), 10.0);
    assert!(nan.x.is_nan());
}

/// The invariant every strategy must keep: for a light both strategies
/// can reach, the NEE weight and the bounce-emission weight are a
/// partition of unity — anything else double-counts or loses emission.
#[test]
fn strategy_weights_partition_unity() {
    let strategies = [
        SamplingStrategy::PowerMis,
        SamplingStrategy::BalanceMis,
        SamplingStrategy::LightOnly,
        SamplingStrategy::BsdfOnly,
    ];
    // (light_pdf, bounce_pdf) pairs spanning near-delta glossy spikes,
    // balanced cases, and tiny-light spikes.
    let pdf_pairs = [
        (0.5, 0.5),
        (1e-4, 1e4),
        (1e4, 1e-4),
        (3.0, 0.2),
        (0.05, 40.0),
    ];
    for s in strategies {
        for (light_pdf, bounce_pdf) in pdf_pairs {
            let sum =
                s.light_weight(light_pdf, bounce_pdf) + s.bounce_weight(bounce_pdf, light_pdf);
            assert!(
                (sum - 1.0).abs() < 1e-3,
                "{s:?}: weights sum to {sum} at pdfs ({light_pdf}, {bounce_pdf})"
            );
        }
    }
}

/// A contribution with no competing technique is taken whole under
/// every strategy. `bounce_weight` against a zero light pdf is not the
/// same thing: `LightOnly` gives it 0, which would drop light NEE cannot
/// reach, and the power heuristic's `1e-6` keeps it just short of 1.
#[test]
fn unopposed_contributions_are_taken_whole() {
    for s in [
        SamplingStrategy::PowerMis,
        SamplingStrategy::BalanceMis,
        SamplingStrategy::LightOnly,
        SamplingStrategy::BsdfOnly,
    ] {
        assert_eq!(s.unopposed_weight(), 1.0, "{s:?}");
    }
    assert_eq!(SamplingStrategy::LightOnly.bounce_weight(1.0, 0.0), 0.0);
}

#[test]
fn single_strategy_modes_disable_the_other_side() {
    assert!(!SamplingStrategy::BsdfOnly.samples_lights());
    assert!(SamplingStrategy::LightOnly.samples_lights());
    assert_eq!(SamplingStrategy::LightOnly.light_weight(1.0, 100.0), 1.0);
    assert_eq!(SamplingStrategy::LightOnly.bounce_weight(100.0, 1.0), 0.0);
    assert_eq!(SamplingStrategy::BsdfOnly.light_weight(100.0, 1.0), 0.0);
    assert_eq!(SamplingStrategy::BsdfOnly.bounce_weight(1.0, 100.0), 1.0);
}

/// The power heuristic commits harder to the denser strategy than the
/// balance heuristic — the property that makes it the better default on
/// glossy surfaces.
#[test]
fn power_sharpens_balance() {
    let (a, b) = (10.0, 1.0);
    let balance = SamplingStrategy::BalanceMis.light_weight(a, b);
    let power = SamplingStrategy::PowerMis.light_weight(a, b);
    assert!(power > balance, "power {power} <= balance {balance}");
}
