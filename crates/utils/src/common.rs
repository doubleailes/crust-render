use glam::Vec3A;
use std::f32::consts::PI;

pub fn degrees_to_radians(degrees: f32) -> f32 {
    degrees * PI / 180.0
}

/// Rec. 709 luminance of a linear RGB value — the scalar the renderer uses
/// wherever a colour has to become one weight: guiding flux, light power,
/// environment-map importance, adaptive-sampling variance, lobe selection.
#[inline]
pub fn luminance(c: Vec3A) -> f32 {
    0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z
}

/// `e^v` per channel — Beer–Lambert transmittance (`exp3(-σₜ·t)`) and the
/// medium corrections built from it. Three scalar `exp`s, in channel order,
/// so every caller rounds exactly as the inline form it replaced.
#[inline]
pub fn exp3(v: Vec3A) -> Vec3A {
    Vec3A::new(v.x.exp(), v.y.exp(), v.z.exp())
}

/// Veach's balance heuristic: `w_a = pdf_a / (pdf_a + pdf_b)`.
///
/// Historical note: before the sampling strategies were made configurable
/// this function computed the β=2 power heuristic under the wrong name —
/// that formula now lives in [`power_heuristic`], which remains the
/// integrator's default so existing renders are unchanged.
pub fn balance_heuristic(pdf_a: f32, pdf_b: f32) -> f32 {
    pdf_a / (pdf_a + pdf_b + 1e-6)
}

/// Veach's power heuristic with β = 2: `w_a = pdf_a² / (pdf_a² + pdf_b²)`.
/// Sharpens the balance heuristic toward whichever strategy is denser —
/// Veach found β = 2 a good default for glossy surfaces.
pub fn power_heuristic(pdf_a: f32, pdf_b: f32) -> f32 {
    let pdf_a2 = pdf_a * pdf_a;
    let pdf_b2 = pdf_b * pdf_b;
    pdf_a2 / (pdf_a2 + pdf_b2 + 1e-6)
}

pub trait Lerp {
    fn lerp(self, b: Self, t: Self) -> Self;
}

impl Lerp for f32 {
    fn lerp(self, b: f32, t: f32) -> f32 {
        self * (1.0 - t) + b * t
    }
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
