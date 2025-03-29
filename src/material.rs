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

    fn scatter_importance(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
    ) -> Option<(Ray, Color, f32)> {
        // Default fallback for materials that don't support importance sampling
        let mut attenuation = Color::default();
        let mut scattered = Ray::default();
        if self.scatter(r_in, rec, &mut attenuation, &mut scattered) {
            let cosine = f32::max(vec3::dot(rec.normal, vec3::unit_vector(scattered.direction())), 0.0);
            let pdf = 1.0; // uniform sampling (fake)
            return Some((scattered, attenuation * cosine, pdf));
        }
        None
    }
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
        Self {
            albedo,
            roughness: roughness.clamp(0.05, 1.0), // Clamp to avoid degenerate values
            metallic: metallic.clamp(0.0, 1.0),
        }
    }
    fn fresnel_schlick(cos_theta: f32, f0: Color) -> Color {
        f0 + (Color::new(1.0, 1.0, 1.0) - f0) * f32::powf(1.0 - cos_theta, 5.0)
    }

    // GGX sample (based on spherical coordinates)
    fn sample_ggx(normal: vec3::Vec3, roughness: f32) -> vec3::Vec3 {
        let u1 = common::random();
        let u2 = common::random();

        let a = roughness * roughness;

        let theta = f32::acos(f32::sqrt((1.0 - u1) / (1.0 + (a * a - 1.0) * u1)));
        let phi = 2.0 * std::f32::consts::PI * u2;

        let sin_theta = f32::sin(theta);
        let x = sin_theta * f32::cos(phi);
        let y = sin_theta * f32::sin(phi);
        let z = f32::cos(theta);

        let h_local = vec3::Vec3::new(x, y, z);
        vec3::align_to_normal(h_local, normal)
    }

    fn pdf_ggx(normal: vec3::Vec3, h: vec3::Vec3, roughness: f32) -> f32 {
        let a = roughness * roughness;
        let a2 = a * a;
        let n_dot_h = f32::max(vec3::dot(normal, h), 0.0);
        let denom = n_dot_h * n_dot_h * (a2 - 1.0) + 1.0;
        let d = a2 / (std::f32::consts::PI * denom * denom);
        d * n_dot_h / (4.0 * vec3::dot(h, vec3::unit_vector(h)).abs())
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
        // Keep this if you want compatibility with existing code
        // Or remove once you're fully using scatter_importance()
        let normal = rec.normal;
        let v = -vec3::unit_vector(r_in.direction());
        let mut reflect_dir = vec3::reflect(v, normal);
        reflect_dir += self.roughness * vec3::random_in_unit_sphere();

        if reflect_dir.near_zero() {
            reflect_dir = normal;
        }

        *scattered = Ray::new(rec.p, vec3::unit_vector(reflect_dir));
        let cos_theta = f32::max(vec3::dot(normal, v), 0.0);
        let f0 = Color::new(0.04, 0.04, 0.04).lerp(self.albedo, self.metallic);
        let fresnel = CookTorrance::fresnel_schlick(cos_theta, f0);
        let kd = (Color::new(1.0, 1.0, 1.0) - fresnel) * (1.0 - self.metallic);
        let diffuse = self.albedo / std::f32::consts::PI;
        *attenuation = kd * diffuse + fresnel;
        true
    }

    fn scatter_importance(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
    ) -> Option<(Ray, Color, f32)> {
        let n = rec.normal;
        let v = -vec3::unit_vector(r_in.direction());

        let h = Self::sample_ggx(n, self.roughness);
        let l = vec3::reflect(-v, h);

        if vec3::dot(l, n) <= 0.0 {
            return None;
        }

        let f0 = Color::new(0.04, 0.04, 0.04).lerp(self.albedo, self.metallic);
        let fresnel = Self::fresnel_schlick(f32::max(vec3::dot(h, v), 0.0), f0);
        let kd = (Color::new(1.0, 1.0, 1.0) - fresnel) * (1.0 - self.metallic);
        let diffuse = self.albedo / std::f32::consts::PI;
        let attenuation = kd * diffuse + fresnel;

        let scattered = Ray::new(rec.p, l);
        let pdf = Self::pdf_ggx(n, h, self.roughness).max(0.001);

        Some((scattered, attenuation, pdf))
    }
}
