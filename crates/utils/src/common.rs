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

// Function to generate a uniformly distributed random Vec3A on the unit sphere
// (a true random unit vector). Used for Lambertian diffuse scattering.
pub fn random_unit_vector() -> Vec3A {
    let mut rng = rand::rng();
    loop {
        let v = random_vec3_unit_cube(&mut rng);
        let len_sq = v.length_squared();
        // Reject points outside the unit sphere and the (near) origin to avoid
        // dividing by zero when normalizing.
        if (1e-12..1.0).contains(&len_sq) {
            return v / len_sq.sqrt();
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
    cosine_hemisphere([random(), random()])
}

/// Cosine-weighted hemisphere sample from an explicit 2D uniform pair.
/// Returns a direction in the local frame with the surface normal on +Z.
pub fn cosine_hemisphere(uv: [f32; 2]) -> Vec3A {
    let (u, v) = (uv[0], uv[1]);
    let z = f32::sqrt(1.0 - v);
    let phi = 2.0 * std::f32::consts::PI * u;
    let x = f32::cos(phi) * f32::sqrt(v);
    let y = f32::sin(phi) * f32::sqrt(v);
    Vec3A::new(x, y, z)
}

/// Uniform sphere-surface sample from an explicit 2D uniform pair. Analytic
/// (no rejection), so behaves identically each time for a given `uv`.
pub fn uniform_sphere(uv: [f32; 2]) -> Vec3A {
    let (u, v) = (uv[0], uv[1]);
    let z = 1.0 - 2.0 * u;
    let r = (1.0 - z * z).max(0.0).sqrt();
    let phi = 2.0 * std::f32::consts::PI * v;
    Vec3A::new(r * phi.cos(), r * phi.sin(), z)
}

/// Uniform sample of the closed unit ball (volume, not surface) from an
/// explicit 3D uniform triple. Radius warp is `u^(1/3)` so the result is
/// volumetrically uniform.
pub fn uniform_ball(uvw: [f32; 3]) -> Vec3A {
    let dir = uniform_sphere([uvw[0], uvw[1]]);
    let r = uvw[2].max(0.0).cbrt();
    dir * r
}

/// Concentric-disk warp (Shirley 1997) from an explicit 2D uniform pair.
/// Returns an xy-point in the unit disk, `z = 0`.
pub fn concentric_disk(uv: [f32; 2]) -> Vec3A {
    // Remap to [-1, 1]^2, handle the origin explicitly to avoid divide-by-0.
    let sx = 2.0 * uv[0] - 1.0;
    let sy = 2.0 * uv[1] - 1.0;
    if sx == 0.0 && sy == 0.0 {
        return Vec3A::ZERO;
    }
    let (r, theta) = if sx.abs() > sy.abs() {
        (sx, std::f32::consts::FRAC_PI_4 * (sy / sx))
    } else {
        (
            sy,
            std::f32::consts::FRAC_PI_2 - std::f32::consts::FRAC_PI_4 * (sx / sy),
        )
    };
    Vec3A::new(r * theta.cos(), r * theta.sin(), 0.0)
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
