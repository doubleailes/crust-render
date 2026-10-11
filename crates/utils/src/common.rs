use glam::Vec3A;
use std::f32::consts::PI;

/// `e^v` per component — Beer–Lambert transmittance `exp3(-σ·t)` and its
/// kin, wherever a colour is exponentiated.
#[inline]
pub fn exp3(v: Vec3A) -> Vec3A {
    Vec3A::new(v.x.exp(), v.y.exp(), v.z.exp())
}

pub fn degrees_to_radians(degrees: f32) -> f32 {
    degrees * PI / 180.0
}

/// Rec. 709 luminance of a linear RGB value: [`Luma::REC709`].
#[inline]
pub fn luminance(c: Vec3A) -> f32 {
    Luma::REC709.of(c)
}

/// The luminance weights of a working colour space — the `Y` row of its
/// RGB → XYZ matrix — and so the scalar the renderer uses wherever a colour
/// has to become one weight: light power, environment-map
/// importance, adaptive-sampling variance, lobe selection. Carried by value
/// to each of those rather than read from a global, since two scenes in one
/// process may render in different spaces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Luma(pub Vec3A);

impl Luma {
    /// Linear Rec.709's, as its standard rounds them.
    pub const REC709: Luma = Luma(Vec3A::new(0.2126, 0.7152, 0.0722));

    /// The luminance of `c`. The same three products and two sums, in the same
    /// order, as the Rec.709 constant expression this replaced, so a
    /// `lin_rec709` render is bit-identical.
    #[inline]
    pub fn of(self, c: Vec3A) -> f32 {
        self.0.x * c.x + self.0.y * c.y + self.0.z * c.z
    }
}

impl Default for Luma {
    fn default() -> Luma {
        Luma::REC709
    }
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
