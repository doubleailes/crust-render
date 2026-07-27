use crate::environment::EnvironmentMap;
use crate::material::{Emissive, Material};
use glam::{Mat3A, Vec3A};
use std::sync::Arc;

/// The emitting surface of an area light, decoupled from any material: pure
/// geometry that knows how to sample itself uniformly by area. One shape
/// implementation per supported UsdLux schema (sphere, rect, …).
pub trait LightShape: Send + Sync {
    /// A point on the surface, uniform by area, from two unit random numbers.
    fn sample_point(&self, u: f32, v: f32) -> Vec3A;

    /// Outward surface normal at a point known to lie on the shape.
    fn normal_at(&self, p: Vec3A) -> Vec3A;

    /// Total surface area.
    fn area(&self) -> f32;
}

/// Spherical light surface (UsdLux `SphereLight`).
pub struct SphereShape {
    pub center: Vec3A,
    pub radius: f32,
}

impl LightShape for SphereShape {
    fn sample_point(&self, u: f32, v: f32) -> Vec3A {
        let theta = 2.0 * std::f32::consts::PI * u;
        let phi = (1.0 - 2.0 * v).acos();
        let n = Vec3A::new(
            phi.sin() * theta.cos(),
            phi.sin() * theta.sin(),
            phi.cos(),
        );
        self.center + self.radius * n
    }

    fn normal_at(&self, p: Vec3A) -> Vec3A {
        (p - self.center).normalize()
    }

    fn area(&self) -> f32 {
        4.0 * std::f32::consts::PI * self.radius * self.radius
    }
}

/// Rectangular light surface (UsdLux `RectLight`): the parallelogram
/// `origin + u·edge_u + v·edge_v`, emitting from the side its `normal`
/// faces. Per the UsdLux convention the importer orients the normal along
/// the light's local -Z.
pub struct RectShape {
    pub origin: Vec3A,
    pub edge_u: Vec3A,
    pub edge_v: Vec3A,
    pub normal: Vec3A,
}

impl RectShape {
    pub fn new(origin: Vec3A, edge_u: Vec3A, edge_v: Vec3A, normal: Vec3A) -> Self {
        Self {
            origin,
            edge_u,
            edge_v,
            normal: normal.normalize(),
        }
    }
}

impl LightShape for RectShape {
    fn sample_point(&self, u: f32, v: f32) -> Vec3A {
        self.origin + u * self.edge_u + v * self.edge_v
    }

    fn normal_at(&self, _p: Vec3A) -> Vec3A {
        self.normal
    }

    fn area(&self) -> f32 {
        self.edge_u.cross(self.edge_v).length()
    }
}

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
    /// Samples a direction from `from` toward the light. `None` when the
    /// light cannot be reached from there (below a dome's horizon, say).
    ///
    /// # Parameters
    /// - `u`, `v`: Unit random numbers driving the sample.
    fn sample_li(&self, from: Vec3A, u: f32, v: f32) -> Option<LightSample>;

    /// Solid-angle pdf, as seen from `from`, of [`Light::sample_li`] having
    /// produced `light_point` — the bounce side of MIS for a light whose
    /// geometry a ray hit. Lights at infinity have no such point and keep
    /// the default.
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

    /// The `geom_id` of this light's scene geometry in the world, used to
    /// recognize the light when a bounce ray hits it. `None` for lights
    /// with no geometry in the world.
    fn geom_id(&self) -> Option<u32> {
        None
    }
}

/// A geometric area light: any [`LightShape`] paired with the [`Emissive`]
/// material its scene geometry carries (Cornell-box semantics — the same
/// surface is both light and visible object).
pub struct AreaLight {
    shape: Box<dyn LightShape>,
    material: Arc<Emissive>,
    /// The world `geom_id` of the emissive geometry this light shares its
    /// surface with — how bounce hits are attributed back to the light.
    geom_id: u32,
}

impl AreaLight {
    pub fn new(shape: Box<dyn LightShape>, material: Arc<Emissive>, geom_id: u32) -> Self {
        Self {
            shape,
            material,
            geom_id,
        }
    }

    /// Solid-angle pdf of sampling `light_point` uniformly by area, as seen
    /// from `from`: `dist² / (cos(θ_light) · area)`, where θ_light is the
    /// angle between the light's surface normal at `light_point` and the
    /// direction back toward the shaded point. Back-facing points clamp the
    /// cosine to zero, so their pdf explodes and both MIS strategies agree
    /// the contribution is negligible — area lights are effectively
    /// one-sided.
    fn solid_angle_pdf(&self, from: Vec3A, light_point: Vec3A) -> f32 {
        let direction = light_point - from;
        let distance_squared = direction.length_squared();
        let dir_to_light = direction.normalize();
        let light_normal = self.shape.normal_at(light_point);
        let cosine = f32::max(light_normal.dot(-dir_to_light), 0.0);
        distance_squared / (cosine * self.shape.area() + 1e-4)
    }
}

impl Light for AreaLight {
    fn sample_li(&self, from: Vec3A, u: f32, v: f32) -> Option<LightSample> {
        let light_point = self.shape.sample_point(u, v);
        let to_light = light_point - from;
        let distance = to_light.length();
        if distance < 1e-6 {
            return None;
        }
        Some(LightSample {
            direction: to_light / distance,
            distance,
            radiance: self.material.emitted(),
            pdf: self.solid_angle_pdf(from, light_point),
        })
    }

    fn pdf_at_point(&self, from: Vec3A, light_point: Vec3A) -> f32 {
        self.solid_angle_pdf(from, light_point)
    }

    fn geom_id(&self) -> Option<u32> {
        Some(self.geom_id)
    }
}

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
/// **`intensity × color` is irradiance, not radiance.** The radiance the
/// light emits is derived as `E / Ω` over the cone's solid angle, so
/// widening the angle softens shadows without changing exposure — the
/// behaviour Hydra and most production renderers normalize to. Treating the
/// input as radiance instead would make a sun-sized source almost black.
pub struct DistantLight {
    /// Unit direction the light travels *toward* (the direction photons
    /// move), so a shading point is lit from `-direction`.
    direction: Vec3A,
    /// Irradiance on a surface facing the light.
    irradiance: Vec3A,
    /// Half-angle of the source cone, in radians.
    cos_half_angle: f32,
    /// Solid angle of the cone, `2π(1 − cos θ)`.
    solid_angle: f32,
}

/// The floor a `DistantLight`'s angular diameter is clamped to, in degrees.
/// Small enough to read as a sharp shadow, large enough that the cone stays
/// a genuine solid angle with a finite pdf.
pub const MIN_DISTANT_ANGLE_DEG: f32 = 0.05;

impl DistantLight {
    /// `direction` is the direction the light travels toward (UsdLux's
    /// convention: a distant light points down its local -Z). `angle_deg`
    /// is the source's angular *diameter*, as `inputs:angle` gives it.
    pub fn new(direction: Vec3A, irradiance: Vec3A, angle_deg: f32) -> Self {
        let diameter = angle_deg.clamp(MIN_DISTANT_ANGLE_DEG, 179.0);
        let half_angle = 0.5 * diameter.to_radians();
        let cos_half_angle = half_angle.cos();
        Self {
            direction: direction.normalize(),
            irradiance,
            cos_half_angle,
            solid_angle: 2.0 * std::f32::consts::PI * (1.0 - cos_half_angle),
        }
    }

    /// Radiance within the cone: irradiance spread over its solid angle.
    fn radiance(&self) -> Vec3A {
        self.irradiance / self.solid_angle.max(1e-12)
    }

    /// Uniform-cone pdf, constant inside the cone.
    fn cone_pdf(&self) -> f32 {
        1.0 / self.solid_angle.max(1e-12)
    }

    /// Is `direction` (pointing away from the shaded point) inside the
    /// cone of directions this light occupies?
    fn covers(&self, direction: Vec3A) -> bool {
        direction.dot(-self.direction) >= self.cos_half_angle
    }
}

impl Light for DistantLight {
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

    fn escaped(&self, _from: Vec3A, direction: Vec3A) -> Option<(Vec3A, f32)> {
        self.covers(direction)
            .then(|| (self.radiance(), self.cone_pdf()))
    }
}

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
    tint: Vec3A,
    map: Option<Arc<EnvironmentMap>>,
    /// World → dome-local rotation.
    world_to_light: Mat3A,
    /// Dome-local → world rotation.
    light_to_world: Mat3A,
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
    fn radiance_toward(&self, direction: Vec3A) -> Vec3A {
        match &self.map {
            Some(map) => self.tint * map.radiance(self.world_to_light * direction),
            None => self.tint,
        }
    }

    /// Solid-angle pdf of a world-space `direction` under this dome's own
    /// sampling: the map's distribution, or uniform over the sphere.
    fn pdf_toward(&self, direction: Vec3A) -> f32 {
        match &self.map {
            Some(map) => map.pdf(self.world_to_light * direction),
            None => 1.0 / (4.0 * std::f32::consts::PI),
        }
    }
}

impl Light for DomeLight {
    fn sample_li(&self, _from: Vec3A, u: f32, v: f32) -> Option<LightSample> {
        let (direction, radiance, pdf) = match &self.map {
            Some(map) => {
                let (local, radiance, pdf) = map.sample(u, v)?;
                ((self.light_to_world * local).normalize(), radiance, pdf)
            }
            None => {
                // Uniform over the sphere.
                let z = 1.0 - 2.0 * u;
                let r = (1.0 - z * z).max(0.0).sqrt();
                let phi = std::f32::consts::TAU * v;
                (
                    Vec3A::new(r * phi.cos(), z, r * phi.sin()),
                    Vec3A::ONE,
                    1.0 / (4.0 * std::f32::consts::PI),
                )
            }
        };
        (pdf > 0.0).then(|| LightSample {
            direction,
            // Nothing in the scene can occlude the environment beyond it.
            distance: f32::INFINITY,
            radiance: self.tint * radiance,
            pdf,
        })
    }

    fn escaped(&self, _from: Vec3A, direction: Vec3A) -> Option<(Vec3A, f32)> {
        // A dome covers every direction, so every escaping ray finds it.
        Some((self.radiance_toward(direction), self.pdf_toward(direction)))
    }
}

/// The `LightList` struct manages a collection of light sources in the scene.
pub struct LightList {
    /// A vector of light sources stored as `Arc<dyn Light>` for shared ownership.
    pub lights: Vec<Arc<dyn Light>>,
}

impl Default for LightList {
    /// Creates a new, empty `LightList` as the default implementation.
    fn default() -> Self {
        Self::new()
    }
}

impl LightList {
    /// Creates a new, empty `LightList`.
    pub fn new() -> Self {
        Self { lights: Vec::new() }
    }

    /// Adds a light source to the `LightList`.
    pub fn add(&mut self, light: Arc<dyn Light>) {
        self.lights.push(light);
    }

    /// Uniformly picks a light source from the `LightList` from a single
    /// `[0, 1)` sample `u`.
    ///
    /// # Returns
    /// - `Some(&Arc<dyn Light>)` if the list is not empty.
    /// - `None` if the list is empty.
    pub fn pick(&self, u: f32) -> Option<&Arc<dyn Light>> {
        if self.lights.is_empty() {
            None
        } else {
            let i = (u * self.lights.len() as f32) as usize;
            // Guard against `u == 1.0 - epsilon` rounding to len.
            let i = i.min(self.lights.len() - 1);
            self.lights.get(i)
        }
    }

    /// Finds the light whose scene geometry has world id `geom_id`. Used
    /// by the integrator to attribute a bounce-hit emissive surface to its
    /// light for MIS; emissive geometry with no light-list entry returns
    /// `None`.
    pub fn find_by_geom(&self, geom_id: u32) -> Option<&Arc<dyn Light>> {
        self.lights
            .iter()
            .find(|l| l.geom_id() == Some(geom_id))
    }

    /// Returns the number of lights in the `LightList`.
    pub fn count(&self) -> usize {
        self.lights.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sphere_shape_samples_lie_on_surface() {
        let shape = SphereShape {
            center: Vec3A::new(1.0, 2.0, 3.0),
            radius: 0.5,
        };
        for (u, v) in [(0.0, 0.0), (0.25, 0.75), (0.99, 0.5), (0.5, 0.01)] {
            let p = shape.sample_point(u, v);
            let d = (p - shape.center).length();
            assert!((d - shape.radius).abs() < 1e-5, "sample off surface: {d}");
            let n = shape.normal_at(p);
            assert!((n.length() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn rect_shape_samples_lie_in_rect() {
        let shape = RectShape::new(
            Vec3A::new(-1.0, 5.0, -2.0),
            Vec3A::new(2.0, 0.0, 0.0),
            Vec3A::new(0.0, 0.0, 4.0),
            Vec3A::new(0.0, -1.0, 0.0),
        );
        assert!((shape.area() - 8.0).abs() < 1e-5);
        let p = shape.sample_point(0.5, 0.5);
        assert!((p - Vec3A::new(0.0, 5.0, 0.0)).length() < 1e-5);
        assert_eq!(shape.normal_at(p), Vec3A::new(0.0, -1.0, 0.0));
    }

    #[test]
    fn area_light_pdf_is_positive_facing_side() {
        let light = AreaLight::new(
            Box::new(SphereShape {
                center: Vec3A::new(0.0, 5.0, 0.0),
                radius: 1.0,
            }),
            Arc::new(Emissive::new(Vec3A::splat(10.0))),
            0,
        );
        // Nearest point on the sphere as seen from below.
        let pdf = light.pdf_at_point(Vec3A::ZERO, Vec3A::new(0.0, 4.0, 0.0));
        assert!(pdf.is_finite() && pdf > 0.0);

        // The sampled connection agrees: it aims upward at the light, stops
        // at a finite distance, and reports the same emission and a pdf of
        // the same shape.
        let s = light
            .sample_li(Vec3A::ZERO, 0.3, 0.7)
            .expect("a sphere overhead is always reachable");
        assert!(s.direction.is_normalized());
        assert!(s.distance.is_finite() && s.distance > 0.0);
        assert_eq!(s.radiance, Vec3A::splat(10.0));
        assert!(s.pdf.is_finite() && s.pdf > 0.0);

        // `sample_li` and `pdf_at_point` are the two MIS sides of one
        // strategy and must agree on the density of the same direction.
        let point = Vec3A::ZERO + s.direction * s.distance;
        let from_point = light.pdf_at_point(Vec3A::ZERO, point);
        assert!(
            (s.pdf - from_point).abs() <= 1e-3 * s.pdf.max(from_point),
            "MIS sides disagree: sample_li {} vs pdf_at_point {}",
            s.pdf,
            from_point
        );

        // An area light has geometry and no escaped-ray contribution.
        assert_eq!(light.geom_id(), Some(0));
        assert!(light.escaped(Vec3A::ZERO, Vec3A::Y).is_none());
    }

    /// The cone convention: every sampled direction lies inside the
    /// source cone, and `escaped` agrees about exactly which directions
    /// those are. Disagreement here would mean NEE and the bounce side
    /// find the light in different sets of directions.
    #[test]
    fn distant_light_cone_is_consistent() {
        let dir = Vec3A::new(0.3, -1.0, 0.2).normalize();
        let light = DistantLight::new(dir, Vec3A::splat(2.0), 10.0);

        let mut rng = openqmc::pcg::Rng::new(7);
        for _ in 0..2000 {
            let s = light
                .sample_li(Vec3A::ZERO, rng.next_f32(), rng.next_f32())
                .expect("a distant light is reachable from anywhere");
            assert!(s.direction.is_normalized());
            assert!(
                s.distance.is_infinite(),
                "a light at infinity cannot be occluded by anything in the scene"
            );
            // Sampled directions must be ones `escaped` also covers.
            let (radiance, pdf) = light
                .escaped(Vec3A::ZERO, s.direction)
                .expect("sample_li produced a direction escaped() does not cover");
            assert_eq!(radiance, s.radiance);
            assert!(
                (pdf - s.pdf).abs() < 1e-3 * s.pdf,
                "MIS sides disagree on the pdf: {} vs {}",
                s.pdf,
                pdf
            );
        }

        // And nothing outside the cone is covered: the opposite hemisphere
        // and a direction just past the half-angle both miss.
        assert!(light.escaped(Vec3A::ZERO, dir).is_none());
        let outside = utils::align_to_normal(
            Vec3A::new(20f32.to_radians().sin(), 0.0, 20f32.to_radians().cos()),
            -dir,
        )
        .normalize();
        assert!(
            light.escaped(Vec3A::ZERO, outside).is_none(),
            "a direction 20° off-axis is outside a 10° cone"
        );
    }

    /// The energy convention: `intensity × color` is the *irradiance* on a
    /// surface facing the light, and radiance is derived over the cone. So
    /// widening the angle must soften shadows without changing exposure —
    /// `L · Ω` stays put.
    ///
    /// This is the assumption most easily got backwards; treating the input
    /// as radiance instead would make a sun-sized source ~5 orders of
    /// magnitude too dark.
    #[test]
    fn distant_light_irradiance_is_angle_invariant() {
        let dir = -Vec3A::Y;
        let e = Vec3A::new(3.0, 2.0, 1.0);
        for angle in [0.0f32, 0.53, 5.0, 30.0] {
            let light = DistantLight::new(dir, e, angle);
            let s = light.sample_li(Vec3A::ZERO, 0.4, 0.6).expect("reachable");
            // Radiance integrated over the cone's solid angle (1/pdf)
            // returns the authored irradiance, whatever the angle.
            let recovered = s.radiance / s.pdf;
            assert!(
                (recovered - e).length() < 1e-3 * e.length(),
                "angle {angle}°: irradiance {recovered:?} != authored {e:?}"
            );
        }
    }

    /// A zero angle is widened rather than made singular, so the pdf stays
    /// finite and the integrator needs no delta-light path.
    #[test]
    fn distant_light_zero_angle_stays_finite() {
        let light = DistantLight::new(-Vec3A::Y, Vec3A::ONE, 0.0);
        let s = light.sample_li(Vec3A::ZERO, 0.5, 0.5).expect("reachable");
        assert!(s.pdf.is_finite() && s.pdf > 0.0, "pdf = {}", s.pdf);
        assert!(
            s.radiance.is_finite(),
            "radiance must stay finite: {:?}",
            s.radiance
        );
        // Still a *tight* cone: a degree off-axis is outside it.
        let off = utils::align_to_normal(
            Vec3A::new(1f32.to_radians().sin(), 0.0, 1f32.to_radians().cos()),
            Vec3A::Y,
        )
        .normalize();
        assert!(light.escaped(Vec3A::ZERO, off).is_none());
    }

    /// A distant light has no scene geometry, so bounce rays must never try
    /// to attribute a *hit* to it.
    #[test]
    fn distant_light_has_no_geometry() {
        let light = DistantLight::new(-Vec3A::Y, Vec3A::ONE, 1.0);
        assert_eq!(light.geom_id(), None);
        assert_eq!(light.pdf_at_point(Vec3A::ZERO, Vec3A::Y), 0.0);
    }

    #[test]
    fn find_by_geom_matches_by_id() {
        let mat = Arc::new(Emissive::new(Vec3A::splat(1.0)));
        let mut lights = LightList::new();
        lights.add(Arc::new(AreaLight::new(
            Box::new(SphereShape {
                center: Vec3A::ZERO,
                radius: 1.0,
            }),
            mat,
            7,
        )));

        assert!(lights.find_by_geom(7).is_some());
        assert!(lights.find_by_geom(8).is_none());
    }
}
