// Constants

use glam::Vec3A;
use std::f32::consts::PI;
// Utility functions

pub fn degrees_to_radians(degrees: f32) -> f32 {
    degrees * PI / 180.0
}

pub fn random() -> f32 {
    // Return a random real in [0.0, 1.0)
    rand::random()
}

pub fn random_range(min: f32, max: f32) -> f32 {
    // Return a random real in [min, max)
    min + (max - min) * random()
}

#[allow(dead_code)]
pub fn clamp(x: f32, min: f32, max: f32) -> f32 {
    if x < min {
        return min;
    }
    if x > max {
        return max;
    }
    x
}

pub fn balance_heuristic(pdf_a: f32, pdf_b: f32) -> f32 {
    let pdf_a2 = pdf_a * pdf_a;
    let pdf_b2 = pdf_b * pdf_b;
    pdf_a2 / (pdf_a2 + pdf_b2 + 1e-6)
}

pub fn random2() -> (f32, f32) {
    (random(), random())
}

pub trait Lerp {
    fn lerp(self, b: Self, t: Self) -> Self;
}

impl Lerp for f32 {
    fn lerp(self, b: f32, t: f32) -> f32 {
        self * (1.0 - t) + b * t
    }
}

use rand::Rng;

// Function to generate a random Vec3A within a unit cube
pub fn random_vec3_unit_cube(rng: &mut impl Rng) -> Vec3A {
    Vec3A::new(
        rng.random_range(-1.0..1.0),
        rng.random_range(-1.0..1.0),
        rng.random_range(-1.0..1.0),
    )
}

// Function to generate a random Vec3A within a unit sphere
pub fn random_vec3_unit_sphere(rng: &mut impl Rng) -> Vec3A {
    loop {
        let v = random_vec3_unit_cube(rng);
        if v.length_squared() < 1.0 {
            return v;
        }
    }
}

pub fn random_in_unit_disk() -> Vec3A {
    loop {
        let p = Vec3A::new(random_range(-1.0, 1.0), random_range(-1.0, 1.0), 0.0);
        if p.length_squared() >= 1.0 {
            continue;
        }
        return p;
    }
}
pub fn random3() -> Vec3A {
    Vec3A::new(random(), random(), random())
}

pub fn random_range3(min: f32, max: f32) -> Vec3A {
    Vec3A::new(
        random_range(min, max),
        random_range(min, max),
        random_range(min, max),
    )
}

pub fn random_cosine_direction() -> Vec3A {
    let r1 = random();
    let r2 = random();
    let z = f32::sqrt(1.0 - r2);

    let phi = 2.0 * std::f32::consts::PI * r1;
    let x = f32::cos(phi) * f32::sqrt(r2);
    let y = f32::sin(phi) * f32::sqrt(r2);

    Vec3A::new(x, y, z)
}

pub fn align_to_normal(local: Vec3A, normal: Vec3A) -> Vec3A {
    // Assume Z-up in local, rotate to match `normal`
    let up = if normal.z.abs() < 0.999 {
        Vec3A::Z
    } else {
        Vec3A::X
    };

    let tangent = normal.cross(up).normalize(); // Swapped cross order and normalized
    let bitangent = normal.cross(tangent);

    local.x * tangent + local.y * bitangent + local.z * normal
}
