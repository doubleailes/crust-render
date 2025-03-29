use crate::color::Color;
use crate::hittable::HitRecord;
use crate::ray::Ray;
use crate::{common, vec3};

pub trait Material: Send + Sync {
    fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Color,
        scattered: &mut Ray,
    ) -> bool;
}

pub struct Lambertian {
    albedo: Color,
}

impl Lambertian {
    pub fn new(a: Color) -> Lambertian {
        Lambertian { albedo: a }
    }
}

impl Material for Lambertian {
    fn scatter(
        &self,
        _r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Color,
        scattered: &mut Ray,
    ) -> bool {
        let mut scatter_direction = rec.normal + vec3::random_unit_vector();

        // Catch degenerate scatter direction
        if scatter_direction.near_zero() {
            scatter_direction = rec.normal;
        }

        *attenuation = self.albedo;
        *scattered = Ray::new(rec.p, scatter_direction);
        true
    }
}

pub struct Metal {
    albedo: Color,
    fuzz: f32,
}

impl Metal {
    pub fn new(a: Color, f: f32) -> Metal {
        Metal {
            albedo: a,
            fuzz: if f < 1.0 { f } else { 1.0 },
        }
    }
}

impl Material for Metal {
    fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Color,
        scattered: &mut Ray,
    ) -> bool {
        let reflected = vec3::reflect(vec3::unit_vector(r_in.direction()), rec.normal);

        *attenuation = self.albedo;
        *scattered = Ray::new(rec.p, reflected + self.fuzz * vec3::random_in_unit_sphere());
        vec3::dot(scattered.direction(), rec.normal) > 0.0
    }
}

pub struct Dielectric {
    ir: f32, // Index of refraction
}

impl Dielectric {
    pub fn new(index_of_refraction: f32) -> Dielectric {
        Dielectric {
            ir: index_of_refraction,
        }
    }

    fn reflectance(cosine: f32, ref_idx: f32) -> f32 {
        // Use Schlick's approximation for reflectance
        let mut r0 = (1.0 - ref_idx) / (1.0 + ref_idx);
        r0 = r0 * r0;
        r0 + (1.0 - r0) * f32::powf(1.0 - cosine, 5.0)
    }
}

impl Material for Dielectric {
    fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Color,
        scattered: &mut Ray,
    ) -> bool {
        let refraction_ratio = if rec.front_face {
            1.0 / self.ir
        } else {
            self.ir
        };

        let unit_direction = vec3::unit_vector(r_in.direction());
        let cos_theta = f32::min(vec3::dot(-unit_direction, rec.normal), 1.0);
        let sin_theta = f32::sqrt(1.0 - cos_theta * cos_theta);

        let cannot_refract = refraction_ratio * sin_theta > 1.0;
        let direction = if cannot_refract
            || Self::reflectance(cos_theta, refraction_ratio) > common::random()
        {
            vec3::reflect(unit_direction, rec.normal)
        } else {
            vec3::refract(unit_direction, rec.normal, refraction_ratio)
        };

        *attenuation = Color::new(1.0, 1.0, 1.0);
        *scattered = Ray::new(rec.p, direction);
        true
    }
}

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

pub struct CookTorrance {
    pub albedo: Color,
    pub roughness: f32,
    pub metallic: f32,
}

impl CookTorrance {
    pub fn new(albedo: Color, roughness: f32, metallic: f32) -> Self {
        CookTorrance {
            albedo,
            roughness: roughness.clamp(0.05, 1.0),
            metallic,
        }
    }

    fn fresnel_schlick(cos_theta: f32, f0: Color) -> Color {
        f0 + (Color::new(1.0, 1.0, 1.0) - f0) * f32::powf(1.0 - cos_theta, 5.0)
    }

    fn distribution_ggx(n: vec3::Vec3, h: vec3::Vec3, roughness: f32) -> f32 {
        let a = roughness * roughness;
        let a2 = a * a;
        let n_dot_h = f32::max(vec3::dot(n, h), 0.0);
        let n_dot_h2 = n_dot_h * n_dot_h;

        let denom = n_dot_h2 * (a2 - 1.0) + 1.0;
        a2 / (std::f32::consts::PI * denom * denom)
    }

    fn geometry_smith(n: vec3::Vec3, v: vec3::Vec3, l: vec3::Vec3, roughness: f32) -> f32 {
        fn geometry_schlick_ggx(n_dot_v: f32, roughness: f32) -> f32 {
            let r = roughness + 1.0;
            let k = (r * r) / 8.0;
            n_dot_v / (n_dot_v * (1.0 - k) + k)
        }

        let n_dot_v = f32::max(vec3::dot(n, v), 0.0);
        let n_dot_l = f32::max(vec3::dot(n, l), 0.0);
        geometry_schlick_ggx(n_dot_v, roughness) * geometry_schlick_ggx(n_dot_l, roughness)
    }
}

impl Material for CookTorrance {
    fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Color,
        scattered: &mut Ray,
    ) -> bool {
        let normal = rec.normal;
        let v = -vec3::unit_vector(r_in.direction());

        // Sample halfway vector for microfacet reflection
        let mut reflect_dir = vec3::reflect(v, normal);
        
        // Add roughness via GGX-style fuzz (approximated)
        reflect_dir += self.roughness * vec3::random_in_unit_sphere();

        if reflect_dir.near_zero() {
            reflect_dir = normal;
        }

        *scattered = Ray::new(rec.p, vec3::unit_vector(reflect_dir));

        // Approximate Fresnel reflectance
        let cos_theta = f32::max(vec3::dot(normal, v), 0.0);
        let f0 = Color::new(0.04, 0.04, 0.04).lerp(self.albedo, self.metallic);
        let fresnel = CookTorrance::fresnel_schlick(cos_theta, f0);

        let kd = Color::new(1.0, 1.0, 1.0) - fresnel;
        let kd = kd * (1.0 - self.metallic);

        let diffuse = self.albedo / std::f32::consts::PI;

        *attenuation = kd * diffuse + fresnel;

        true
    }
}
