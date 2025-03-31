// Constants

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
