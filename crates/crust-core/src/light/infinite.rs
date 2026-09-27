//! Lights at infinity: [`DistantLight`] and [`DomeLight`], found by the bounce
//! side through [`Light::escaped`].

use std::f32::consts::PI;
use std::sync::Arc;

use glam::{Mat3A, Vec3A};

use crate::environment::EnvironmentMap;
use crate::pdf::PdfSolidAngle;

use super::{Light, LightSample};

/// A `UsdLuxDistantLight`: parallel light from infinitely far away, as the
/// sun is.
///
/// Two conventions worth stating, because renderers differ.
///
/// **The cone is always real.** UsdLux gives the source an angular diameter
/// (`inputs:angle`, default 0.53° — the sun's), and authors may set it to
/// zero for perfectly sharp shadows. Rather than making that a delta light,
/// which would need a second MIS path through the integrator, a zero angle
/// is widened to [`MIN_DISTANT_ANGLE_DEG`]. The resulting penumbra is far
/// below a pixel at any sane scene scale, and MIS handles the rest: when a
/// bounce ray happens into the tiny cone the light pdf is enormous, so the
/// bounce side's weight collapses to nothing and no firefly survives.
///
/// **The light stores radiance, and there are two ways to ask for it.**
/// UsdLux says `intensity` is the source's *luminance* in nits
/// ([`DistantLight::with_radiance`]); with `inputs:normalize` it divides by
/// `π·sin²θ`, which makes `intensity` the *illuminance* on a surface facing
/// the light ([`DistantLight::new`]). The importer decides which — see
/// `emit_distant_light` — and also owns what widening a zero angle means for
/// each: a zero-angle light is a delta, whose `intensity` the spec and
/// hdEmbree both deliver as irradiance, so it goes through `new` too.
pub struct DistantLight {
    /// Unit direction the light travels *toward* (the direction photons
    /// move), so a shading point is lit from `-direction`.
    pub(super) direction: Vec3A,
    /// Radiance inside the cone.
    pub(super) radiance: Vec3A,
    /// Half-angle of the source cone, in radians.
    pub(super) cos_half_angle: f32,
    /// Solid angle of the cone, `2π(1 − cos θ)`.
    pub(super) solid_angle: f32,
}

/// The floor a `DistantLight`'s angular diameter is clamped to, in degrees.
/// Small enough to read as a sharp shadow, large enough that the cone stays
/// a genuine solid angle with a finite pdf.
pub const MIN_DISTANT_ANGLE_DEG: f32 = 0.05;

impl DistantLight {
    /// A distant light delivering `irradiance` to a surface facing it,
    /// however wide the cone. `direction` is the direction the light travels
    /// toward (UsdLux's convention: a distant light points down its local
    /// -Z). `angle_deg` is the source's angular *diameter*, as `inputs:angle`
    /// gives it.
    ///
    /// The radiance is `E / (π·sin²θ)` — the *cosine-weighted* solid angle,
    /// which is what makes `E` exact on the facing surface — not `E / Ω`,
    /// which undershoots by `cos²(θ/2)`: 1.7% at a 30° diameter, nothing at
    /// the sun's.
    pub fn new(direction: Vec3A, irradiance: Vec3A, angle_deg: f32) -> Self {
        let half = 0.5 * Self::clamp_diameter(angle_deg).to_radians();
        Self::with_radiance(
            direction,
            irradiance / projected_cone_solid_angle(half).max(1e-12),
            angle_deg,
        )
    }

    /// A distant light of the given radiance (nits) inside its cone.
    pub fn with_radiance(direction: Vec3A, radiance: Vec3A, angle_deg: f32) -> Self {
        let half_angle = 0.5 * Self::clamp_diameter(angle_deg).to_radians();
        let cos_half_angle = half_angle.cos();
        Self {
            direction: direction.normalize(),
            radiance,
            cos_half_angle,
            solid_angle: 2.0 * std::f32::consts::PI * (1.0 - cos_half_angle),
        }
    }

    /// The angular diameter actually used: the widening floor and a ceiling
    /// short of a full hemisphere.
    pub fn clamp_diameter(angle_deg: f32) -> f32 {
        angle_deg.clamp(MIN_DISTANT_ANGLE_DEG, 179.0)
    }

    /// Radiance within the cone.
    pub(super) fn radiance(&self) -> Vec3A {
        self.radiance
    }

    /// Uniform-cone pdf, constant inside the cone. Finite and positive: the
    /// solid angle is floored, and the cone is at least
    /// [`MIN_DISTANT_ANGLE_DEG`] wide.
    pub(super) fn cone_pdf(&self) -> PdfSolidAngle {
        PdfSolidAngle::from_measure(1.0 / self.solid_angle.max(1e-12))
    }

    /// Is `direction` (pointing away from the shaded point) inside the
    /// cone of directions this light occupies?
    pub(super) fn covers(&self, direction: Vec3A) -> bool {
        direction.dot(-self.direction) >= self.cos_half_angle
    }
}

impl Light for DistantLight {
    fn kind(&self) -> &'static str {
        "distant"
    }

    fn sample_li(&self, _from: Vec3A, u: f32, v: f32) -> Option<LightSample> {
        // Uniform direction within the cone around `-direction`.
        let cos_theta = 1.0 - u * (1.0 - self.cos_half_angle);
        let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
        let phi = 2.0 * std::f32::consts::PI * v;
        let local = Vec3A::new(sin_theta * phi.cos(), sin_theta * phi.sin(), cos_theta);
        Some(LightSample {
            direction: utils::align_to_normal(local, -self.direction).normalize(),
            // Nothing beyond the scene can occlude a light at infinity.
            distance: f32::INFINITY,
            radiance: self.radiance(),
            pdf: self.cone_pdf(),
        })
    }

    fn escaped(&self, _from: Vec3A, direction: Vec3A) -> Option<(Vec3A, Option<PdfSolidAngle>)> {
        self.covers(direction)
            .then(|| (self.radiance(), Some(self.cone_pdf())))
    }

    fn at_infinity(&self) -> bool {
        true
    }

    /// At infinity: no finite power (see [`Light::power`]).
    fn power(&self) -> Option<f32> {
        None
    }
}

/// The cosine-weighted solid angle of a cone of half-angle `half` (≤ π/2)
/// seen along its axis, `π·sin²θ`: the irradiance unit radiance inside it
/// delivers to a surface facing it.
///
/// Evaluated as `π·(1 − c)(1 + c)` from the f32 cosine `c` — the same
/// cosine [`DistantLight`] bounds its cone with — rather than from `sin θ`.
/// The two agree mathematically, but the cone crust actually samples and
/// tests against is the one the *rounded* cosine bounds, and for a sun-sized
/// cone that rounding is 2.4e-4 of the solid angle: dividing by `π·sin²θ`
/// would deliver the authored illuminance to that accuracy, dividing by this
/// delivers it exactly. (`1 − c` is itself exact — Sterbenz — so this is
/// also free of the cancellation that made `2π(1 − cos θ)` computed the
/// obvious way in f32 wrong by the same 2.4e-4.)
pub fn projected_cone_solid_angle(half: f32) -> f32 {
    let c = half.clamp(0.0, 0.5 * PI).cos();
    PI * (1.0 - c) * (1.0 + c)
}

/// The uniform density over the sphere of directions, `1 / 4π`.
const UNIFORM_SPHERE_PDF: PdfSolidAngle = PdfSolidAngle::from_measure(1.0 / (4.0 * PI));

/// A `UsdLuxDomeLight`: an infinite environment surrounding the scene.
///
/// Covers every direction, so once one exists it *is* the background — the
/// integrator's built-in sky gradient stops applying, because
/// [`Light::escaped`] answers for every ray that leaves.
///
/// Radiance is a uniform `tint` multiplied by an optional lat-long
/// [`EnvironmentMap`]. With a map, directions are importance-sampled from
/// its luminance so a small bright sun in an HDRI does not become a firefly
/// farm; without one, directions are sampled uniformly over the sphere.
///
/// `orientation` maps *world* directions into the dome's own space, so a
/// rotated dome prim rotates the sky. It is the inverse of the prim's
/// world transform, cached once.
pub struct DomeLight {
    pub(super) tint: Vec3A,
    pub(super) map: Option<Arc<EnvironmentMap>>,
    /// World → dome-local rotation.
    pub(super) world_to_light: Mat3A,
    /// Dome-local → world rotation.
    pub(super) light_to_world: Mat3A,
}

impl DomeLight {
    pub fn new(tint: Vec3A, map: Option<Arc<EnvironmentMap>>, light_to_world: Mat3A) -> Self {
        Self {
            tint,
            map,
            world_to_light: light_to_world.inverse(),
            light_to_world,
        }
    }

    /// Radiance arriving from a world-space `direction`.
    pub(super) fn radiance_toward(&self, direction: Vec3A) -> Vec3A {
        match &self.map {
            Some(map) => self.tint * map.radiance(self.world_to_light * direction),
            None => self.tint,
        }
    }

    /// Solid-angle pdf of a world-space `direction` under this dome's own
    /// sampling: the map's distribution, or uniform over the sphere. `None`
    /// where the map's is not a finite, positive density — a direction
    /// `sample_li` never delivers.
    pub(super) fn pdf_toward(&self, direction: Vec3A) -> Option<PdfSolidAngle> {
        match &self.map {
            Some(map) => PdfSolidAngle::new(map.pdf(self.world_to_light * direction)),
            None => Some(UNIFORM_SPHERE_PDF),
        }
    }
}

impl Light for DomeLight {
    fn kind(&self) -> &'static str {
        "dome"
    }

    fn sample_li(&self, _from: Vec3A, u: f32, v: f32) -> Option<LightSample> {
        let (direction, radiance, pdf) = match &self.map {
            Some(map) => {
                let (local, radiance, pdf) = map.sample(u, v)?;
                (
                    (self.light_to_world * local).normalize(),
                    radiance,
                    PdfSolidAngle::new(pdf)?,
                )
            }
            None => {
                // Uniform over the sphere.
                let z = 1.0 - 2.0 * u;
                let r = (1.0 - z * z).max(0.0).sqrt();
                let phi = std::f32::consts::TAU * v;
                (
                    Vec3A::new(r * phi.cos(), z, r * phi.sin()),
                    Vec3A::ONE,
                    UNIFORM_SPHERE_PDF,
                )
            }
        };
        Some(LightSample {
            direction,
            // Nothing in the scene can occlude the environment beyond it.
            distance: f32::INFINITY,
            radiance: self.tint * radiance,
            pdf,
        })
    }

    fn escaped(&self, _from: Vec3A, direction: Vec3A) -> Option<(Vec3A, Option<PdfSolidAngle>)> {
        // A dome covers every direction, so every escaping ray finds it; the
        // same refusal as `sample_li`'s says whether NEE could have.
        Some((self.radiance_toward(direction), self.pdf_toward(direction)))
    }

    fn at_infinity(&self) -> bool {
        true
    }

    /// At infinity: no finite power (see [`Light::power`]).
    fn power(&self) -> Option<f32> {
        None
    }
}
