//! The formulas UsdLux *specifies*, kept apart from the lights that use them.
//!
//! Everything here is a function of authored attributes and nothing else — no
//! geometry, no sampling — because that is the part of lighting a reference
//! comparison checks first, and it is easiest to check in isolation. The
//! normative source is the `LightAPI` / `ShapingAPI` documentation in
//! OpenUSD's `usdLux/schema.usda`; where the spec text and OpenUSD's own
//! reference implementation (hdEmbree's `lightSamplers.cpp`, the delegate that
//! grew the UsdLux reference) disagree, this follows the implementation and
//! says so, since the implementation is what a conformance image is rendered
//! with.
//!
//! - [`blackbody_rgb`] — `inputs:colorTemperature`.
//! - [`distant_size_factor`] / [`distant_illuminance`] — `inputs:normalize`
//!   for a `DistantLight`, and what its nits mean in lux (area lights divide
//!   by their world-space surface area, which their shapes own).
//! - [`Shaping`] — `ShapingAPI`: focus, focus tint, cone, IES.
//! - [`LightTexture`] — `RectLight`'s `inputs:texture:file`, decoded.
//! - [`IesProfile`] — the IES photometric web the shaping samples. Parsing the
//!   LM-63 text is a file-format concern and lives in `crust-assets`; this is
//!   the decoded table and its evaluation.

use glam::{Mat3A, Vec3A};
use std::f32::consts::PI;
use std::sync::Arc;

// -----------------------------------------------------------------------
// Colour temperature
// -----------------------------------------------------------------------

/// The linear Rec.709 colour of a blackbody at `kelvin`, normalised to unit
/// Rec.709 luminance — what `inputs:colorTemperature` multiplies into the
/// emission when `inputs:enableColorTemperature` is on.
///
/// This is hdEmbree's `_BlackbodyTemperatureAsRgb`, step for step: Krystek's
/// 1985 rational approximation of the Planckian locus in CIE 1960 `(u, v)`
/// (clamped to 1000–15000 K, as nanocolor's `NcKelvinToYxy` does), lifted to
/// `xyY` at `Y = 1`, then to linear Rec.709 through the primaries' own
/// normalised primary matrix. So 6500 K is *not* exactly white — D65 sits a
/// little off the Planckian locus — which the spec text ("normalized such
/// that the default value of 6500 will always result in white") glosses over,
/// and which `usdLux/blackbody.cpp`'s older table-driven version admits in a
/// comment. It lands at (1.044, 0.983, 1.036) — within 5% of white.
///
/// Not clamped at zero: below ~1900 K the locus leaves the Rec.709 gamut and
/// blue goes slightly negative, exactly as in the reference. The importer
/// clamps the final emission instead.
pub fn blackbody_rgb(kelvin: f32) -> Vec3A {
    let t = kelvin.clamp(1000.0, 15000.0) as f64;
    // Krystek's coefficients, as nanocolor carries them.
    let u = (0.860117757 + 1.54118254e-4 * t + 1.2864121e-7 * t * t)
        / (1.0 + 8.42420235e-4 * t + 7.08145163e-7 * t * t);
    let v = (0.317398726 + 4.22806245e-5 * t + 4.20481691e-8 * t * t)
        / (1.0 - 2.89741816e-5 * t + 1.61456053e-7 * t * t);
    // CIE 1960 v → CIE 1976 v', then u'v' → xy.
    let v_prime = 1.5 * v;
    let d = 6.0 * u - 16.0 * v_prime + 12.0;
    let (x, y) = (9.0 * u / d, 4.0 * v_prime / d);
    let xyz = [x / y, 1.0, (1.0 - x - y) / y];
    let rgb = xyz_to_rec709(xyz);
    let luma = REC709_LUMA[0] * rgb[0] + REC709_LUMA[1] * rgb[1] + REC709_LUMA[2] * rgb[2];
    Vec3A::new(
        (rgb[0] / luma) as f32,
        (rgb[1] / luma) as f32,
        (rgb[2] / luma) as f32,
    )
}

/// Rec.709 luminance weights (the `Y` row of the RGB → XYZ matrix).
const REC709_LUMA: [f64; 3] = [0.2126390059, 0.7151686788, 0.0721923154];

/// XYZ → linear Rec.709, derived from the primaries and the D65 white point
/// the way nanocolor derives every colour space's matrix, rather than typed
/// in to four places from a textbook.
fn xyz_to_rec709(xyz: [f64; 3]) -> [f64; 3] {
    let m = rec709_to_xyz();
    let inv = invert3(m);
    [0, 1, 2].map(|r| inv[r][0] * xyz[0] + inv[r][1] * xyz[1] + inv[r][2] * xyz[2])
}

/// The normalised primary matrix (RGB → XYZ) of linear Rec.709 / D65.
fn rec709_to_xyz() -> [[f64; 3]; 3] {
    let prim = [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06)];
    let white = (0.3127, 0.3290);
    let col = |(x, y): (f64, f64)| [x / y, 1.0, (1.0 - x - y) / y];
    let p = [col(prim[0]), col(prim[1]), col(prim[2])];
    // Primaries as columns.
    let pm = [
        [p[0][0], p[1][0], p[2][0]],
        [p[0][1], p[1][1], p[2][1]],
        [p[0][2], p[1][2], p[2][2]],
    ];
    let w = col(white);
    let pi = invert3(pm);
    let s = [0, 1, 2].map(|r| pi[r][0] * w[0] + pi[r][1] * w[1] + pi[r][2] * w[2]);
    [0, 1, 2].map(|r| [pm[r][0] * s[0], pm[r][1] * s[1], pm[r][2] * s[2]])
}

fn invert3(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let c =
        |r0: usize, c0: usize, r1: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    let det = m[0][0] * c(1, 1, 2, 2) - m[0][1] * c(1, 0, 2, 2) + m[0][2] * c(1, 0, 2, 1);
    [
        [
            c(1, 1, 2, 2) / det,
            -c(0, 1, 2, 2) / det,
            c(0, 1, 1, 2) / det,
        ],
        [
            -c(1, 0, 2, 2) / det,
            c(0, 0, 2, 2) / det,
            -c(0, 0, 1, 2) / det,
        ],
        [
            c(1, 0, 2, 1) / det,
            -c(0, 0, 2, 1) / det,
            c(0, 0, 1, 1) / det,
        ],
    ]
}

// -----------------------------------------------------------------------
// Normalize
// -----------------------------------------------------------------------

/// `sizeFactor` for a `DistantLight` under `inputs:normalize`, from its
/// authored `inputs:angle` (the angular *diameter*, in degrees):
///
/// - `θmax = clamp(angle/2, 0, π)`,
/// - `θmax = 0` → 1,
/// - `0 < θmax ≤ π/2` → `π·sin²θmax`,
/// - `π/2 < θmax ≤ π` → `(2 − sin²θmax)·π`.
///
/// `π·sin²θmax` is the cosine-weighted solid angle of the source cone, so a
/// normalised distant light delivers exactly `intensity` lux to a surface
/// facing it — the property the spec picks the formula for.
pub fn distant_size_factor(angle_deg: f32) -> f32 {
    let theta = (0.5 * (angle_deg as f64).to_radians()).clamp(0.0, std::f64::consts::PI);
    if theta == 0.0 {
        return 1.0;
    }
    // f64: at the sun's 0.265° the f32 cosine is within a few ulps of 1, and
    // anything built from it loses up to half a percent of sin²θ.
    let s2 = theta.sin().powi(2);
    let factor = if theta <= 0.5 * std::f64::consts::PI {
        std::f64::consts::PI * s2
    } else {
        (2.0 - s2) * std::f64::consts::PI
    };
    factor as f32
}

/// The illuminance (lux) a `DistantLight` of luminance `luminance` (nits)
/// and angular diameter `angle_deg` delivers to a surface facing it:
/// `L · π·sin²θmax`, capped at the facing hemisphere (`L · π`). In f64 for
/// the reason [`distant_size_factor`] is. A zero angle is a delta light and
/// has no such conversion; the caller treats its intensity as illuminance.
pub fn distant_illuminance(luminance: f32, angle_deg: f32) -> f32 {
    let theta = (0.5 * (angle_deg as f64).to_radians()).clamp(0.0, 0.5 * std::f64::consts::PI);
    (luminance as f64 * std::f64::consts::PI * theta.sin().powi(2)) as f32
}

// -----------------------------------------------------------------------
// Shaping
// -----------------------------------------------------------------------

/// `UsdLuxShapingAPI`: how a light's emission varies with the direction it
/// leaves in. A multiplier on radiance, evaluated per emission direction —
/// so it is shared by NEE (`AreaLight::sample_li`) and by a bounce ray that
/// hits the light's surface (`Emissive::emitted_at`), which is what keeps the
/// two MIS strategies seeing the same light.
///
/// The formulas are the spec's, evaluated in the order hdEmbree applies them
/// (focus, cone, IES) — they are all multiplicative, so the order only
/// matters for bit-exactness.
#[derive(Debug, Clone)]
pub struct Shaping {
    /// `inputs:shaping:focus`. Values ≤ 0 disable focus.
    pub focus: f32,
    /// `inputs:shaping:focusTint`.
    pub focus_tint: Vec3A,
    /// `inputs:shaping:cone:angle`, in degrees off the primary axis.
    pub cone_angle_deg: f32,
    /// `inputs:shaping:cone:softness`, clamped to `[0, 1]` as the spec says.
    pub cone_softness: f32,
    /// `inputs:shaping:ies:*`, when a profile was authored and decoded.
    pub ies: Option<IesShaping>,
    /// The light's primary axis in world space (its local −Z), unit length.
    axis: Vec3A,
    /// World → light-local rotation-and-scale, for the IES lookup.
    world_to_light: Mat3A,
}

/// The IES part of [`Shaping`].
#[derive(Debug, Clone)]
pub struct IesShaping {
    pub profile: Arc<IesProfile>,
    /// `inputs:shaping:ies:angleScale`.
    pub angle_scale: f32,
    /// `inputs:shaping:ies:normalize`.
    pub normalize: bool,
}

impl Shaping {
    /// A shaping with neutral defaults for a light whose local frame maps to
    /// world by `light_to_world` (its linear part; translation is irrelevant
    /// to directions). Neutral means *no effect at all*: a 180° cone, not the
    /// schema's 90° fallback — see [`Shaping::SCHEMA_CONE_ANGLE_DEG`].
    pub fn new(light_to_world: Mat3A) -> Self {
        Self {
            focus: 0.0,
            focus_tint: Vec3A::ZERO,
            cone_angle_deg: 180.0,
            cone_softness: 0.0,
            ies: None,
            axis: (light_to_world * -Vec3A::Z).normalize_or(-Vec3A::Z),
            world_to_light: light_to_world.inverse(),
        }
    }

    /// The schema fallback for `inputs:shaping:cone:angle`. It applies only
    /// to a prim that has `ShapingAPI` applied — the attribute does not exist
    /// otherwise, and hdEmbree's own default for the unauthored case is 180°.
    /// Applying 90° to every light would cut the back half off every sphere
    /// and cylinder light in every scene, which is not what anyone authored.
    pub const SCHEMA_CONE_ANGLE_DEG: f32 = 90.0;

    /// Does this shaping change anything at all? A light whose shaping is
    /// neutral skips the per-direction evaluation entirely.
    pub fn is_neutral(&self) -> bool {
        self.focus <= 0.0 && self.cone_angle_deg >= 180.0 && self.ies.is_none()
    }

    /// The light's primary axis in world space.
    pub fn axis(&self) -> Vec3A {
        self.axis
    }

    /// The factor emission leaving along the world-space unit direction
    /// `emission_dir` (away from the light) is multiplied by.
    pub fn factor(&self, emission_dir: Vec3A) -> Vec3A {
        let cos_off = emission_dir.dot(self.axis).clamp(-1.0, 1.0);
        let mut f = Vec3A::ONE;

        // Focus: lerp(|cos|^focus, focusTint, white).
        if self.focus > 0.0 {
            let ff = cos_off.abs().powf(self.focus);
            f *= self.focus_tint * (1.0 - ff) + Vec3A::splat(ff);
        }

        // Cone: 1 − smoothstep(θoff, θsoft, θcutoff), θsoft = lerp(softness, θcutoff, 0).
        let theta_cone = self.cone_angle_deg.to_radians();
        let theta_soft = (1.0 - self.cone_softness.clamp(0.0, 1.0)) * theta_cone;
        f *= 1.0 - smoothstep(cos_off.acos(), theta_soft, theta_cone);

        if let Some(ies) = &self.ies {
            f *= ies.eval(self.world_to_light, emission_dir);
        }
        f
    }
}

impl IesShaping {
    /// The profile's scale on emission leaving along `emission_dir`.
    ///
    /// The polar angle is measured from the light's **−Z**, its emission axis,
    /// so an IES downlight (whose 0° is nadir) aimed with its light points its
    /// beam the way the light points. hdEmbree gets there by evaluating the
    /// profile with the *incident* direction (surface → light), and the
    /// azimuth is taken the same way so the two agree on φ too.
    fn eval(&self, world_to_light: Mat3A, emission_dir: Vec3A) -> f32 {
        let w = (world_to_light * -emission_dir).normalize_or_zero();
        if w == Vec3A::ZERO {
            return 0.0;
        }
        let theta = w.z.clamp(-1.0, 1.0).acos();
        let mut phi = w.y.atan2(w.x);
        if phi < 0.0 {
            phi += 2.0 * PI;
        }
        let norm = if self.normalize {
            self.profile.power()
        } else {
            1.0
        };
        if norm <= 0.0 {
            return 0.0;
        }
        self.profile.eval(theta, phi, self.angle_scale) / norm
    }
}

/// The spec's `smoothStep`, with hdEmbree's rule for a zero-width range: 0 at
/// or below it, 1 above — which is what makes the default `softness = 0` a
/// hard cutoff that keeps `θoff = θcutoff` itself lit.
fn smoothstep(t: f32, lo: f32, hi: f32) -> f32 {
    let length = hi - lo;
    if length == 0.0 {
        return if t <= lo { 0.0 } else { 1.0 };
    }
    let t = ((t - lo) / length).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// -----------------------------------------------------------------------
// Light textures
// -----------------------------------------------------------------------

/// A light's colour map (`RectLight`'s `inputs:texture:file`): linear float
/// RGB, row-major, row 0 at the top of the image. The host decodes it
/// through [`crate::AssetLoader::load_light_texture`]; float rather than
/// the UV-texture path's 8-bit tiles because a light's map is exactly where
/// a value above 1.0 is meaningful.
///
/// It multiplies the light's emission, which `normalize` then divides by the
/// area as usual — the map shapes the light and scales it, as hdEmbree does;
/// the spec says only "a color texture to use on the rectangle".
#[derive(Debug, Clone)]
pub struct LightTexture {
    width: usize,
    height: usize,
    pixels: Vec<Vec3A>,
}

impl LightTexture {
    /// `None` for an empty or mis-sized buffer.
    pub fn new(width: usize, height: usize, pixels: Vec<Vec3A>) -> Option<Self> {
        (width > 0 && height > 0 && pixels.len() == width * height).then_some(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// The texel at image coordinates `(s, t)` in `[0, 1]²`, `t = 0` the top
    /// row. **Nearest**, as hdEmbree's `_SampleLightTexture` is: the lookup is
    /// part of what a conformance image checks, and a light seen directly
    /// shows its filtering. Out-of-range coordinates clamp to the edge.
    pub fn texel(&self, s: f32, t: f32) -> Vec3A {
        let index = |c: f32, n: usize| ((c * n as f32) as isize).clamp(0, n as isize - 1) as usize;
        self.pixels[index(t, self.height) * self.width + index(s, self.width)]
    }
}

/// A [`LightTexture`] laid onto a rectangle light's surface: the map from a
/// world point on the parallelogram `origin + u·edge_u + v·edge_v` to the
/// image. `u` runs along the light's local +X and the image's `s`; `v` along
/// local +Y and *up* the image, so `t = 1 − v` — hdEmbree's orientation,
/// which puts the image's top row at the light's +Y edge.
#[derive(Debug, Clone)]
pub struct RectTexture {
    image: Arc<LightTexture>,
    origin: Vec3A,
    /// Dual basis of the edges: `(p − origin)·dual_u = u`.
    dual_u: Vec3A,
    dual_v: Vec3A,
}

impl RectTexture {
    /// `None` for a degenerate rectangle (parallel or zero edges).
    pub fn new(
        image: Arc<LightTexture>,
        origin: Vec3A,
        edge_u: Vec3A,
        edge_v: Vec3A,
    ) -> Option<Self> {
        let n = edge_u.cross(edge_v);
        let n2 = n.length_squared();
        if n2 <= 0.0 || !n2.is_finite() {
            return None;
        }
        Some(Self {
            image,
            origin,
            dual_u: edge_v.cross(n) / n2,
            dual_v: n.cross(edge_u) / n2,
        })
    }

    /// The map's colour at a world point on the rectangle.
    pub fn at(&self, p: Vec3A) -> Vec3A {
        let d = p - self.origin;
        self.image
            .texel(d.dot(self.dual_u), 1.0 - d.dot(self.dual_v))
    }
}

// -----------------------------------------------------------------------
// IES profiles
// -----------------------------------------------------------------------

/// A decoded IES photometric web, in the canonical layout the evaluator
/// reads: polar ("vertical") angles and azimuthal ("horizontal") angles in
/// **radians**, both ascending, with `intensity[h][v]` in candela. The host's
/// parser has already converted type A/B files and mirrored partial
/// azimuthal ranges out to the full circle (`crust_assets::parse_ies`), so
/// every profile here is type C covering 0–2π.
#[derive(Debug, Clone)]
pub struct IesProfile {
    v_angles: Vec<f32>,
    h_angles: Vec<f32>,
    intensity: Vec<Vec<f32>>,
    power: f32,
}

impl IesProfile {
    /// Wraps a decoded table. `None` for one the evaluator could not read:
    /// fewer than two angles on either axis, a ragged table, angles that do
    /// not ascend, or any non-finite angle or intensity — which would
    /// otherwise reach the power integral and every radiance the light
    /// emits.
    pub fn new(v_angles: Vec<f32>, h_angles: Vec<f32>, intensity: Vec<Vec<f32>>) -> Option<Self> {
        let ascending = |a: &[f32]| a.windows(2).all(|w| w[0] <= w[1]);
        if v_angles.len() < 2
            || h_angles.len() < 2
            || intensity.len() != h_angles.len()
            || intensity.iter().any(|row| row.len() != v_angles.len())
            || !ascending(&v_angles)
            || !ascending(&h_angles)
            || !v_angles.iter().chain(&h_angles).all(|a| a.is_finite())
            || !intensity.iter().flatten().all(|i| i.is_finite())
        {
            return None;
        }
        let power = profile_power(&v_angles, &h_angles, &intensity);
        Some(Self {
            v_angles,
            h_angles,
            intensity,
            power,
        })
    }

    /// The profile's mean intensity over the (hemi)sphere it covers — what
    /// `inputs:shaping:ies:normalize` divides by. See [`profile_power`].
    pub fn power(&self) -> f32 {
        self.power
    }

    /// Intensity toward polar angle `theta` and azimuth `phi` (radians),
    /// after the spec's `angleScale` remap. Bilinear in the table, as the
    /// reference is (its comment notes cubic would be better; matching the
    /// reference matters more here). Directions the table does not cover
    /// evaluate to zero.
    pub fn eval(&self, theta: f32, phi: f32, angle_scale: f32) -> f32 {
        let phi = phi.rem_euclid(2.0 * PI);
        let Some((hi, dh)) = bracket(&self.h_angles, phi) else {
            return 0.0;
        };
        let theta = if angle_scale > 0.0 {
            theta / angle_scale
        } else if angle_scale < 0.0 {
            (PI - theta) / angle_scale + PI
        } else {
            theta
        };
        let last = self.v_angles[self.v_angles.len() - 1];
        let (vi, dv) = if theta < 0.0 {
            return 0.0;
        } else if theta >= last {
            // The table's own upper endpoint, which the half-open bracket
            // below cannot reach — and, for a table that runs to the pole,
            // anything an angle scale pushes past π. hdEmbree takes the last
            // sample for every θ ≥ π whatever the table covers, so a
            // hemisphere-only downlight would light its own back pole with
            // its 90° value; that is not followed here.
            if theta == last || last >= PI - 1e-4 {
                (self.v_angles.len() - 2, 1.0)
            } else {
                return 0.0;
            }
        } else {
            match bracket(&self.v_angles, theta) {
                Some(b) => b,
                None => return 0.0,
            }
        };
        let lerp = |t: f32, a: f32, b: f32| a + t * (b - a);
        let i0 = lerp(dv, self.intensity[hi][vi], self.intensity[hi][vi + 1]);
        let i1 = lerp(
            dv,
            self.intensity[hi + 1][vi],
            self.intensity[hi + 1][vi + 1],
        );
        lerp(dh, i0, i1)
    }
}

/// The interval `[a[i], a[i+1])` holding `x`, and `x`'s fraction across it.
fn bracket(a: &[f32], x: f32) -> Option<(usize, f32)> {
    // First index whose angle exceeds x; the interval starts one before it.
    let upper = a.partition_point(|&v| v <= x);
    if upper == 0 || upper >= a.len() {
        return None;
    }
    let i = upper - 1;
    let span = a[i + 1] - a[i];
    let t = if span > 0.0 { (x - a[i]) / span } else { 0.0 };
    Some((i, t.clamp(0.0, 1.0)))
}

/// Integrates the intensity over the solid-angle patches the table defines
/// and divides by the area of the unit sphere — or hemisphere, when the polar
/// range spans no more than a quarter turn (plus a 0.1 rad allowance). This
/// is hdEmbree's `PxrIESFile::pxr_extra_process`, whose comment records that
/// it matches Karma and RenderMan.
///
/// The spec text writes `iesSample · iesProfilePower`; every implementation,
/// and the attribute's own description ("normalizes the IES profile so that
/// it affects the shaping of the light while preserving the overall energy
/// output"), divides.
fn profile_power(v: &[f32], h: &[f32], intensity: &[Vec<f32>]) -> f32 {
    let (v_min, v_max) = (v[0], v[v.len() - 1]);
    let is_sphere = v_max - v_min > 0.5 * PI + 0.1;
    let mut power = 0.0;
    for hi in 0..h.len() - 1 {
        for vi in 0..v.len() - 1 {
            let dh = h[hi + 1] - h[hi];
            let dv = v[vi + 1] - v[vi];
            let i0 = 0.5 * (intensity[hi][vi] + intensity[hi][vi + 1]);
            let i1 = 0.5 * (intensity[hi + 1][vi] + intensity[hi + 1][vi + 1]);
            let ds = dh * dv * (v[vi] + 0.5 * dv).sin();
            power += ds * 0.5 * (i0 + i1);
        }
    }
    power / (PI * if is_sphere { 4.0 } else { 2.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit Rec.709 luminance at every temperature, warm below D65 and cool
    /// above it, and near white at 6500 K.
    #[test]
    fn blackbody_is_luminance_normalised_and_ordered() {
        for k in [1000.0f32, 1900.0, 3200.0, 5000.0, 6500.0, 9000.0, 15000.0] {
            let c = blackbody_rgb(k);
            let y = 0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z;
            assert!((y - 1.0).abs() < 1e-3, "{k} K: luminance {y}");
        }
        let warm = blackbody_rgb(3000.0);
        assert!(
            warm.x > warm.y && warm.y > warm.z,
            "3000 K is orange: {warm}"
        );
        let cool = blackbody_rgb(10000.0);
        assert!(
            cool.z > cool.y && cool.y > cool.x,
            "10000 K is blue: {cool}"
        );
        let d65 = blackbody_rgb(6500.0);
        assert!(
            d65.abs_diff_eq(Vec3A::ONE, 0.05),
            "6500 K is near white, not exactly: {d65}"
        );
        // Clamped outside the approximation's range.
        assert_eq!(blackbody_rgb(500.0), blackbody_rgb(1000.0));
        assert_eq!(blackbody_rgb(40000.0), blackbody_rgb(15000.0));
    }

    /// Pinned against an independent float64 evaluation of the same chain
    /// (Krystek at 3000 K → u = 0.250515, v = 0.347680 → x = 0.437049,
    /// y = 0.404375), so a transcription slip in a coefficient shows.
    #[test]
    fn blackbody_matches_krystek_at_3000k() {
        let c = blackbody_rgb(3000.0);
        // XYZ(x=0.43693, y=0.40407, Y=1) through the Rec.709 matrix, then
        // luminance-normalised (already Y = 1).
        let expect = Vec3A::new(1.769931, 0.844704, 0.270638);
        assert!(c.abs_diff_eq(expect, 1e-4), "{c} vs {expect}");
        let d65 = blackbody_rgb(6500.0);
        let expect = Vec3A::new(1.044155, 0.983269, 1.035692);
        assert!(d65.abs_diff_eq(expect, 1e-4), "{d65} vs {expect}");
    }

    #[test]
    fn distant_size_factor_follows_the_spec() {
        assert_eq!(distant_size_factor(0.0), 1.0);
        let small = distant_size_factor(0.53);
        let theta = 0.265f64.to_radians();
        let exact = std::f64::consts::PI * theta.sin().powi(2);
        assert!(
            ((small as f64 - exact) / exact).abs() < 1e-6,
            "{small} vs {exact}"
        );
        assert!(
            (distant_size_factor(180.0) - PI).abs() < 1e-5,
            "hemisphere: π"
        );
        // Past a hemisphere the factor keeps growing, to 2π at a full sphere.
        assert!(distant_size_factor(270.0) > PI);
        assert!((distant_size_factor(360.0) - 2.0 * PI).abs() < 1e-4);
    }

    fn shaping() -> Shaping {
        Shaping::new(Mat3A::IDENTITY) // axis = −Z
    }

    #[test]
    fn neutral_shaping_changes_nothing() {
        let s = shaping();
        assert!(s.is_neutral());
        for d in [
            -Vec3A::Z,
            Vec3A::X,
            Vec3A::Z,
            Vec3A::new(0.3, -0.4, 0.5).normalize(),
        ] {
            assert_eq!(s.factor(d), Vec3A::ONE, "{d}");
        }
    }

    #[test]
    fn hard_cone_cuts_off_exactly_at_its_angle() {
        let mut s = shaping();
        s.cone_angle_deg = 30.0;
        let off = |deg: f32| {
            let r = deg.to_radians();
            Vec3A::new(r.sin(), 0.0, -r.cos())
        };
        assert_eq!(s.factor(off(0.0)), Vec3A::ONE);
        assert_eq!(s.factor(off(29.0)), Vec3A::ONE);
        assert_eq!(s.factor(off(31.0)), Vec3A::ZERO);
        assert_eq!(s.factor(Vec3A::Z), Vec3A::ZERO, "behind the light");
    }

    #[test]
    fn soft_cone_ramps_over_the_softened_fraction() {
        let mut s = shaping();
        s.cone_angle_deg = 40.0;
        s.cone_softness = 0.5; // ramp from 20° to 40°
        let off = |deg: f32| {
            let r = deg.to_radians();
            Vec3A::new(0.0, r.sin(), -r.cos())
        };
        assert_eq!(s.factor(off(19.0)).x, 1.0);
        let mid = s.factor(off(30.0)).x;
        assert!((mid - 0.5).abs() < 1e-4, "smoothstep midpoint: {mid}");
        assert_eq!(s.factor(off(41.0)).x, 0.0);
        // Softness past 1 is clamped, not extrapolated below zero.
        s.cone_softness = 3.0;
        assert!(s.factor(off(1.0)).x > 0.99);
    }

    #[test]
    fn focus_is_a_power_of_the_axis_cosine_toward_the_tint() {
        let mut s = shaping();
        s.focus = 2.0;
        s.focus_tint = Vec3A::new(1.0, 0.0, 0.0);
        let r = 60f32.to_radians(); // cos = 0.5 → ff = 0.25
        let f = s.factor(Vec3A::new(r.sin(), 0.0, -r.cos()));
        assert!(f.abs_diff_eq(Vec3A::new(1.0, 0.25, 0.25), 1e-5), "{f}");
        assert_eq!(s.factor(-Vec3A::Z), Vec3A::ONE, "on axis: white");
        // |cos|: behind the light is focused like in front of it.
        assert!(
            s.factor(Vec3A::new(r.sin(), 0.0, r.cos()))
                .abs_diff_eq(f, 1e-5)
        );
    }

    #[test]
    fn light_texture_is_nearest_and_clamps() {
        let red = Vec3A::new(8.0, 0.0, 0.0);
        let green = Vec3A::new(0.0, 1.0, 0.0);
        let img = LightTexture::new(2, 1, vec![red, green]).unwrap();
        assert_eq!(img.texel(0.49, 0.5), red);
        assert_eq!(img.texel(0.51, 0.5), green);
        assert_eq!(img.texel(-3.0, 9.0), red, "clamped, not wrapped");
        assert_eq!(img.texel(1.0, 0.0), green, "s = 1 is the last column");
        assert!(LightTexture::new(2, 2, vec![red]).is_none());
    }

    /// The image's top row lands on the rectangle's +Y edge and its left
    /// column on the −X edge, under any placement of the parallelogram.
    #[test]
    fn rect_texture_orientation() {
        let (tl, tr, bl, br) = (Vec3A::X, Vec3A::Y, Vec3A::Z, Vec3A::ONE);
        let img = Arc::new(LightTexture::new(2, 2, vec![tl, tr, bl, br]).unwrap());
        // A 4×2 rectangle in a tilted plane.
        let eu = Vec3A::new(4.0, 0.0, 0.0);
        let ev = Vec3A::new(0.0, 1.0, 1.0).normalize() * 2.0;
        let o = Vec3A::new(-2.0, 0.0, 0.0);
        let map = RectTexture::new(img, o, eu, ev).unwrap();
        let at = |u: f32, v: f32| map.at(o + u * eu + v * ev);
        assert_eq!(at(0.25, 0.75), tl);
        assert_eq!(at(0.75, 0.75), tr);
        assert_eq!(at(0.25, 0.25), bl);
        assert_eq!(at(0.75, 0.25), br);
        assert!(
            RectTexture::new(
                Arc::new(LightTexture::new(1, 1, vec![tl]).unwrap()),
                o,
                eu,
                eu
            )
            .is_none()
        );
    }

    fn two_zone_profile() -> IesProfile {
        // 100 cd below 90°, 0 above — a downlight, full azimuth.
        let v: Vec<f32> = [0.0f32, 89.0, 90.0, 180.0].map(f32::to_radians).to_vec();
        let h: Vec<f32> = [0.0f32, 360.0].map(f32::to_radians).to_vec();
        let row = vec![100.0, 100.0, 0.0, 0.0];
        IesProfile::new(v, h, vec![row.clone(), row]).unwrap()
    }

    #[test]
    fn ies_profile_evaluates_bilinearly_and_rejects_bad_tables() {
        let p = two_zone_profile();
        assert_eq!(p.eval(0.3, 1.0, 0.0), 100.0);
        assert_eq!(p.eval(2.0, 5.0, 0.0), 0.0);
        let mid = p.eval(89.5f32.to_radians(), 0.0, 0.0);
        assert!((mid - 50.0).abs() < 1e-2, "{mid}");
        // angleScale > 0 divides θ: 0.5 doubles every angle, so 60° reads 120°.
        assert_eq!(p.eval(60f32.to_radians(), 0.0, 0.5), 0.0);
        assert!(IesProfile::new(vec![0.0], vec![0.0, 1.0], vec![vec![1.0], vec![1.0]]).is_none());
        assert!(IesProfile::new(vec![0.0, 1.0], vec![0.0, 1.0], vec![vec![1.0]]).is_none());
        let nan = vec![vec![1.0, f32::NAN], vec![1.0, 1.0]];
        assert!(IesProfile::new(vec![0.0, 1.0], vec![0.0, 1.0], nan).is_none());
        let inf = vec![vec![1.0, 1.0], vec![1.0, 1.0]];
        assert!(IesProfile::new(vec![0.0, f32::INFINITY], vec![0.0, 1.0], inf).is_none());
    }

    /// A hemisphere-only table covers nothing past its last angle, and an
    /// angle scale that pushes θ past π must not wrap round to that angle's
    /// value; a table that does reach the pole keeps its endpoint.
    #[test]
    fn ies_profile_is_dark_outside_its_vertical_range() {
        let deg = |a: [f32; 3]| a.map(f32::to_radians).to_vec();
        let h = [0.0f32, 360.0].map(f32::to_radians).to_vec();
        let half =
            IesProfile::new(deg([0.0, 45.0, 90.0]), h.clone(), vec![vec![5.0; 3]; 2]).unwrap();
        assert_eq!(half.eval(0.5 * PI, 0.0, 0.0), 5.0, "its own endpoint");
        assert_eq!(half.eval(2.0, 0.0, 0.0), 0.0);
        assert_eq!(half.eval(PI, 0.0, 0.0), 0.0, "the back pole");
        assert_eq!(half.eval(3.0, 0.0, 0.5), 0.0, "scaled past π");
        let full = IesProfile::new(deg([0.0, 90.0, 180.0]), h, vec![vec![5.0; 3]; 2]).unwrap();
        assert_eq!(full.eval(PI, 0.0, 0.0), 5.0);
        assert_eq!(
            full.eval(3.0, 0.0, 0.5),
            5.0,
            "scaled past π on a full table"
        );
    }

    /// The power integral of a constant profile over the whole sphere is the
    /// constant: `∫ I dω / 4π`.
    #[test]
    fn ies_power_of_a_uniform_sphere_is_its_intensity() {
        let v: Vec<f32> = (0..=36).map(|i| (i as f32 * 5.0).to_radians()).collect();
        let h: Vec<f32> = (0..=72).map(|i| (i as f32 * 5.0).to_radians()).collect();
        let table = vec![vec![7.0; v.len()]; h.len()];
        let p = IesProfile::new(v, h, table).unwrap();
        assert!((p.power() - 7.0).abs() < 0.01, "{}", p.power());
    }

    /// The beam points down the light's −Z: a downlight profile lights what
    /// the light faces and nothing behind it.
    #[test]
    fn ies_zero_degrees_is_the_emission_axis() {
        let mut s = shaping();
        s.ies = Some(IesShaping {
            profile: Arc::new(two_zone_profile()),
            angle_scale: 0.0,
            normalize: false,
        });
        assert_eq!(s.factor(-Vec3A::Z).x, 100.0);
        assert_eq!(s.factor(Vec3A::Z).x, 0.0);
        s.ies.as_mut().unwrap().normalize = true;
        let p = two_zone_profile().power();
        assert!((s.factor(-Vec3A::Z).x - 100.0 / p).abs() < 1e-3);
    }
}
