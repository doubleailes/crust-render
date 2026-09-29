use super::SamplingStrategy;
use crate::pdf::PdfSolidAngle;

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
            let sum = s.light_weight(
                PdfSolidAngle::from_measure(light_pdf),
                PdfSolidAngle::from_measure(bounce_pdf),
            ) + s.bounce_weight(
                PdfSolidAngle::from_measure(bounce_pdf),
                PdfSolidAngle::from_measure(light_pdf),
            );
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
    assert_eq!(
        SamplingStrategy::LightOnly.bounce_weight(
            PdfSolidAngle::from_measure(1.0),
            PdfSolidAngle::from_measure(0.0)
        ),
        0.0
    );
}

#[test]
fn single_strategy_modes_disable_the_other_side() {
    assert!(!SamplingStrategy::BsdfOnly.samples_lights());
    assert!(SamplingStrategy::LightOnly.samples_lights());
    assert_eq!(
        SamplingStrategy::LightOnly.light_weight(
            PdfSolidAngle::from_measure(1.0),
            PdfSolidAngle::from_measure(100.0)
        ),
        1.0
    );
    assert_eq!(
        SamplingStrategy::LightOnly.bounce_weight(
            PdfSolidAngle::from_measure(100.0),
            PdfSolidAngle::from_measure(1.0)
        ),
        0.0
    );
    assert_eq!(
        SamplingStrategy::BsdfOnly.light_weight(
            PdfSolidAngle::from_measure(100.0),
            PdfSolidAngle::from_measure(1.0)
        ),
        0.0
    );
    assert_eq!(
        SamplingStrategy::BsdfOnly.bounce_weight(
            PdfSolidAngle::from_measure(1.0),
            PdfSolidAngle::from_measure(100.0)
        ),
        1.0
    );
}

/// The power heuristic commits harder to the denser strategy than the
/// balance heuristic — the property that makes it the better default on
/// glossy surfaces.
#[test]
fn power_sharpens_balance() {
    let (a, b) = (10.0, 1.0);
    let balance = SamplingStrategy::BalanceMis.light_weight(
        PdfSolidAngle::from_measure(a),
        PdfSolidAngle::from_measure(b),
    );
    let power = SamplingStrategy::PowerMis.light_weight(
        PdfSolidAngle::from_measure(a),
        PdfSolidAngle::from_measure(b),
    );
    assert!(power > balance, "power {power} <= balance {balance}");
}

/// A dome hidden from the camera: a camera ray escaping past it collects
/// black, while a bounce ray still collects the dome, at the same weight as
/// when it was visible.
#[test]
fn a_camera_invisible_dome_is_black_to_the_camera_only() {
    use super::path::escaped_emission;
    use crate::ray::{MASK_ALL, MASK_CAMERA, MASK_INDIRECT};
    use crate::{DomeLight, LightList};
    use glam::{Mat3A, Vec3A};
    let dome = || DomeLight::new(Vec3A::new(0.2, 0.4, 0.8), None, Mat3A::IDENTITY);
    let mut hidden = LightList::new();
    hidden.add_masked(dome(), crate::RayMask(MASK_ALL.0 & !MASK_CAMERA.0));
    let mut shown = LightList::new();
    shown.add(dome());
    let s = SamplingStrategy::PowerMis;
    let dir = Vec3A::Y;
    assert_eq!(
        escaped_emission(&None, &hidden, dir, MASK_CAMERA, s),
        Vec3A::ZERO
    );
    assert_eq!(
        escaped_emission(&None, &hidden, dir, MASK_INDIRECT, s),
        escaped_emission(&None, &shown, dir, MASK_INDIRECT, s)
    );
    assert_eq!(
        escaped_emission(&None, &shown, dir, MASK_CAMERA, s),
        Vec3A::new(0.2, 0.4, 0.8)
    );
    // `domeLightCameraVisibility = false` hides it the same way.
    shown.hide_infinite_from_camera();
    assert_eq!(
        escaped_emission(&None, &shown, dir, MASK_CAMERA, s),
        Vec3A::ZERO
    );
}

/// A backdrop stands in front of the HDRI for camera rays and does not
/// exist for any other: each ray category collects exactly one of them.
#[test]
fn a_backdrop_is_seen_by_camera_rays_alone() {
    use super::path::escaped_emission;
    use crate::ray::{MASK_CAMERA, MASK_INDIRECT};
    use crate::{DomeLight, LightList};
    use glam::{Mat3A, Vec3A};
    let hdri = Vec3A::new(1.0, 0.9, 0.7);
    let backdrop = Vec3A::new(0.1, 0.3, 0.9);
    let mut lights = LightList::new();
    lights.add(DomeLight::new(hdri, None, Mat3A::IDENTITY));
    lights.add_backdrop(DomeLight::new(backdrop, None, Mat3A::IDENTITY));
    let s = SamplingStrategy::PowerMis;
    for dir in [Vec3A::Y, -Vec3A::Y, Vec3A::X] {
        assert_eq!(
            escaped_emission(&None, &lights, dir, MASK_CAMERA, s),
            backdrop
        );
        assert_eq!(
            escaped_emission(&None, &lights, dir, MASK_INDIRECT, s),
            hdri
        );
    }
    // A backdrop alone lights nothing.
    let mut only = LightList::new();
    only.add_backdrop(DomeLight::new(backdrop, None, Mat3A::IDENTITY));
    assert_eq!(
        escaped_emission(&None, &only, Vec3A::Y, MASK_INDIRECT, s),
        Vec3A::ZERO
    );
    // The global switch hides the backdrop too: camera rays see nothing.
    lights.hide_infinite_from_camera();
    assert_eq!(
        escaped_emission(&None, &lights, Vec3A::Y, MASK_CAMERA, s),
        Vec3A::ZERO
    );
    assert_eq!(
        escaped_emission(&None, &lights, Vec3A::Y, MASK_INDIRECT, s),
        hdri
    );
}

/// With no light at infinity an escaping ray is black: there is no
/// built-in sky.
#[test]
fn nothing_at_infinity_is_black() {
    use super::path::escaped_emission;
    use crate::LightList;
    use crate::ray::MASK_CAMERA;
    use glam::Vec3A;
    let s = SamplingStrategy::PowerMis;
    let none = LightList::new();
    assert_eq!(
        escaped_emission(&None, &none, Vec3A::Y, MASK_CAMERA, s),
        Vec3A::ZERO
    );
}
