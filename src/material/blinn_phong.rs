use crate::color::Color;
use crate::hittable::HitRecord;
use crate::material::Material;
use crate::ray::Ray;
use crate::vec3;
pub struct BlinnPhong {
    pub diffuse: Color,
    pub specular: Color,
    pub shininess: f32,
    pub light_dir: vec3::Vec3, // Assume one directional light for now
}

impl BlinnPhong {
    pub fn new(diffuse: Color, specular: Color, shininess: f32, light_dir: vec3::Vec3) -> Self {
        BlinnPhong {
            diffuse,
            specular,
            shininess,
            light_dir: vec3::unit_vector(light_dir),
        }
    }
}

impl Material for BlinnPhong {
    fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Color,
        scattered: &mut Ray,
    ) -> bool {
        let normal = rec.normal;
        let view_dir = -vec3::unit_vector(r_in.direction());
        let light_dir = self.light_dir;

        let halfway = vec3::unit_vector(view_dir + light_dir);

        let diff = f32::max(vec3::dot(normal, light_dir), 0.0);
        let spec = f32::powf(f32::max(vec3::dot(normal, halfway), 0.0), self.shininess);

        let color = self.diffuse * diff + self.specular * spec;

        *attenuation = color;
        *scattered = Ray::new(rec.p, light_dir); // Optional: or bounce randomly for realism

        true
    }
}
