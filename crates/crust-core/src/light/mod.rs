use glam::Vec3A;

mod area;
mod infinite;
mod list;
mod rect;
mod shape;

pub use area::{AreaLight, AreaShape};
pub use infinite::{DistantLight, DomeLight, projected_cone_solid_angle};
pub use list::{LightList, LightSelection};
pub use rect::RectShape;
pub use shape::{AffineShape, LightShape, SphereShape, UnitShape};

/// One sampled connection from a shading point to a light: where to aim
/// the shadow ray, how far it must reach, the radiance arriving from that
/// direction, and the solid-angle density of having chosen it.
///
/// Directions rather than points, because a light can be at infinity — a
/// `DistantLight` or a `DomeLight` has no surface point to aim at.
#[derive(Clone, Copy, Debug)]
pub struct LightSample {
    /// Unit direction from the shading point toward the light.
    pub direction: Vec3A,
    /// How far the shadow ray must be traced. `f32::INFINITY` for lights
    /// at infinity — nothing beyond the scene can occlude them.
    pub distance: f32,
    /// Radiance arriving along `direction`.
    pub radiance: Vec3A,
    /// Solid-angle pdf of this direction under the light's own sampling.
    ///
    /// Always finite and positive: crust has no delta lights. A
    /// `DistantLight` with a zero `angle` is widened to a small but real
    /// cone rather than being made singular, which keeps one MIS path
    /// through the integrator instead of two.
    pub pdf: f32,
}

/// The `Light` trait is what the integrator's light-sampling strategy (NEE)
/// needs from a light: a direction to aim a shadow ray, the solid-angle
/// density of that choice for MIS, the radiance it carries, and — for
/// lights with scene geometry — the geometry id that lets a bounce ray
/// recognize the light it hit.
///
/// The MIS pairing is the thing to be careful with. Every light has two
/// ways of being found: NEE samples it directly, and a bounce ray may
/// arrive at it by chance. Both sides must evaluate the *same* density or
/// emission is double-counted. For lights with geometry that second path
/// is a bounce hit, weighted with [`Light::pdf_at_point`]; for lights at
/// infinity it is a ray escaping the scene, weighted with
/// [`Light::escaped`]. A light implements whichever applies.
pub trait Light: Send + Sync {
    /// Short name for the `--stats` light breakdown.
    fn kind(&self) -> &'static str {
        "light"
    }

    /// Samples a direction from `from` toward the light. `None` when the
    /// light cannot be reached from there (below a dome's horizon, say).
    ///
    /// # Parameters
    /// - `u`, `v`: Unit random numbers driving the sample.
    #[must_use]
    fn sample_li(&self, from: Vec3A, u: f32, v: f32) -> Option<LightSample>;

    /// Solid-angle pdf, as seen from `from`, of [`Light::sample_li`] having
    /// produced `light_point` — the bounce side of MIS for a light whose
    /// geometry a ray hit. Zero means `sample_li` never delivers that point
    /// (and is the default, for lights at infinity, which have no such
    /// point): nothing competes there, so the bounce keeps full weight.
    fn pdf_at_point(&self, _from: Vec3A, _light_point: Vec3A) -> f32 {
        0.0
    }

    /// For a ray that escaped the scene along `direction`: the radiance it
    /// picks up and the solid-angle pdf NEE would have used for that
    /// direction, as `(radiance, pdf)`. This is the bounce side of MIS for
    /// lights at infinity. `None` for lights with finite geometry, and for
    /// directions this light does not cover.
    fn escaped(&self, _from: Vec3A, _direction: Vec3A) -> Option<(Vec3A, f32)> {
        None
    }

    /// Whether this light is at infinity — whether [`Light::escaped`] can
    /// ever answer `Some`. [`LightList`] records the lights that are once,
    /// so an escaping ray asks only them rather than every light in the
    /// scene. A light that overrides `escaped` must override this too, or
    /// escaping rays never find it.
    fn at_infinity(&self) -> bool {
        false
    }

    /// The `geom_id` of this light's scene geometry in the world, used to
    /// recognize the light when a bounce ray hits it. `None` for lights
    /// with no geometry in the world.
    fn geom_id(&self) -> Option<u32> {
        None
    }

    /// The light's emitted power as a luminance flux — what
    /// [`LightSelection::Power`] divides shadow rays by. It is a sampling
    /// weight, never a shading quantity, so it must be proportionate across
    /// lights and positive wherever the light emits, not exact.
    ///
    /// `None` for a light at infinity, which has no finite power to compare.
    /// pbrt-v4's `PowerLightSampler` gives one the flux it sends into the
    /// scene's bounding sphere; measured here, that let a sun take 88% of the
    /// shadow rays from its dome and made `samples/domelight.usda` 1.4× noisier,
    /// because in the sun's shadows the dome is the only light. So power
    /// selection gives lights at infinity a fixed share instead, as pbrt-v4's
    /// BVH sampler and Karma do.
    fn power(&self) -> Option<f32>;
}

#[cfg(test)]
mod tests;
