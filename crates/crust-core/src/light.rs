use crate::material::{Emissive, Material};
use glam::Vec3A;
use sampler::Sampler;
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

/// The `Light` trait is what the integrator's light-sampling strategy (NEE)
/// needs from a light: a surface point to aim a shadow ray at, the
/// solid-angle density of that choice for MIS, the emitted radiance, and —
/// for lights with scene geometry — the material identity that lets a
/// bounce ray recognize the light it hit.
pub trait Light: Send + Sync {
    /// Samples a point on the light source, uniform by area.
    ///
    /// # Parameters
    /// - `u`, `v`: Unit random numbers driving the sample.
    fn sample_point(&self, u: f32, v: f32) -> Vec3A;

    /// Solid-angle pdf, as seen from `hit_point`, of `sample_point` having
    /// produced `light_point` (which must lie on the light's surface).
    fn pdf(&self, hit_point: Vec3A, light_point: Vec3A) -> f32;

    /// Radiance emitted by the light surface. Matches `emitted()` of the
    /// material bound to the light's scene geometry.
    fn emission(&self) -> Vec3A;

    /// The emissive material bound to this light's scene geometry, used to
    /// recognize the light when a bounce ray hits it (by address identity).
    /// `None` for lights with no geometry in the world.
    fn material(&self) -> Option<&dyn Material>;
}

/// A geometric area light: any [`LightShape`] paired with the [`Emissive`]
/// material its scene geometry carries (Cornell-box semantics — the same
/// surface is both light and visible object).
pub struct AreaLight {
    shape: Box<dyn LightShape>,
    material: Arc<Emissive>,
}

impl AreaLight {
    pub fn new(shape: Box<dyn LightShape>, material: Arc<Emissive>) -> Self {
        Self { shape, material }
    }
}

impl Light for AreaLight {
    fn sample_point(&self, u: f32, v: f32) -> Vec3A {
        self.shape.sample_point(u, v)
    }

    fn pdf(&self, hit_point: Vec3A, light_point: Vec3A) -> f32 {
        let direction = light_point - hit_point;
        let distance_squared = direction.length_squared();
        let dir_to_light = direction.normalize();
        // Solid-angle pdf of sampling this point uniformly by area:
        // dist^2 / (cos(theta_light) * area), where theta_light is the angle
        // between the light's surface normal at `light_point` and the
        // direction back toward the shaded point. Back-facing points clamp
        // the cosine to zero, so their pdf explodes and both MIS strategies
        // agree the contribution is negligible — area lights are effectively
        // one-sided.
        let light_normal = self.shape.normal_at(light_point);
        let cosine = f32::max(light_normal.dot(-dir_to_light), 0.0);
        distance_squared / (cosine * self.shape.area() + 1e-4)
    }

    fn emission(&self) -> Vec3A {
        self.material.emitted()
    }

    fn material(&self) -> Option<&dyn Material> {
        Some(self.material.as_ref())
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

    /// Randomly samples a light source from the `LightList` using the given
    /// sampler for the uniform pick.
    ///
    /// # Returns
    /// - `Some(&Arc<dyn Light>)` if the list is not empty.
    /// - `None` if the list is empty.
    pub fn sample(&self, sampler: &mut dyn Sampler) -> Option<&Arc<dyn Light>> {
        if self.lights.is_empty() {
            None
        } else {
            let i = (sampler.next_1d() * self.lights.len() as f32) as usize;
            // Guard against `next_1d() == 1.0 - epsilon` rounding to len.
            let i = i.min(self.lights.len() - 1);
            self.lights.get(i)
        }
    }

    /// Finds the light whose scene geometry carries `mat`, by address
    /// identity of the material. Used by the integrator to attribute a
    /// bounce-hit emissive surface to its light for MIS; emissive geometry
    /// with no light-list entry returns `None`.
    pub fn find_by_material(&self, mat: &dyn Material) -> Option<&Arc<dyn Light>> {
        self.lights.iter().find(|l| {
            l.material()
                .is_some_and(|m| std::ptr::addr_eq(m as *const dyn Material, mat))
        })
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
        );
        // Nearest point on the sphere as seen from below.
        let pdf = light.pdf(Vec3A::ZERO, Vec3A::new(0.0, 4.0, 0.0));
        assert!(pdf.is_finite() && pdf > 0.0);
        assert_eq!(light.emission(), Vec3A::splat(10.0));
    }

    #[test]
    fn find_by_material_matches_by_address() {
        let mat_a = Arc::new(Emissive::new(Vec3A::splat(1.0)));
        let mat_b = Arc::new(Emissive::new(Vec3A::splat(1.0)));
        let mut lights = LightList::new();
        lights.add(Arc::new(AreaLight::new(
            Box::new(SphereShape {
                center: Vec3A::ZERO,
                radius: 1.0,
            }),
            mat_a.clone(),
        )));

        assert!(lights.find_by_material(mat_a.as_ref()).is_some());
        // Equal color but a different allocation must not match.
        assert!(lights.find_by_material(mat_b.as_ref()).is_none());
    }
}
