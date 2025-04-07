// Constants

use glam::Vec3;
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

// Function to generate a random Vec3 within a unit cube
pub fn random_vec3_unit_cube(rng: &mut impl Rng) -> Vec3 {
    Vec3::new(
        rng.random_range(-1.0..1.0),
        rng.random_range(-1.0..1.0),
        rng.random_range(-1.0..1.0),
    )
}

// Function to generate a random Vec3 within a unit sphere
pub fn random_vec3_unit_sphere(rng: &mut impl Rng) -> Vec3 {
    loop {
        let v = random_vec3_unit_cube(rng);
        if v.length_squared() < 1.0 {
            return v;
        }
    }
}

// Function to generate a random Vec3 on the surface of a unit sphere
pub fn random_vec3_unit_sphere_surface(rng: &mut impl Rng) -> Vec3 {
    let v = random_vec3_unit_sphere(rng).normalize();
    v
}

// Function to generate a random Vec3 in a hemisphere (oriented by normal)
pub fn random_vec3_in_hemisphere(rng: &mut impl Rng, normal: Vec3) -> Vec3 {
    let v = random_vec3_unit_sphere_surface(rng);
    if v.dot(normal) > 0.0 { v } else { -v }
}

pub fn random_in_unit_disk() -> Vec3 {
    loop {
        let p = Vec3::new(random_range(-1.0, 1.0), random_range(-1.0, 1.0), 0.0);
        if p.length_squared() >= 1.0 {
            continue;
        }
        return p;
    }
}
