use super::*;
use crate::rt_world::WorldBuilder;
use crust_rt::Geometry;
use std::sync::Arc;

fn world_of(geoms: Vec<Geometry>) -> World {
    let mut b = WorldBuilder::new();
    for g in geoms {
        b.attach(g, Arc::new(ExitLambertian));
    }
    b.commit()
}

/// Mean walk weight (absorbed walks count as zero) from the top of a sphere
/// two hundred mean free paths across — a semi-infinite slab — with a
/// cosine-weighted entry into the medium, which is what Chiang's fit
/// describes.
fn slab_albedo(albedo: Vec3A, radius: Vec3A, g: f32, n: u32) -> Vec3A {
    // Not larger: at a radius of 1000 the sphere's own intersection error
    // reaches the walk's 1e-4 offset, and the entry point hits itself.
    let world = world_of(vec![Geometry::Sphere {
        center: Vec3A::new(0.0, 0.0, -20.0),
        radius: 20.0,
    }]);
    let guide = Vec3A::Z;
    let mut sum = Vec3A::ZERO;
    let mut cost = WalkCost::default();
    for i in 0..n {
        let s = PathSampler::new(0, 0, 0, i as i32);
        let l = cosine_hemisphere(s.new_domain(99).draw_sample_f32::<2>());
        let entry = SubsurfaceEntry {
            dir: Vec3A::new(l.x, l.y, -l.z),
            albedo,
            radius,
            anisotropy: g,
        };
        if let Some(exit) = random_walk(
            &world,
            0,
            Vec3A::ZERO,
            guide,
            true,
            &entry,
            0.0,
            s,
            &mut cost,
        ) {
            let out = (exit.rec.p - Vec3A::new(0.0, 0.0, -20.0)).normalize();
            assert!(exit.rec.normal.dot(out) > 0.99, "outward at {}", exit.rec.p);
            assert!(exit.dir.dot(exit.rec.normal) > 0.0, "leaving outward");
            sum += exit.weight;
        }
    }
    sum / n as f32
}

/// The point of the remap: a walk reflects the colour it was asked for.
/// Chiang's polynomial is a fit, so a couple of percent is its accuracy, not
/// noise (measured: within 0.02 for `g ≤ 0.6` and albedos up to 0.9).
#[test]
fn walk_reflects_the_target_albedo_off_a_slab() {
    for (albedo, g) in [
        (Vec3A::new(0.8, 0.5, 0.2), 0.0),
        (Vec3A::new(0.9, 0.85, 0.3), 0.0),
        (Vec3A::new(0.7, 0.7, 0.7), 0.6),
        (Vec3A::new(0.4, 0.4, 0.4), 0.3),
    ] {
        let got = slab_albedo(albedo, Vec3A::splat(0.1), g, 8192);
        for c in 0..3 {
            assert!(
                (got[c] - albedo[c]).abs() < 0.05,
                "albedo {albedo} g {g}: walk reflects {got}"
            );
        }
    }
}

/// A chromatic radius must not change the reflected colour — only how far
/// light travels. Channel MIS keeps the estimate unbiased for every channel.
#[test]
fn chromatic_radius_keeps_the_albedo() {
    let albedo = Vec3A::new(0.8, 0.6, 0.4);
    let got = slab_albedo(albedo, Vec3A::new(0.4, 0.1, 0.02), 0.0, 8192);
    for c in 0..3 {
        assert!(
            (got[c] - albedo[c]).abs() < 0.05,
            "chromatic radius reflects {got} for {albedo}"
        );
    }
}

/// A slab thinner than the mean free path lets most light through: the walk
/// exits from the far side, found by the extended first ray.
#[test]
fn thin_slab_transmits_through_the_far_side() {
    // Two big spheres would not share a surface; a thin box mesh does.
    let (w, h) = (50.0f32, 0.02f32);
    let v = |x: f32, y: f32, z: f32| Vec3A::new(x, y, z);
    let vertices = [
        v(-w, -w, 0.0),
        v(w, -w, 0.0),
        v(w, w, 0.0),
        v(-w, w, 0.0),
        v(-w, -w, -h),
        v(w, -w, -h),
        v(w, w, -h),
        v(-w, w, -h),
    ];
    let indices = vec![
        [0, 1, 2],
        [0, 2, 3],
        [4, 6, 5],
        [4, 7, 6],
        [0, 4, 5],
        [0, 5, 1],
        [1, 5, 6],
        [1, 6, 2],
        [2, 6, 7],
        [2, 7, 3],
        [3, 7, 4],
        [3, 4, 0],
    ];
    let world = world_of(vec![Geometry::TriangleMesh {
        vertices: vertices.iter().map(|v: &Vec3A| v.to_array()).collect(),
        indices,
        normals: None,
    }]);
    let entry = SubsurfaceEntry {
        dir: -Vec3A::Z,
        albedo: Vec3A::splat(0.9),
        radius: Vec3A::splat(1.0),
        anisotropy: 0.0,
    };
    let (mut top, mut bottom) = (0u32, 0u32);
    let mut cost = WalkCost::default();
    for i in 0..2048 {
        let s = PathSampler::new(0, 0, 0, i);
        if let Some(exit) = random_walk(
            &world,
            0,
            Vec3A::ZERO,
            Vec3A::Z,
            true,
            &entry,
            0.0,
            s,
            &mut cost,
        ) {
            if exit.rec.normal.z > 0.5 {
                top += 1;
            } else if exit.rec.normal.z < -0.5 {
                assert!(exit.rec.p.z < -h + 1e-3);
                bottom += 1;
            }
        }
    }
    assert!(
        bottom > top,
        "a thin slab transmits: top {top} bottom {bottom}"
    );
}

/// The walk sees only its own geometry: a foreign sphere buried inside the
/// medium is stepped past, never exited through.
#[test]
fn walk_ignores_foreign_geometry() {
    let world = world_of(vec![
        Geometry::Sphere {
            center: Vec3A::new(0.0, 0.0, -20.0),
            radius: 20.0,
        },
        Geometry::Sphere {
            center: Vec3A::new(0.0, 0.0, -0.3),
            radius: 0.2,
        },
    ]);
    let entry = SubsurfaceEntry {
        dir: -Vec3A::Z,
        albedo: Vec3A::splat(0.9),
        radius: Vec3A::splat(0.2),
        anisotropy: 0.0,
    };
    let mut cost = WalkCost::default();
    let mut exits = 0;
    for i in 0..1024 {
        let s = PathSampler::new(0, 0, 0, i);
        if let Some(exit) = random_walk(
            &world,
            0,
            Vec3A::ZERO,
            Vec3A::Z,
            true,
            &entry,
            0.0,
            s,
            &mut cost,
        ) {
            let r = (exit.rec.p - Vec3A::new(0.0, 0.0, -20.0)).length();
            assert!((r - 20.0).abs() < 1e-3, "exited at {}", exit.rec.p);
            exits += 1;
        }
    }
    assert!(exits > 512);
}

/// However many foreign surfaces lie in a segment, the owner's boundary
/// behind them is still found: twelve spheres stacked down the entry ray put
/// 24 crossings ahead of the far side, past the 16 the search used to allow
/// before it read "no boundary" and the walk carried on outside its object.
#[test]
fn many_embedded_objects_do_not_hide_the_boundary() {
    let mut geoms = vec![Geometry::Sphere {
        center: Vec3A::new(0.0, 0.0, -20.0),
        radius: 20.0,
    }];
    for k in 0..12 {
        geoms.push(Geometry::Sphere {
            center: Vec3A::new(0.0, 0.0, -2.0 - 3.0 * k as f32),
            radius: 1.0,
        });
    }
    let world = world_of(geoms);
    // A mean free path far beyond the 40-unit object: nearly every walk's
    // first flight runs straight through to the far side.
    let entry = SubsurfaceEntry {
        dir: -Vec3A::Z,
        albedo: Vec3A::splat(0.9),
        radius: Vec3A::splat(1000.0),
        anisotropy: 0.0,
    };
    let mut cost = WalkCost::default();
    let n = 256;
    let mut far_side = 0;
    for i in 0..n {
        let s = PathSampler::new(0, 0, 0, i);
        if let Some(exit) = random_walk(
            &world,
            0,
            Vec3A::ZERO,
            Vec3A::Z,
            true,
            &entry,
            0.0,
            s,
            &mut cost,
        ) && exit.rec.p.z < -39.9
        {
            far_side += 1;
        }
    }
    assert!(
        far_side > n * 9 / 10,
        "{far_side} of {n} walks exited the far side"
    );
}

/// Dwivedi's inverse CDF draws exactly the density `eval_phase_dwivedi`
/// reports — the guided pdf in the walk's MIS depends on it.
#[test]
fn dwivedi_sampling_matches_its_pdf() {
    for alpha in [0.3f32, 0.8, 0.99] {
        let nu = diffusion_length_dwivedi(alpha);
        let log = ((nu + 1.0) / (nu - 1.0)).ln();
        let bins = 16usize;
        let n = 100_000u32;
        let mut hist = vec![0u32; bins];
        for i in 0..n {
            let u = (i as f32 + 0.5) / n as f32;
            let c = sample_phase_dwivedi(nu, log, u).clamp(-1.0, 1.0);
            let b = (((c + 1.0) * 0.5 * bins as f32) as usize).min(bins - 1);
            hist[b] += 1;
        }
        for (b, &count) in hist.iter().enumerate() {
            let lo = -1.0 + 2.0 * b as f32 / bins as f32;
            let hi = lo + 2.0 / bins as f32;
            // The CDF `ln((ν + 1)/(ν − c)) / ln((ν + 1)/(ν − 1))`, exactly:
            // the density is too steep near +1 for a quadrature.
            let cdf = |c: f32| ((nu + 1.0) / (nu - c)).ln() / log;
            let expected = cdf(hi) - cdf(lo);
            // And the density is that CDF's derivative.
            let mid = 0.5 * (lo + hi);
            let dc = 1e-3;
            let slope = (cdf(mid + dc) - cdf(mid - dc)) / (2.0 * dc);
            let pdf = eval_phase_dwivedi(nu, log, mid);
            assert!(
                (slope - pdf).abs() < 1e-2 * pdf.max(1.0),
                "{slope} vs {pdf}"
            );
            let observed = count as f32 / n as f32;
            assert!(
                (observed - expected).abs() < 2e-3,
                "alpha {alpha} bin {b}: {observed} vs {expected}"
            );
        }
    }
}

/// The remap's contract with the walk: the extinction is `1/(r(1 − g))`,
/// the albedo rises with the colour, and the floor records its raw value.
#[test]
fn chiang_remap_shape() {
    let (ext, alpha, raw) =
        chiang_remap(Vec3A::new(0.01, 0.5, 0.99), Vec3A::new(1.0, 0.5, 0.25), 0.5);
    assert!(
        (ext - Vec3A::new(2.0, 4.0, 8.0)).abs().max_element() < 1e-4,
        "{ext}"
    );
    assert!(raw.x < MIN_ALPHA && alpha.x == MIN_ALPHA);
    assert!(raw.y < raw.z && raw.z <= 0.999_999);
    let mut last = 0.0;
    for i in 1..100 {
        let (_, a, raw) = chiang_remap(Vec3A::splat(i as f32 / 100.0), Vec3A::ONE, 0.0);
        assert!(raw.x >= last, "monotonic");
        assert!(a.x >= MIN_ALPHA);
        last = raw.x;
    }
}

/// Where the fit stops fitting, pinned so a change to it is seen. Chiang's
/// albedo exceeds 1 at high anisotropy (1.0003 for colour 0.8 at `g = 0.8`)
/// and near-white colours (1.0010 for 0.95 at `g = 0`), and is clamped
/// just below it, so such a walk reflects more than it was asked for, never
/// more than one — Cycles' and Typhoon's walks share the same polynomial.
#[test]
fn high_anisotropy_over_reflects_boundedly() {
    let got = slab_albedo(Vec3A::new(0.6, 0.8, 0.95), Vec3A::splat(0.1), 0.9, 8192);
    assert!(got.x > 0.62 && got.x < 0.8, "{got}");
    assert!(got.y > 0.82 && got.y < 1.0, "{got}");
    assert!(got.z > 0.85 && got.z < 1.0, "{got}");
    // And a negative anisotropy walks as isotropic, not as a dead fit.
    let iso = slab_albedo(Vec3A::splat(0.6), Vec3A::splat(0.1), -0.5, 4096);
    assert!((iso.x - 0.6).abs() < 0.05, "{iso}");
}

/// Chiang's fit describes a *diffuse* entry. Entered straight down — which a
/// refraction at IOR 1.5 nearly is — the same walk goes deeper and reflects
/// less: (0.78, 0.45, 0.16) for a target of (0.8, 0.5, 0.2). Typhoon enters
/// by refraction and so does crust (`closure::subsurface_entry`); pinned so
/// the difference stays a measured one.
#[test]
fn a_normal_entry_reflects_less_than_the_fit() {
    let world = world_of(vec![Geometry::Sphere {
        center: Vec3A::new(0.0, 0.0, -20.0),
        radius: 20.0,
    }]);
    let entry = SubsurfaceEntry {
        dir: -Vec3A::Z,
        albedo: Vec3A::new(0.8, 0.5, 0.2),
        radius: Vec3A::splat(0.1),
        anisotropy: 0.0,
    };
    let mut sum = Vec3A::ZERO;
    let mut cost = WalkCost::default();
    let n = 8192;
    for i in 0..n {
        let s = PathSampler::new(0, 0, 0, i);
        if let Some(e) = random_walk(
            &world,
            0,
            Vec3A::ZERO,
            Vec3A::Z,
            true,
            &entry,
            0.0,
            s,
            &mut cost,
        ) {
            sum += e.weight;
        }
    }
    let got = sum / n as f32;
    assert!(
        (got - Vec3A::new(0.777, 0.446, 0.163)).abs().max_element() < 0.02,
        "{got}"
    );
}
