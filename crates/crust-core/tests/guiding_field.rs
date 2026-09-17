//! The path-guiding `GuidingField` facade: configuration, the untrained
//! state, training on synthetic samples and the sample/pdf consistency the
//! integrator's one-sample MIS depends on.

use crust_core::{AABB, GuidingConfig, GuidingField, SampleData, Vec3A};
use openqmc::pcg::Rng;

fn unit_bounds() -> AABB {
    AABB::new(Vec3A::ZERO, Vec3A::ONE)
}

fn samples_toward(dir: Vec3A, n: usize, radiance: f32) -> Vec<SampleData> {
    (0..n)
        .map(|i| SampleData {
            pos: Vec3A::splat(0.1 + 0.8 * (i % 17) as f32 / 17.0),
            dir,
            radiance,
        })
        .collect()
}

#[test]
fn default_config_follows_the_documented_values() {
    let c = GuidingConfig::default();
    assert_eq!(c.train_iterations, 4);
    assert_eq!(c.guide_prob, 0.5);
    assert!(c.spatial_c > 0.0);
    assert!(c.dtree_rho > 0.0 && c.dtree_rho < 1.0);
    assert!(c.dtree_max_depth > 0);
    assert!(c.spatial_max_depth > 0);
}

#[test]
fn field_reports_its_config() {
    let cfg = GuidingConfig { train_iterations: 7, guide_prob: 0.3, ..GuidingConfig::default() };
    let f = GuidingField::new(unit_bounds(), cfg);
    assert_eq!(f.config().train_iterations, 7);
    assert_eq!(f.config().guide_prob, 0.3);
}

#[test]
fn an_untrained_field_has_nothing_to_sample() {
    let f = GuidingField::new(unit_bounds(), GuidingConfig::default());
    let mut rng = Rng::new(1);
    for _ in 0..20 {
        let p = Vec3A::new(rng.next_f32(), rng.next_f32(), rng.next_f32());
        assert!(!f.trained_at(p));
        assert!(f.sample(p, rng.next_2d()).is_none());
        assert_eq!(f.pdf(p, Vec3A::Z), 0.0);
    }
    // Points outside the bounds are handled, not panicked on.
    assert!(!f.trained_at(Vec3A::splat(5.0)));
    assert!(f.sample(Vec3A::splat(-5.0), [0.5, 0.5]).is_none());
}

#[test]
fn training_makes_the_field_sampleable() {
    let mut f = GuidingField::new(unit_bounds(), GuidingConfig::default());
    f.update(&samples_toward(Vec3A::X, 2000, 1.0), 1);
    let mut rng = Rng::new(2);
    let p = Vec3A::splat(0.5);
    assert!(f.trained_at(p));
    for _ in 0..50 {
        let (d, pdf) = f.sample(p, rng.next_2d()).expect("trained");
        assert!((d.length() - 1.0).abs() < 1e-3, "{d}");
        assert!(pdf > 0.0 && pdf.is_finite());
    }
}

#[test]
fn a_trained_field_concentrates_on_the_taught_direction() {
    let mut f = GuidingField::new(unit_bounds(), GuidingConfig::default());
    let target = Vec3A::new(0.0, 0.0, 1.0);
    // Refinement creates children that inherit their parent's flux
    // evenly, so a freshly split level samples uniformly until the next
    // pass records into it: several passes are needed before the
    // distribution is sharp.
    let mut rng = Rng::new(3);
    let p = Vec3A::splat(0.5);
    let aligned_after = |f: &GuidingField, rng: &mut Rng| {
        (0..500)
            .filter(|_| f.sample(p, rng.next_2d()).unwrap().0.dot(target) > 0.5)
            .count()
    };
    f.update(&samples_toward(target, 4000, 1.0), 1);
    let first = aligned_after(&f, &mut rng);
    for pass in 2..=4 {
        f.update(&samples_toward(target, 4000, 1.0), pass);
    }
    let aligned = aligned_after(&f, &mut rng);
    assert!(aligned > 350, "only {aligned} of 500 samples within 60° of the taught direction");
    assert!(aligned >= first, "more training must not blur the peak: {first} -> {aligned}");
    // And the density is higher there than on the opposite side.
    assert!(f.pdf(p, target) > f.pdf(p, -target));
}

#[test]
fn sample_and_pdf_agree() {
    let mut f = GuidingField::new(unit_bounds(), GuidingConfig::default());
    let mut rng = Rng::new(4);
    let mut data = Vec::new();
    for _ in 0..3000 {
        let d = Vec3A::new(rng.next_f32() - 0.5, rng.next_f32() - 0.5, rng.next_f32()).normalize();
        data.push(SampleData { pos: Vec3A::splat(0.5), dir: d, radiance: 0.5 + rng.next_f32() });
    }
    f.update(&data, 1);
    let p = Vec3A::splat(0.5);
    for _ in 0..200 {
        let (d, pdf) = f.sample(p, rng.next_2d()).unwrap();
        let again = f.pdf(p, d);
        assert!((again - pdf).abs() < 1e-3 * pdf.max(1.0), "{again} vs {pdf}");
    }
}

#[test]
fn the_guided_pdf_integrates_to_one() {
    let mut f = GuidingField::new(unit_bounds(), GuidingConfig::default());
    let mut data = Vec::new();
    let mut rng = Rng::new(5);
    for _ in 0..5000 {
        let z = 1.0 - 2.0 * rng.next_f32();
        let r = (1.0 - z * z).max(0.0).sqrt();
        let phi = std::f32::consts::TAU * rng.next_f32();
        let d = Vec3A::new(r * phi.cos(), r * phi.sin(), z);
        // Weight the upper hemisphere more.
        data.push(SampleData { pos: Vec3A::splat(0.5), dir: d, radiance: if z > 0.0 { 3.0 } else { 1.0 } });
    }
    f.update(&data, 1);
    let p = Vec3A::splat(0.5);
    let n = 200_000;
    let mut sum = 0.0f64;
    for _ in 0..n {
        let z = 1.0 - 2.0 * rng.next_f32();
        let r = (1.0 - z * z).max(0.0).sqrt();
        let phi = std::f32::consts::TAU * rng.next_f32();
        sum += f.pdf(p, Vec3A::new(r * phi.cos(), r * phi.sin(), z)) as f64;
    }
    let integral = sum / n as f64 * 4.0 * std::f64::consts::PI;
    assert!((integral - 1.0).abs() < 0.03, "∫pdf = {integral}");
}

#[test]
fn training_is_spatially_local() {
    // Teach +Z in one corner and -Z in the opposite corner; with enough
    // samples the spatial tree splits and the two corners disagree.
    let cfg = GuidingConfig { spatial_c: 10.0, ..GuidingConfig::default() };
    let mut f = GuidingField::new(unit_bounds(), cfg);
    let mut data = Vec::new();
    let mut rng = Rng::new(6);
    for _ in 0..20_000 {
        let lo = Vec3A::new(rng.next_f32(), rng.next_f32(), rng.next_f32()) * 0.4;
        data.push(SampleData { pos: lo, dir: Vec3A::Z, radiance: 1.0 });
        data.push(SampleData { pos: Vec3A::splat(0.6) + lo, dir: -Vec3A::Z, radiance: 1.0 });
    }
    f.update(&data, 1);
    f.update(&data, 2);
    f.update(&data, 3);
    let a = Vec3A::splat(0.2);
    let b = Vec3A::splat(0.8);
    assert!(f.pdf(a, Vec3A::Z) > f.pdf(a, -Vec3A::Z), "corner a should favour +Z");
    assert!(f.pdf(b, -Vec3A::Z) > f.pdf(b, Vec3A::Z), "corner b should favour -Z");
}

#[test]
fn zero_radiance_samples_do_not_train() {
    let mut f = GuidingField::new(unit_bounds(), GuidingConfig::default());
    f.update(&samples_toward(Vec3A::Y, 500, 0.0), 1);
    assert!(!f.trained_at(Vec3A::splat(0.5)));
    assert!(f.sample(Vec3A::splat(0.5), [0.3, 0.3]).is_none());
}

#[test]
fn field_is_cloneable_and_debuggable() {
    let mut f = GuidingField::new(unit_bounds(), GuidingConfig::default());
    f.update(&samples_toward(Vec3A::X, 100, 1.0), 1);
    let c = f.clone();
    assert_eq!(c.trained_at(Vec3A::splat(0.5)), f.trained_at(Vec3A::splat(0.5)));
    assert_eq!(c.pdf(Vec3A::splat(0.5), Vec3A::X), f.pdf(Vec3A::splat(0.5), Vec3A::X));
    let _ = format!("{f:?}");
    let s = SampleData { pos: Vec3A::ZERO, dir: Vec3A::Z, radiance: 1.0 };
    let t = s;
    assert_eq!(t.radiance, s.radiance);
}

#[test]
fn degenerate_bounds_are_padded() {
    // A flat scene (zero extent on one axis) must still make a usable field.
    let mut f = GuidingField::new(AABB::new(Vec3A::ZERO, Vec3A::new(1.0, 0.0, 1.0)), GuidingConfig::default());
    let data: Vec<SampleData> = (0..500)
        .map(|i| SampleData { pos: Vec3A::new((i % 10) as f32 / 10.0, 0.0, 0.5), dir: Vec3A::Y, radiance: 1.0 })
        .collect();
    f.update(&data, 1);
    assert!(f.trained_at(Vec3A::new(0.5, 0.0, 0.5)));
    assert!(f.sample(Vec3A::new(0.5, 0.0, 0.5), [0.2, 0.7]).is_some());
}
