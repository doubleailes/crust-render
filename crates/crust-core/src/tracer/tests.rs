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
    use super::{PixelRect, held_by_neighbour};
    let (w, h) = (3usize, 3usize);
    let (x, y) = (1usize, 1usize);
    let t = 1.0f32;
    let own = 0.5f32;
    let mut index = vec![own; w * h];
    let active = vec![true; w * h];
    // Left neighbour at exactly `own + t`: not held.
    index[y * w + x - 1] = own + t;
    assert!(!held_by_neighbour(
        &index,
        &active,
        PixelRect::full(w, h),
        x,
        y,
        t
    ));
    // ...and a hair above it: held.
    index[y * w + x - 1] = own + t + 1e-3;
    assert!(held_by_neighbour(
        &index,
        &active,
        PixelRect::full(w, h),
        x,
        y,
        t
    ));
    // A stopped neighbour never holds, whatever its index.
    let mut stopped = active.clone();
    stopped[y * w + x - 1] = false;
    index[y * w + x - 1] = f32::INFINITY;
    assert!(!held_by_neighbour(
        &index,
        &stopped,
        PixelRect::full(w, h),
        x,
        y,
        t
    ));
    // A negative tolerance ignores every neighbour.
    assert!(!held_by_neighbour(
        &index,
        &active,
        PixelRect::full(w, h),
        x,
        y,
        -1.0
    ));
    // A diagonal neighbour at +∞ does not hold.
    index[y * w + x - 1] = own;
    index[(y - 1) * w + x - 1] = f32::INFINITY;
    index[(y + 1) * w + x + 1] = f32::INFINITY;
    assert!(!held_by_neighbour(
        &index,
        &active,
        PixelRect::full(w, h),
        x,
        y,
        t
    ));
    // Each of the four cross neighbours holds on its own, +∞ included.
    for q in [
        y * w + x - 1,
        y * w + x + 1,
        (y - 1) * w + x,
        (y + 1) * w + x,
    ] {
        let mut idx = vec![own; w * h];
        idx[q] = f32::INFINITY;
        assert!(
            held_by_neighbour(&idx, &active, PixelRect::full(w, h), x, y, t),
            "{q}"
        );
    }
}

/// A neighbour outside the image is ignored: corners and edges compare
/// only the neighbours they have.
#[test]
fn neighbours_outside_the_image_are_ignored() {
    use super::{PixelRect, held_by_neighbour};
    let (w, h) = (2usize, 1usize);
    let index = [0.5f32, 0.5];
    let active = [true, true];
    assert!(!held_by_neighbour(
        &index,
        &active,
        PixelRect::full(w, h),
        0,
        0,
        1.0
    ));
    assert!(!held_by_neighbour(
        &index,
        &active,
        PixelRect::full(w, h),
        1,
        0,
        1.0
    ));
    let index = [0.5f32, 5.0];
    assert!(held_by_neighbour(
        &index,
        &active,
        PixelRect::full(w, h),
        0,
        0,
        1.0
    ));
}

/// A neighbour outside the render region is as absent as one outside the
/// frame (design D5): the planes cover the region only, and a pixel on its
/// border compares only the neighbours inside it.
#[test]
fn neighbours_outside_the_region_are_ignored() {
    use super::{PixelRect, held_by_neighbour};
    // A 2×2 region at (10, 20) of a larger frame, planes region-sized.
    let rect = PixelRect::new(10, 20, 12, 22);
    let active = [true; 4];
    let calm = [0.5f32; 4];
    for (x, y) in [(10, 20), (11, 20), (10, 21), (11, 21)] {
        assert!(!held_by_neighbour(&calm, &active, rect, x, y, 1.0));
    }
    // The pixel right of (10, 20) still holds it.
    let mut index = calm;
    index[rect.index(11, 20)] = 5.0;
    assert!(held_by_neighbour(&index, &active, rect, 10, 20, 1.0));
    assert!(!held_by_neighbour(&index, &active, rect, 10, 21, 1.0));
}

/// Over the full frame the generators emit exactly the units they always
/// did (the pre-region code, kept here as the reference), so a render with
/// no region schedules, and replays, exactly as before.
#[test]
fn a_full_frame_region_yields_the_frame_tiles_and_rows() {
    use super::{PixelRect, TILE, generate_rows, generate_tiles};
    fn old_tiles(w: usize, h: usize, t: usize) -> Vec<[usize; 4]> {
        let mut out = Vec::new();
        for y in (0..h).step_by(t) {
            for x in (0..w).step_by(t) {
                out.push([x, y, (x + t).min(w) - x, (y + t).min(h) - y]);
            }
        }
        out
    }
    let as_arrays = |tiles: Vec<super::Tile>| -> Vec<[usize; 4]> {
        tiles
            .iter()
            .map(|t| [t.x, t.y, t.width, t.height])
            .collect()
    };
    for (w, h) in [(640, 360), (1, 1), (17, 33), (16, 16), (100, 7)] {
        let full = PixelRect::full(w, h);
        assert_eq!(as_arrays(generate_tiles(full, TILE)), old_tiles(w, h, TILE));
        let rows: Vec<[usize; 4]> = (0..h).map(|y| [0, y, w, 1]).collect();
        assert_eq!(as_arrays(generate_rows(full)), rows);
    }
}

/// A region's tiles are the frame grid's tiles clipped to it: every edge
/// on a multiple of the tile size or on the region's border, the region
/// covered exactly once, in tile rows by increasing `y`.
#[test]
fn region_tiles_keep_the_frame_grid() {
    use super::{PixelRect, TILE, generate_rows, generate_tiles};
    let rect = PixelRect::new(37, 21, 101, 77);
    let tiles = generate_tiles(rect, TILE);
    let mut covered = vec![0u8; rect.area()];
    for t in &tiles {
        assert!(t.x == rect.x0 || t.x % TILE == 0, "x {}", t.x);
        assert!(t.y == rect.y0 || t.y % TILE == 0, "y {}", t.y);
        let (x1, y1) = (t.x + t.width, t.y + t.height);
        assert!(x1 == rect.x1 || x1 % TILE == 0);
        assert!(y1 == rect.y1 || y1 % TILE == 0);
        for y in t.y..y1 {
            for x in t.x..x1 {
                covered[rect.index(x, y)] += 1;
            }
        }
    }
    assert!(covered.iter().all(|&n| n == 1));
    assert_eq!(tiles[0].x, 37);
    assert_eq!((tiles[0].y, tiles[0].height), (21, 11));
    assert!(tiles.windows(2).all(|p| p[0].y <= p[1].y));
    let rows = generate_rows(rect);
    assert_eq!(rows.len(), 56);
    assert!(
        rows.iter()
            .all(|r| r.x == 37 && r.width == 64 && r.height == 1)
    );
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

/// The last slice's pick stays below 1 for a draw within an ulp of 1, at
/// every count: a coordinate of 1.0 would land on the last light whatever
/// its probability. One sample passes the draw through untouched.
#[test]
fn a_stratified_pick_never_reaches_one() {
    use super::path::stratified_pick;
    let below_one = 1.0 - f32::EPSILON / 2.0;
    assert!(below_one < 1.0 && (below_one + f32::EPSILON / 2.0) == 1.0);
    for count in [2u32, 3, 4, 7, 1024] {
        for u in [below_one, 0.9999999, 0.999999, 0.5] {
            let pick = stratified_pick(u, count, count - 1);
            assert!(pick < 1.0, "count {count}, u {u}: pick {pick}");
            assert!(pick >= (count - 1) as f32 / count as f32);
        }
    }
    assert_eq!(
        stratified_pick(below_one, 1, 0).to_bits(),
        below_one.to_bits()
    );
    // Without the clamp the last slice does round up to 1.
    assert_eq!((1.0 + below_one) / 2.0, 1.0);
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

/// The region defaults to the frame, is clipped to it, refuses what
/// clipping empties (keeping nothing of it), and is reset by a new
/// resolution.
#[test]
fn the_region_is_clipped_to_the_frame_and_reset_by_the_resolution() {
    use super::{PixelRect, RenderSettings};
    let s = RenderSettings::default().with_resolution(640, 360);
    assert_eq!(s.region(), PixelRect::full(640, 360));
    assert!(s.is_full_frame());
    let r = s
        .with_region(PixelRect::new(600, 300, 700, 400))
        .expect("overlaps the frame");
    assert_eq!(r.region(), PixelRect::new(600, 300, 640, 360));
    assert!(!r.is_full_frame());
    // Raster rows count from the bottom.
    assert_eq!(r.raster_region(), PixelRect::new(600, 0, 640, 60));
    let err = s
        .with_region(PixelRect::new(700, 0, 800, 100))
        .expect_err("outside the frame");
    assert!(err.to_string().contains("640x360"), "{err}");
    assert_eq!(
        r.with_resolution(64, 32).region(),
        PixelRect::full(64, 32),
        "a new resolution resets the region"
    );
}

/// A sample stage at a small resolution, for the renderer tests below.
fn sample_scene(name: &str) -> crate::Scene {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name);
    crate::Scene::from_usd(&path).unwrap_or_else(|e| panic!("load {name}: {e}"))
}

fn bits(buffer: &crate::Buffer) -> Vec<u32> {
    let (w, h) = buffer.size();
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let (r, g, b) = buffer.get_rgb(x, y);
            [r.to_bits(), g.to_bits(), b.to_bits()]
        })
        .collect()
}

/// `new(s)` and `new(s0).reconfigure(s)` render the same image, bit for
/// bit, for every setting the diagnostic varies — `new` is `reconfigure` on
/// a fresh renderer, and `reconfigure` leaves nothing of `s0` behind.
#[test]
fn reconfigure_renders_what_new_renders() {
    use crate::{LightSelection, Renderer};
    for name in ["cornellbox.usda", "veach_mis.usda"] {
        let base = sample_scene(name)
            .settings
            .with_resolution(48, 32)
            .with_samples_per_pixel(16);
        // Everything the diagnostic varies, set away from every case below.
        let s0 = base
            .with_sampling_strategy(SamplingStrategy::BsdfOnly)
            .with_light_selection(LightSelection::Learned)
            .with_light_samples(3, 3)
            .with_max_depth(2);
        let cases = [
            base.with_sampling_strategy(SamplingStrategy::BalanceMis),
            base.with_light_selection(LightSelection::Uniform),
            base.with_light_selection(LightSelection::Power),
            base.with_light_selection(LightSelection::Learned),
            base.with_light_samples(2, 1),
            base.with_light_samples(1, 2),
            base.with_max_depth(3),
        ];
        for (k, s) in cases.into_iter().enumerate() {
            let scene = sample_scene(name);
            let fresh =
                Renderer::new(scene.camera, scene.world, scene.lights, s).render_with_tiles();
            let scene = sample_scene(name);
            let mut r = Renderer::new(scene.camera, scene.world, scene.lights, s0);
            r.reconfigure(s);
            assert_eq!(r.lights.selection(), {
                let scene = sample_scene(name);
                Renderer::new(scene.camera, scene.world, scene.lights, s)
                    .lights
                    .selection()
            });
            assert!(
                bits(&fresh) == bits(&r.render_with_tiles()),
                "{name}: case {k} differs after reconfigure"
            );
        }
    }
}

/// `new(s0).retune(s)` renders what `new(s)` renders, bit for bit, under
/// `learned`: kept when only the samples or the region changed (no second
/// pre-pass), rebuilt when the resolution or the strategy did. `s0` is at 8
/// spp so every case changes the samples, and every compared image is at 16.
#[test]
fn retune_renders_what_new_renders() {
    use super::PixelRect;
    use crate::{LightSelection, Renderer};
    let name = "veach_mis.usda";
    let s0 = sample_scene(name)
        .settings
        .with_resolution(48, 32)
        .with_samples_per_pixel(8)
        .with_light_selection(LightSelection::Learned);
    let at16 = s0.with_samples_per_pixel(16);
    let cropped = at16
        .with_region(PixelRect::new(8, 4, 40, 28))
        .expect("region inside the frame");
    let cases = [
        (at16, true),
        (cropped, true),
        (at16.with_resolution(32, 24), false),
        (at16.with_light_selection(LightSelection::Power), false),
    ];
    for (k, (s, kept)) in cases.into_iter().enumerate() {
        let scene = sample_scene(name);
        let fresh = Renderer::new(scene.camera, scene.world, scene.lights, s).render_with_tiles();
        let scene = sample_scene(name);
        let mut r = Renderer::new(scene.camera, scene.world, scene.lights, s0);
        let setup = r.retune(s);
        assert_eq!(setup.is_zero(), kept, "case {k}: kept the selection");
        assert!(
            bits(&fresh) == bits(&r.render_with_tiles()),
            "case {k} differs after retune"
        );
    }
}

/// The clamp counter reports in the image's units: under a camera exposure the
/// luminance it saw removed scales with the beauty, so a share of the beauty
/// (what `crust diagnostic` reports) does not move with the exposure.
#[test]
fn the_clamp_counter_scales_with_the_exposure() {
    use super::Instruments;
    use crate::Renderer;
    let measure = |exposure: f32| {
        let scene = sample_scene("cornellbox.usda");
        let s = scene
            .settings
            .with_resolution(48, 32)
            .with_samples_per_pixel(16)
            .with_indirect_clamp(0.0)
            .with_exposure_scale(exposure);
        let instruments = Instruments {
            clamp: Some(0.25),
            ..Instruments::default()
        };
        Renderer::new(scene.camera, scene.world, scene.lights, s)
            .render_measured(None, instruments)
            .clamp
    };
    let (one, four) = (measure(1.0), measure(4.0));
    assert!(one.removed_luminance > 0.0, "{one:?}");
    assert_eq!(four.pixels_touched, one.pixels_touched);
    assert_eq!(four.removed_luminance, 4.0 * one.removed_luminance);
}

/// The clamp counter observes only: the image is the unclamped one, bit
/// for bit, and what it measures is exactly the luminance the clamp takes
/// from the image when it is on.
#[test]
fn the_clamp_counter_measures_what_the_clamp_removes() {
    use super::Instruments;
    use crate::Renderer;
    let limit = 0.25;
    let settings = sample_scene("cornellbox.usda")
        .settings
        .with_resolution(48, 32)
        .with_samples_per_pixel(16);
    let render = |clamp: f32, instruments: Instruments| {
        let scene = sample_scene("cornellbox.usda");
        let s = settings.with_indirect_clamp(clamp);
        Renderer::new(scene.camera, scene.world, scene.lights, s).render_measured(None, instruments)
    };
    let off = render(0.0, Instruments::default());
    let measured = render(
        0.0,
        Instruments {
            clamp: Some(limit),
            ..Instruments::default()
        },
    );
    let clamped = render(limit, Instruments::default());
    assert!(
        bits(&off.buffer) == bits(&measured.buffer),
        "the counter changed the image"
    );
    // Off by default.
    assert_eq!(off.clamp.pixels, 0);
    let m = measured.clamp;
    assert_eq!(m.pixels, 48 * 32);
    assert!(m.pixels_touched > 0 && m.removed_luminance > 0.0, "{m:?}");
    // With the clamp on, there is nothing for the counter to measure.
    let on = {
        let scene = sample_scene("cornellbox.usda");
        Renderer::new(
            scene.camera,
            scene.world,
            scene.lights,
            settings.with_indirect_clamp(limit),
        )
        .render_measured(
            None,
            Instruments {
                clamp: Some(limit),
                ..Instruments::default()
            },
        )
    };
    assert_eq!(on.clamp.pixels, 0);
    let luma = sample_scene("cornellbox.usda").lights.luma();
    let total = |b: &crate::Buffer| -> f64 {
        let (w, h) = b.size();
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| {
                let (r, g, b) = b.get_rgb(x, y);
                luma.of(crate::Vec3A::new(r, g, b)) as f64
            })
            .sum()
    };
    let diff = total(&off.buffer) - total(&clamped.buffer);
    let rel = (diff - m.removed_luminance).abs() / diff;
    assert!(
        rel < 1e-4,
        "image lost {diff}, the counter measured {}",
        m.removed_luminance
    );
}

/// The unit timer is off by default, and on gives every tile of the frame
/// a time.
#[test]
fn the_tile_timer_times_every_unit_when_asked() {
    use super::Instruments;
    use crate::Renderer;
    let scene = sample_scene("cornellbox.usda");
    let s = scene
        .settings
        .with_resolution(40, 20)
        .with_samples_per_pixel(2);
    let r = Renderer::new(scene.camera, scene.world, scene.lights, s);
    assert!(
        r.render_measured(None, Instruments::default())
            .tiles
            .is_empty()
    );
    let m = r.render_measured(
        None,
        Instruments {
            tile_times: true,
            variance: true,
            ..Instruments::default()
        },
    );
    // 40×20 in 16×16 tiles: 3 × 2.
    assert_eq!(m.tiles.len(), 6);
    assert_eq!(m.tiles.iter().map(|(t, _)| t.area()).sum::<usize>(), 800);
    assert!(m.tiles.iter().all(|&(_, s)| s >= 0.0));
    assert_eq!(m.var_map.len(), 800);
    assert!(m.render_s > 0.0);
}

/// The first sweep's stages (design D1) double from 1 up to the sweep's end,
/// which they always include, and never overflow.
#[test]
fn sweep_stages_double_up_to_the_sweep_end() {
    use super::sweep_stages;
    // The adaptive default: the first check at 32.
    assert_eq!(sweep_stages(32), vec![1, 2, 4, 8, 16, 32]);
    // Adaptive sampling off at 100 spp: the budget is the last stage.
    assert_eq!(sweep_stages(100), vec![1, 2, 4, 8, 16, 32, 64, 100]);
    assert_eq!(sweep_stages(3), vec![1, 2, 3]);
    assert_eq!(sweep_stages(1), vec![1]);
    let s = sweep_stages(u32::MAX);
    assert_eq!(s.len(), 33);
    assert_eq!(s.last(), Some(&u32::MAX));
    assert!(s.windows(2).all(|w| w[0] < w[1]), "strictly increasing");
}

/// A pixel that took no sample (only a cancelled render leaves one) is black
/// with no variance, never the NaN of `0 / 0`.
#[test]
fn a_pixel_with_no_sample_estimates_to_zero() {
    let (color, variance) = super::PixelState::new().estimate();
    assert_eq!(color, glam::Vec3A::ZERO);
    assert_eq!(variance, 0.0);
}

/// An AOV request over every kind of plane: the beauty with its alpha, a
/// closest depth (cleared to `+inf`), a filtered one (averaged over hits
/// only), the normal, a light path expression, the sample count and the
/// variance.
fn every_kind_of_aov() -> crate::AovRequest {
    use crate::{Accumulation, AovProduct, AovRequest, AovSource, AovVar, Precision};
    let var = |name: &str, source: AovSource| AovVar {
        prim_path: format!("/Render/Vars/{name}"),
        name: name.to_owned(),
        channel_prefix: None,
        source,
        components: source.components(),
        precision: Precision::Float,
        accumulation: source.default_accumulation(),
        clear: source.default_clear(),
        expression: None,
        raw: false,
        variance: false,
    };
    let mut beauty = var("beauty", AovSource::Color);
    beauty.components = 4;
    let mut filtered_depth = var("depth_filtered", AovSource::Depth);
    filtered_depth.accumulation = Accumulation::Filtered;
    let mut diffuse = var("diffuse", AovSource::Lpe);
    diffuse.expression = Some("C<RD>[LO]".into());
    AovRequest {
        products: vec![AovProduct {
            prim_path: "/Render/p".into(),
            name: "p.exr".into(),
            vars: vec![
                beauty,
                var("depth", AovSource::Depth),
                filtered_depth,
                var("N", AovSource::Normal),
                diffuse,
                var("sampleCount", AovSource::SampleCount),
                var("variance", AovSource::Variance),
            ],
            attributes: Vec::new(),
        }],
    }
}

/// Every plane `request` writes from `film`, as bits, in request order.
fn film_bits(
    request: &crate::AovRequest,
    film: &crate::AovFilm,
    beauty: &crate::Buffer,
) -> Vec<Vec<u32>> {
    request
        .products
        .iter()
        .flat_map(|p| &p.vars)
        .flat_map(|v| film.var_channels(beauty, v))
        .map(|plane| plane.iter().map(|x| x.to_bits()).collect())
        .collect()
}

/// Cornellbox at a small resolution and `spp`, for the render-control
/// tests below: 48×32 is six 16×16 tiles.
fn small_cornell(spp: u32) -> crate::Renderer {
    let scene = sample_scene("cornellbox.usda");
    let s = scene
        .settings
        .with_resolution(48, 32)
        .with_samples_per_pixel(spp);
    crate::Renderer::new(scene.camera, scene.world, scene.lights, s)
}

/// Staging is scheduling only (design D1): a staged final pass renders the
/// image, every AOV plane and every counter of the same pass swept in one
/// go, bit for bit, under tiles and scanlines alike — adaptive, adaptive
/// sampling off, and with AOVs.
#[test]
fn a_staged_sweep_renders_the_unstaged_one_bit_for_bit() {
    use super::Instruments;
    use crate::Renderer;
    let cornell = |settings: fn(super::RenderSettings) -> super::RenderSettings| {
        let scene = sample_scene("cornellbox.usda");
        let s = settings(scene.settings.with_resolution(48, 32));
        Renderer::new(scene.camera, scene.world, scene.lights, s)
    };
    let aovs = every_kind_of_aov();
    let cases: Vec<(&str, Renderer, Option<&crate::AovRequest>)> = vec![
        // The first check at 32 (stages 1 … 32), then rounds to 64, with a
        // threshold loose enough that pixels do stop early.
        (
            "adaptive",
            cornell(|s| s.with_samples_per_pixel(64).with_adaptive_sampling(32, 0.2)),
            None,
        ),
        // Stages 1 … 16, then 24: the budget.
        (
            "non-adaptive",
            cornell(|s| s.with_samples_per_pixel(24).with_adaptive_sampling(32, 0.0)),
            None,
        ),
        (
            "AOVs",
            cornell(|s| s.with_samples_per_pixel(40).with_adaptive_sampling(16, 0.2)),
            Some(&aovs),
        ),
    ];
    for (name, r, request) in &cases {
        let render = |tiled: bool, unstaged: bool| {
            let layout = request.and_then(|q| r.layout_for(q));
            let instruments = Instruments {
                unstaged,
                ..Instruments::default()
            };
            let m = r.render_impl(tiled, None, layout.as_ref(), instruments, None);
            let planes = match (request, &m.film) {
                (Some(q), Some(film)) => film_bits(q, film, &m.buffer),
                _ => Vec::new(),
            };
            (bits(&m.buffer), planes, m.rays)
        };
        let reference = render(true, true);
        assert!(!reference.0.is_empty());
        if request.is_some() {
            assert!(!reference.1.is_empty(), "{name}: no AOV planes");
        }
        if *name == "adaptive" {
            assert!(reference.2.early_stopped > 0, "{name}: {:?}", reference.2);
        }
        for (tiled, unstaged) in [(true, false), (false, true), (false, false)] {
            let (image, planes, rays) = render(tiled, unstaged);
            let case = format!("{name}, tiled {tiled}, unstaged {unstaged}");
            assert!(image == reference.0, "{case}: the image differs");
            assert_eq!(planes.len(), reference.1.len(), "{case}");
            for (k, (a, b)) in planes.iter().zip(&reference.1).enumerate() {
                assert!(a == b, "{case}: AOV plane {k} differs");
            }
            assert_eq!(rays, reference.2, "{case}: the counters differ");
        }
    }
}

/// A snapshot shows the image as it is written: under a camera exposure the
/// last one is the exposed image the render returned, bit for bit.
#[test]
fn the_last_snapshot_is_exposed() {
    use crate::RenderControl;
    let scene = sample_scene("cornellbox.usda");
    let s = scene
        .settings
        .with_resolution(48, 32)
        .with_samples_per_pixel(16)
        .with_exposure_scale(3.0);
    let r = crate::Renderer::new(scene.camera, scene.world, scene.lights, s);
    for tiled in [true, false] {
        let control = RenderControl::new();
        let out = r.render_with_control(tiled, None, None, &control);
        let (_, snapshot) = control.snapshot().expect("the render published");
        assert!(
            bits(&snapshot) == bits(&out.buffer),
            "tiled {tiled}: the snapshot is not the exposed image"
        );
    }
}

/// The control's last snapshot of a render that completed is the
/// image the render returned, bit for bit, at the control's generation —
/// and that image is the one a render without a control returns. Before
/// the render there is no snapshot.
#[test]
fn the_last_snapshot_is_the_returned_image() {
    use crate::{RenderControl, RenderOutcome};
    let r = small_cornell(64);
    for tiled in [true, false] {
        let control = RenderControl::new();
        assert!(control.snapshot().is_none());
        assert_eq!(control.generation(), 0);
        let out = r.render_with_control(tiled, None, None, &control);
        assert_eq!(out.outcome, RenderOutcome::Completed);
        assert!(out.film.is_none(), "no request, no film");
        let (generation, snapshot) = control.snapshot().expect("the render published");
        assert_eq!(generation, control.generation());
        // At least once per unit per stage (1 … 32): 6 tiles, or 32 rows.
        let units = if tiled { 6 } else { 32 };
        assert!(generation >= units * 6, "tiled {tiled}: {generation}");
        assert!(
            bits(&snapshot) == bits(&out.buffer),
            "tiled {tiled}: the snapshot is not the image"
        );
        let (plain, rays) = r.render_with_stats(tiled, &|_, _| {});
        assert!(bits(&plain) == bits(&out.buffer), "tiled {tiled}");
        assert_eq!(rays, out.rays);
    }
}

/// The samples a control reports reached are the first sweep's completed
/// stages: none before the render or under a cancel that came first, the
/// whole budget when the sweep covers it (4 spp, below the first check
/// point), and the first check point when adaptive rounds follow it.
#[test]
fn samples_reached_follow_the_first_sweep() {
    use crate::RenderControl;
    let control = RenderControl::new();
    assert_eq!(control.samples_reached(), 0);
    small_cornell(4).render_with_control(true, None, None, &control);
    assert_eq!(control.samples_reached(), 4);
    let control = RenderControl::new();
    small_cornell(64).render_with_control(true, None, None, &control);
    assert_eq!(control.samples_reached(), 32, "the first check point");
    let control = RenderControl::new();
    control.cancel();
    small_cornell(64).render_with_control(true, None, None, &control);
    assert_eq!(control.samples_reached(), 0, "cancelled before any stage");
    // A render starts over: a count left from an earlier one is cleared
    // before its first stage.
    let control = RenderControl::new();
    control.reach(8);
    control.cancel();
    small_cornell(64).render_with_control(true, None, None, &control);
    assert_eq!(control.samples_reached(), 0, "a new pass starts at zero");
}

/// Snapshots read from another thread while the render runs only move
/// forward: each new generation is above the last one read, and no
/// snapshot holds a NaN (unsampled pixels read as zero).
#[test]
fn snapshots_read_from_another_thread_only_move_forward() {
    use crate::{RenderControl, RenderOutcome};
    use std::sync::atomic::{AtomicBool, Ordering};
    let r = small_cornell(256);
    let control = RenderControl::new();
    let done = AtomicBool::new(false);
    let (outcome, reads) = std::thread::scope(|s| {
        let reader = s.spawn(|| {
            let mut reads: Vec<u64> = Vec::new();
            while !done.load(Ordering::SeqCst) {
                if let Some((generation, image)) = control.snapshot() {
                    assert!(
                        bits(&image).iter().all(|&b| !f32::from_bits(b).is_nan()),
                        "a NaN at generation {generation}"
                    );
                    if reads.last() != Some(&generation) {
                        reads.push(generation);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            reads
        });
        let out = r.render_with_control(true, None, None, &control);
        done.store(true, Ordering::SeqCst);
        (out.outcome, reader.join().expect("the reader"))
    });
    assert_eq!(outcome, RenderOutcome::Completed);
    assert!(reads.len() >= 2, "{reads:?}");
    assert!(reads.windows(2).all(|w| w[0] < w[1]), "{reads:?}");
}

/// A control cancelled before the render is called stops it before any
/// sample: the image is black, every AOV holds its clear value, and the
/// counters are zero.
#[test]
fn a_render_cancelled_before_it_starts_traces_nothing() {
    use crate::{AovSource, RenderControl, RenderOutcome};
    let request = every_kind_of_aov();
    {
        let scene = sample_scene("cornellbox.usda");
        let s = scene
            .settings
            .with_resolution(40, 20)
            .with_samples_per_pixel(64);
        let r = crate::Renderer::new(scene.camera, scene.world, scene.lights, s);
        let control = RenderControl::new();
        control.cancel();
        let out = r.render_with_control(true, None, Some(&request), &control);
        assert_eq!(out.outcome, RenderOutcome::Cancelled);
        assert_eq!(out.rays.camera_rays, 0);
        assert!(bits(&out.buffer).iter().all(|&b| b == 0));
        let film = out.film.expect("a request gives a film");
        for var in request.products.iter().flat_map(|p| &p.vars) {
            let clear = match var.source {
                AovSource::Color | AovSource::SampleCount | AovSource::Variance => 0.0,
                _ => var.clear,
            };
            for plane in film.var_channels(&out.buffer, var) {
                assert!(
                    plane.iter().all(|v| v.to_bits() == clear.to_bits()),
                    "{} is not at its clear value {clear}",
                    var.name
                );
            }
        }
        assert!(control.is_cancelled(), "cancel is sticky");
    }
}

/// Cancelled from another thread mid-render, a render returns promptly with
/// what it traced: no NaN anywhere, the counters exactly the samples its
/// pixels took, and the progress left where it stopped.
#[test]
fn a_render_cancelled_mid_way_returns_promptly_with_what_it_traced() {
    use crate::{RenderControl, RenderOutcome};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};
    // Minutes in a debug build, were it not cancelled.
    let r = small_cornell(4096);
    let request = every_kind_of_aov();
    for tiled in [true, false] {
        let control = RenderControl::new();
        let (last, total) = (AtomicU64::new(0), AtomicU64::new(0));
        let progress = |n: u64, all: u64| {
            last.store(n, Ordering::SeqCst);
            total.store(all, Ordering::SeqCst);
        };
        let (out, latency) = std::thread::scope(|s| {
            let canceller = s.spawn(|| {
                while control.generation() < 4 {
                    std::thread::sleep(Duration::from_millis(1));
                }
                control.cancel();
                Instant::now()
            });
            let out = r.render_with_control(tiled, Some(&progress), Some(&request), &control);
            let returned = Instant::now();
            let cancelled = canceller.join().expect("the canceller");
            (out, returned.saturating_duration_since(cancelled))
        });
        assert_eq!(out.outcome, RenderOutcome::Cancelled, "tiled {tiled}");
        assert!(
            latency < Duration::from_secs(10),
            "tiled {tiled}: {latency:?}"
        );
        assert!(
            bits(&out.buffer)
                .iter()
                .all(|&b| !f32::from_bits(b).is_nan()),
            "tiled {tiled}: a NaN in the image"
        );
        let film = out.film.expect("a request gives a film");
        for plane in film_bits(&request, &film, &out.buffer) {
            assert!(
                plane.iter().all(|&b| !f32::from_bits(b).is_nan()),
                "tiled {tiled}: a NaN in an AOV"
            );
        }
        let rays = out.rays;
        assert!(rays.camera_rays > 0, "tiled {tiled}");
        assert_eq!(rays.camera_rays, rays.adaptive_samples, "tiled {tiled}");
        assert!(rays.spp_max < 4096, "tiled {tiled}: {rays:?}");
        let (last, total) = (last.load(Ordering::SeqCst), total.load(Ordering::SeqCst));
        // 64 steps a unit (the cap): six tiles, or 32 rows. At 4096 spp a
        // unit's first step comes at its 64th sample, so the render may have
        // stopped before any report.
        let all = if tiled { 6 } else { 32 } * 64;
        assert!(total == 0 || total == all, "tiled {tiled}: total {total}");
        assert!(last < all, "tiled {tiled}: progress walked to {last}/{all}");
    }
}

/// Cancelled once its 4 spp stage is done, a render holds exactly that stage
/// in every pixel: the same image, AOVs and counters as a render of 4 spp
/// (whose first check point is past its budget), since each pixel is
/// estimated from its own samples alone — adaptive or not: a final pass
/// with adaptive sampling off fills the per-pixel sample counters too, so
/// the interruption warning reports its samples like any other.
#[test]
fn a_render_cancelled_after_a_stage_is_that_stage_everywhere() {
    use crate::{RenderControl, RenderOutcome};
    use std::sync::atomic::{AtomicU64, Ordering};
    let request = every_kind_of_aov();
    // The scene's own threshold, then adaptive sampling off.
    for threshold in [None, Some(0.0)] {
        let cornell = |spp: u32| {
            let r = small_cornell(spp);
            match threshold {
                Some(t) => {
                    let s = r.settings.with_adaptive_sampling(32, t);
                    crate::Renderer::new(r.camera, r.world, r.lights, s)
                }
                None => r,
            }
        };
        // 64 spp: one progress step per tile per sample, so on six tiles the
        // third stage (4 spp) ends at step 24.
        let r = cornell(64);
        let control = RenderControl::new();
        let last = AtomicU64::new(0);
        let progress = |n: u64, _: u64| {
            last.store(n, Ordering::SeqCst);
            if n == 24 {
                control.cancel();
            }
        };
        let out = r.render_with_control(true, Some(&progress), Some(&request), &control);
        let case = format!("threshold {threshold:?}");
        assert_eq!(out.outcome, RenderOutcome::Cancelled, "{case}");
        assert_eq!(
            last.load(Ordering::SeqCst),
            24,
            "{case}: walked to the total"
        );
        assert_eq!((out.rays.spp_min, out.rays.spp_max), (4, 4), "{case}");
        assert_eq!(out.rays.adaptive_pixels, 48 * 32, "{case}");
        assert_eq!(out.rays.camera_rays, 48 * 32 * 4, "{case}");
        let (image, film, rays) = cornell(4).render_with_aovs(true, &|_, _| {}, &request);
        assert!(
            bits(&out.buffer) == bits(&image),
            "{case}: the image is not the 4 spp one"
        );
        let partial = out.film.expect("a request gives a film");
        assert!(
            film_bits(&request, &partial, &out.buffer) == film_bits(&request, &film, &image),
            "{case}"
        );
        assert_eq!(out.rays, rays, "{case}");
    }
}

/// A control without snapshots still cancels, and the render publishes
/// nothing into it; a completed render through it is the plain render.
#[test]
fn a_control_without_snapshots_only_cancels() {
    use crate::{RenderControl, RenderOutcome};
    let r = small_cornell(16);
    let control = RenderControl::without_snapshots();
    let out = r.render_with_control(true, None, None, &control);
    assert_eq!(out.outcome, RenderOutcome::Completed);
    assert!(control.snapshot().is_none());
    assert_eq!(control.generation(), 0);
    assert!(bits(&out.buffer) == bits(&r.render_with_tiles()));
    control.cancel();
    let out = r.render_with_control(true, None, None, &control);
    assert_eq!(out.outcome, RenderOutcome::Cancelled);
    assert_eq!(out.rays.camera_rays, 0);
}

/// Progress counts samples, not units — one step per sample per pixel each
/// unit is scheduled, up to 64 a unit, shared out in proportion past that —
/// one step at a time, and a render reaches its total whether its pixels
/// ran the whole budget or stopped early.
#[test]
fn progress_steps_count_samples_and_reach_their_total() {
    use std::sync::Mutex;
    // (spp, threshold): adaptive, adaptive and mostly stopping early, off,
    // and a budget past the cap that converges long before it.
    for (spp, threshold) in [(64, 0.05f32), (64, 0.5), (24, 0.0), (1000, 0.5)] {
        let scene = sample_scene("cornellbox.usda");
        let s = scene
            .settings
            .with_resolution(48, 32)
            .with_samples_per_pixel(spp)
            .with_adaptive_sampling(16, threshold);
        let r = crate::Renderer::new(scene.camera, scene.world, scene.lights, s);
        let seen = Mutex::new((0u64, 0u64));
        let cb = |done: u64, total: u64| {
            let mut seen = seen.lock().unwrap();
            assert_eq!(done, seen.0 + 1, "spp {spp}, threshold {threshold}");
            *seen = (done, total);
        };
        let (_, rays) = r.render_with_stats(true, &cb);
        let (done, total) = *seen.lock().unwrap();
        // 48×32 is six 16×16 tiles.
        assert_eq!(
            total,
            6 * (spp as u64).min(64),
            "spp {spp}, threshold {threshold}"
        );
        assert_eq!(done, total, "spp {spp}, threshold {threshold}");
        if threshold == 0.5 {
            assert!(rays.early_stopped > 0, "{rays:?}");
        }
    }
}
