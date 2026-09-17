//! `EnvironmentMap`: the lat-long direction mapping and the luminance ×
//! sin θ importance sampling behind `DomeLight`.

use crust_core::{EnvironmentMap, Vec3A};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as u32 & 0x00FF_FFFF) as f32 / 16_777_216.0
    }
    fn dir(&mut self) -> Vec3A {
        // Uniform on the sphere.
        let z = 1.0 - 2.0 * self.next();
        let r = (1.0 - z * z).max(0.0).sqrt();
        let phi = std::f32::consts::TAU * self.next();
        Vec3A::new(r * phi.cos(), z, r * phi.sin())
    }
}

fn approx(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

/// 2×2 map with four distinct colours.
fn quad_map() -> EnvironmentMap {
    EnvironmentMap::new(
        2,
        2,
        vec![
            Vec3A::new(1.0, 0.0, 0.0), // row 0 (top), column 0
            Vec3A::new(0.0, 1.0, 0.0), // row 0, column 1
            Vec3A::new(0.0, 0.0, 1.0), // row 1 (bottom), column 0
            Vec3A::new(1.0, 1.0, 0.0), // row 1, column 1
        ],
    )
    .unwrap()
}

#[test]
fn constructor_rejects_degenerate_buffers() {
    assert!(EnvironmentMap::new(0, 4, vec![]).is_none());
    assert!(EnvironmentMap::new(4, 0, vec![]).is_none());
    assert!(EnvironmentMap::new(2, 2, vec![Vec3A::ONE; 3]).is_none());
    assert!(EnvironmentMap::new(2, 2, vec![Vec3A::ONE; 5]).is_none());
    assert!(EnvironmentMap::new(1, 1, vec![Vec3A::ONE]).is_some());
}

#[test]
fn dimensions_are_reported() {
    let m = EnvironmentMap::new(6, 3, vec![Vec3A::ONE; 18]).unwrap();
    assert_eq!(m.width(), 6);
    assert_eq!(m.height(), 3);
}

#[test]
fn row_zero_is_the_upper_pole() {
    let m = quad_map();
    let up = m.radiance(Vec3A::Y);
    assert!(
        up == Vec3A::new(1.0, 0.0, 0.0) || up == Vec3A::new(0.0, 1.0, 0.0),
        "{up}"
    );
    let down = m.radiance(-Vec3A::Y);
    assert!(
        down == Vec3A::new(0.0, 0.0, 1.0) || down == Vec3A::new(1.0, 1.0, 0.0),
        "{down}"
    );
    // Anything above the horizon reads the top row.
    let mut rng = Lcg(1);
    for _ in 0..200 {
        let mut d = rng.dir();
        d.y = d.y.abs().max(0.01);
        let c = m.radiance(d.normalize());
        assert_eq!(c.z, 0.0, "{d} read the bottom row: {c}");
    }
}

#[test]
fn minus_z_is_the_image_centre_and_plus_z_the_seam() {
    let m = quad_map();
    // u = 0.5 → column 1 of two.
    let c = m.radiance(-Vec3A::Z + Vec3A::Y * 0.1);
    assert_eq!(c, Vec3A::new(0.0, 1.0, 0.0));
    // u wraps to 0 → column 0.
    let c = m.radiance(Vec3A::Z + Vec3A::Y * 0.1);
    assert_eq!(c, Vec3A::new(1.0, 0.0, 0.0));
    // +X is at u = 0.75 (column 1), -X at u = 0.25 (column 0).
    assert_eq!(
        m.radiance(Vec3A::new(1.0, 0.1, 0.0)),
        Vec3A::new(0.0, 1.0, 0.0)
    );
    assert_eq!(
        m.radiance(Vec3A::new(-1.0, 0.1, 0.0)),
        Vec3A::new(1.0, 0.0, 0.0)
    );
}

#[test]
fn radiance_lookup_tolerates_unnormalized_directions() {
    let m = quad_map();
    let a = m.radiance(Vec3A::new(0.0, 5.0, -5.0));
    let b = m.radiance(Vec3A::new(0.0, 1.0, -1.0).normalize());
    assert_eq!(a, b);
}

#[test]
fn a_uniform_map_samples_every_direction_with_a_finite_pdf() {
    let m = EnvironmentMap::new(8, 4, vec![Vec3A::splat(2.0); 32]).unwrap();
    let mut rng = Lcg(2);
    for _ in 0..500 {
        let (d, r, pdf) = m.sample(rng.next(), rng.next()).unwrap();
        assert!(approx(d.length(), 1.0, 1e-4), "{d}");
        assert_eq!(r, Vec3A::splat(2.0));
        assert!(pdf > 0.0 && pdf.is_finite());
        // The reported density is what `pdf` recomputes for that direction.
        // Within a texel of the poles the round trip through `acos` can
        // land exactly on sin θ = 0, where the lat-long density is
        // undefined; those directions are excluded.
        if d.y.abs() < 0.999 {
            let again = m.pdf(d);
            assert!(approx(again, pdf, 0.03 * pdf), "{again} vs {pdf}");
        }
    }
}

#[test]
fn sampled_radiance_is_the_lookup_at_the_sampled_direction() {
    let mut rng = Lcg(3);
    let px: Vec<Vec3A> = (0..64)
        .map(|_| Vec3A::new(rng.next(), rng.next(), rng.next()) + 0.05)
        .collect();
    let m = EnvironmentMap::new(8, 8, px).unwrap();
    for _ in 0..500 {
        let (d, r, _) = m.sample(rng.next(), rng.next()).unwrap();
        assert_eq!(r, m.radiance(d));
    }
}

#[test]
fn a_black_map_cannot_be_sampled_and_has_zero_pdf() {
    let m = EnvironmentMap::new(4, 2, vec![Vec3A::ZERO; 8]).unwrap();
    assert!(m.sample(0.3, 0.3).is_none());
    assert_eq!(m.pdf(Vec3A::X), 0.0);
    assert_eq!(m.radiance(Vec3A::X), Vec3A::ZERO);
}

#[test]
fn negative_pixels_do_not_break_the_distribution() {
    let m = EnvironmentMap::new(2, 1, vec![Vec3A::splat(-1.0), Vec3A::splat(1.0)]).unwrap();
    let (d, r, pdf) = m.sample(0.4, 0.4).unwrap();
    assert!(d.is_finite() && pdf.is_finite() && pdf > 0.0);
    // Negative luminance is treated as no weight: only the bright texel
    // is ever chosen.
    assert_eq!(r, Vec3A::splat(1.0));
}

#[test]
fn the_pole_direction_has_zero_solid_angle_density() {
    let m = EnvironmentMap::new(4, 4, vec![Vec3A::ONE; 16]).unwrap();
    assert_eq!(m.pdf(Vec3A::Y), 0.0);
    assert_eq!(m.pdf(-Vec3A::Y), 0.0);
    assert!(m.pdf(Vec3A::X) > 0.0);
}

#[test]
fn importance_sampling_finds_a_single_bright_texel() {
    let (w, h) = (32, 16);
    let mut px = vec![Vec3A::splat(0.02); w * h];
    px[7 * w + 20] = Vec3A::splat(2000.0);
    let m = EnvironmentMap::new(w, h, px).unwrap();
    let mut rng = Lcg(4);
    let mut bright = 0;
    for _ in 0..4000 {
        let (_, r, _) = m.sample(rng.next(), rng.next()).unwrap();
        if r.x > 1.0 {
            bright += 1;
        }
    }
    let frac = bright as f32 / 4000.0;
    assert!(frac > 0.95, "only {frac} of samples hit the bright texel");
}

#[test]
fn bright_texels_carry_proportionally_higher_pdf() {
    let (w, h) = (8, 4);
    let mut px = vec![Vec3A::splat(1.0); w * h];
    // Same row (same sin θ), one texel 10× brighter → 10× the density.
    px[2 * w + 1] = Vec3A::splat(10.0);
    let m = EnvironmentMap::new(w, h, px).unwrap();
    // Directions at the centres of texels (1, 2) and (5, 2).
    let dir_at = |x: usize, y: usize| {
        let u = (x as f32 + 0.5) / w as f32;
        let v = (y as f32 + 0.5) / h as f32;
        let theta = v * std::f32::consts::PI;
        let phi = (u - 0.5) * std::f32::consts::TAU;
        Vec3A::new(
            theta.sin() * phi.sin(),
            theta.cos(),
            -theta.sin() * phi.cos(),
        )
    };
    let bright = m.pdf(dir_at(1, 2));
    let dim = m.pdf(dir_at(5, 2));
    assert!(approx(bright / dim, 10.0, 0.05), "{}", bright / dim);
}

#[test]
fn the_solid_angle_pdf_integrates_to_one() {
    let mut rng = Lcg(5);
    let px: Vec<Vec3A> = (0..16 * 8)
        .map(|_| Vec3A::splat(rng.next() + 0.1))
        .collect();
    let m = EnvironmentMap::new(16, 8, px).unwrap();
    let n = 400_000;
    let mut sum = 0.0f64;
    for _ in 0..n {
        sum += m.pdf(rng.dir()) as f64;
    }
    let integral = sum / n as f64 * 4.0 * std::f64::consts::PI;
    assert!((integral - 1.0).abs() < 0.03, "∫pdf dω = {integral}");
}

#[test]
fn sampling_is_deterministic_in_its_inputs() {
    let m = quad_map();
    let a = m.sample(0.123, 0.456).unwrap();
    let b = m.sample(0.123, 0.456).unwrap();
    assert_eq!(a.0, b.0);
    assert_eq!(a.1, b.1);
    assert_eq!(a.2, b.2);
}

#[test]
fn a_one_texel_map_is_a_uniform_sky() {
    let m = EnvironmentMap::new(1, 1, vec![Vec3A::new(0.2, 0.4, 0.6)]).unwrap();
    let mut rng = Lcg(6);
    for _ in 0..100 {
        let d = rng.dir();
        assert_eq!(m.radiance(d), Vec3A::new(0.2, 0.4, 0.6));
    }
    // The density over the sphere is still uniform in (u, v), so it
    // varies as 1/sin θ over solid angle: larger near the poles.
    let equator = m.pdf(Vec3A::X);
    let high = m.pdf(Vec3A::new(0.1, 1.0, 0.0).normalize());
    assert!(high > equator);
}
