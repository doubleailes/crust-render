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

/// The cross-neighbour rule on a hand-built index buffer, so the numbers
/// are exact: only a still-sampling up/down/left/right neighbour more than
/// the tolerance above `p` holds it.
#[test]
fn a_cross_neighbour_holds_a_pixel_only_past_the_tolerance() {
    use super::held_by_neighbour;
    let (w, h) = (3usize, 3usize);
    let (x, y) = (1usize, 1usize);
    let t = 1.0f32;
    let own = 0.5f32;
    let mut index = vec![own; w * h];
    let active = vec![true; w * h];
    // Left neighbour at exactly `own + t`: not held.
    index[y * w + x - 1] = own + t;
    assert!(!held_by_neighbour(&index, &active, w, h, x, y, t));
    // ...and a hair above it: held.
    index[y * w + x - 1] = own + t + 1e-3;
    assert!(held_by_neighbour(&index, &active, w, h, x, y, t));
    // A stopped neighbour never holds, whatever its index.
    let mut stopped = active.clone();
    stopped[y * w + x - 1] = false;
    index[y * w + x - 1] = f32::INFINITY;
    assert!(!held_by_neighbour(&index, &stopped, w, h, x, y, t));
    // A negative tolerance ignores every neighbour.
    assert!(!held_by_neighbour(&index, &active, w, h, x, y, -1.0));
    // A diagonal neighbour at +∞ does not hold.
    index[y * w + x - 1] = own;
    index[(y - 1) * w + x - 1] = f32::INFINITY;
    index[(y + 1) * w + x + 1] = f32::INFINITY;
    assert!(!held_by_neighbour(&index, &active, w, h, x, y, t));
    // Each of the four cross neighbours holds on its own, +∞ included.
    for q in [
        y * w + x - 1,
        y * w + x + 1,
        (y - 1) * w + x,
        (y + 1) * w + x,
    ] {
        let mut idx = vec![own; w * h];
        idx[q] = f32::INFINITY;
        assert!(held_by_neighbour(&idx, &active, w, h, x, y, t), "{q}");
    }
}

/// A neighbour outside the image is ignored: corners and edges compare
/// only the neighbours they have.
#[test]
fn neighbours_outside_the_image_are_ignored() {
    use super::held_by_neighbour;
    let (w, h) = (2usize, 1usize);
    let index = [0.5f32, 0.5];
    let active = [true, true];
    assert!(!held_by_neighbour(&index, &active, w, h, 0, 0, 1.0));
    assert!(!held_by_neighbour(&index, &active, w, h, 1, 0, 1.0));
    let index = [0.5f32, 5.0];
    assert!(held_by_neighbour(&index, &active, w, h, 0, 0, 1.0));
}

/// The round schedule (design D8): deterministic, every batch at least 4,
/// none more than a quarter of what was taken before it, and the last one
/// ending exactly at the budget.
#[test]
fn batch_schedule_grows_a_quarter_a_round_and_ends_at_the_budget() {
    use super::batch_schedule;
    let s = batch_schedule(1024, 32);
    assert_eq!(s, batch_schedule(1024, 32), "deterministic");
    assert_eq!(
        s.first(),
        Some(&40),
        "first batch is the floor of 4 raised to 32/4"
    );
    assert_eq!(s.last(), Some(&1024));
    assert_eq!(s.len(), 16, "{s:?}");
    let mut taken = 32;
    for &next in &s {
        let batch = next - taken;
        assert!(batch >= 4 || next == 1024, "{s:?}");
        assert!(batch <= (taken / 4).max(4), "{s:?}");
        taken = next;
    }
    // Small budgets: the batch floor of 4 applies, capped at the budget.
    assert_eq!(batch_schedule(64, 32), vec![40, 50, 62, 64]);
    assert_eq!(batch_schedule(10, 4), vec![8, 10]);
    // Never reaching the first check: nothing to schedule.
    assert!(batch_schedule(16, 32).is_empty());
    assert!(batch_schedule(32, 32).is_empty());
}

/// A budget near `u32::MAX` must schedule without overflowing: targets
/// strictly increase and the last one is the budget.
#[test]
fn batch_schedule_survives_a_budget_near_u32_max() {
    use super::batch_schedule;
    let spp = u32::MAX - 1;
    let s = batch_schedule(spp, u32::MAX - 6);
    assert_eq!(s, vec![spp]);
    let s = batch_schedule(spp, 4);
    assert_eq!(s.last(), Some(&spp));
    assert!(s.windows(2).all(|w| w[0] < w[1]), "strictly increasing");
    assert!(s.len() < 200, "{}", s.len());
    // A minimum past the budget schedules nothing.
    assert!(batch_schedule(64, u32::MAX).is_empty());
}

/// A pass-through walk takes a hit for the surface it just crossed, met
/// again through rounding, when it is the same primitive in the same
/// placement, from the same side, within the re-hit window of the crossing
/// — and for a new surface otherwise: the other side (an exit), another
/// primitive, another placement of the same one, or farther along.
#[test]
fn a_re_hit_is_the_same_primitive_from_the_same_side_within_the_window() {
    use super::path::{LastCrossing, RE_HIT_WINDOW};
    use crate::hittable::HitRecord;
    use crate::rt_world::WorldHit;
    use crate::{OpenPBR, Vec3A};
    let mat = OpenPBR::diffuse(Vec3A::splat(0.5));
    let placed = |geom_id: u32, prim_id: u32, front_face: bool, placement: u32| WorldHit {
        rec: HitRecord {
            front_face,
            ..HitRecord::new()
        },
        mat: &mat,
        geom_id,
        prim_id,
        placement,
    };
    let hit = |geom_id, prim_id, front_face| placed(geom_id, prim_id, front_face, 0);
    for t in [0.02, 0.5, 8.0, 3e4] {
        let last = LastCrossing::of(&hit(3, 7, true), t);
        let window = t.max(1.0) * RE_HIT_WINDOW;
        // The re-hit a restarted segment can report lies past the restart's
        // offset and short of the window.
        let again = t + 0.5 * window;
        assert!(last.repeats(&hit(3, 7, true), again), "at {t}");
        assert!(last.repeats(&hit(3, 7, true), t + 0.99 * window), "at {t}");
        assert!(
            !last.repeats(&hit(3, 7, true), t + 1.01 * window),
            "at {t}: past the window"
        );
        assert!(!last.repeats(&hit(3, 7, false), again), "at {t}: the exit");
        assert!(
            !last.repeats(&hit(3, 8, true), again),
            "at {t}: another primitive"
        );
        assert!(
            !last.repeats(&hit(4, 7, true), again),
            "at {t}: another geometry"
        );
        assert!(
            !last.repeats(&placed(3, 7, true, 5), again),
            "at {t}: another placement of the same primitive"
        );
    }
}

/// The first light sample at a vertex draws exactly what the renderer drew
/// before sample counts existed: the vertex's own `K_NEE` draw, with the
/// pick coordinate untouched. Every later sample draws from a sub-domain of
/// its own.
#[test]
fn one_light_sample_draws_as_before() {
    use super::path::{K_NEE_TEST, nee_sampler, stratified_pick};
    use crate::PathSampler;
    for (x, y, index) in [(0, 0, 0), (3, 7, 5), (640, 359, 1023)] {
        let v = PathSampler::new(x, y, 0, index).new_domain(1).new_domain(2);
        let before = v.new_domain(K_NEE_TEST).draw_sample_f32::<4>();
        let now = nee_sampler(v, 0)
            .new_domain(K_NEE_TEST)
            .draw_sample_f32::<4>();
        assert_eq!(before.map(f32::to_bits), now.map(f32::to_bits));
        assert_eq!(
            stratified_pick(before[0], 1, 0).to_bits(),
            before[0].to_bits()
        );
        // With four samples, each one's draw is its own and its pick lands
        // in its own quarter.
        let mut draws = Vec::new();
        for i in 0..4 {
            let d = nee_sampler(v, i)
                .new_domain(K_NEE_TEST)
                .draw_sample_f32::<4>();
            let pick = stratified_pick(d[0], 4, i);
            assert!(
                pick >= i as f32 / 4.0 && pick < (i + 1) as f32 / 4.0,
                "sample {i} picks at {pick}"
            );
            draws.push(d.map(f32::to_bits));
        }
        draws.sort();
        draws.dedup();
        assert_eq!(draws.len(), 4, "the four samples share a draw");
    }
}

/// Stratified picks (design D1): with four samples over lights selected
/// with probabilities 0.5, 0.25 and 0.25, every vertex samples the first
/// light twice and each other light once — not a binomial count.
#[test]
fn stratified_picks_sample_each_light_count_times_its_probability() {
    use super::path::stratified_pick;
    use crate::{AreaLight, Emissive, LightList, LightSelection, RectShape};
    use glam::Vec3A;
    use std::sync::Arc;
    let mut lights = LightList::new();
    // Power selection: a uniform half plus a half by flux, so fluxes of
    // 4 : 1 : 1 give 1/6 + 1/3, 1/6 + 1/12, 1/6 + 1/12.
    for (id, radiance) in [(0u32, 4.0f32), (1, 1.0), (2, 1.0)] {
        let rect = RectShape::new(
            Vec3A::new(-0.5, 0.0, -0.5),
            Vec3A::new(1.0, 0.0, 0.0),
            Vec3A::new(0.0, 0.0, 1.0),
            -Vec3A::Y,
        );
        lights.add(AreaLight::new(
            rect,
            Arc::new(Emissive::light(Vec3A::splat(radiance), None)),
            id,
        ));
    }
    lights.select_by(LightSelection::Power);
    for (i, want) in [0.5f32, 0.25, 0.25].into_iter().enumerate() {
        assert!(
            (lights.pmf(i) - want).abs() < 1e-6,
            "light {i}: pmf {} vs {want}",
            lights.pmf(i)
        );
    }
    let mut rng = openqmc::pcg::Rng::new(5);
    for _ in 0..1000 {
        let mut counts = [0usize; 3];
        for i in 0..4 {
            let u = rng.next_f32();
            let (index, _) = lights.pick_index(stratified_pick(u, 4, i)).unwrap();
            counts[index] += 1;
        }
        assert_eq!(counts, [2, 1, 1]);
    }
}
