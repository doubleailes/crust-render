use crate::color::Color;
use crate::common;
use crate::hittable::HitRecord;
use crate::material::Material;
use crate::ray::Ray;
use crate::vec3;
use crate::vec3::Vec3;

pub struct CookTorrance {
    pub albedo: Color,
    pub roughness: f32,
    pub metallic: f32,
}

impl CookTorrance {
    pub fn new(albedo: Color, roughness: f32, metallic: f32) -> Self {
        Self {
            albedo,
            roughness: roughness.clamp(0.05, 1.0),
            metallic: metallic.clamp(0.0, 1.0),
        }
    }

    fn fresnel_schlick(cos_theta: f32, f0: Color) -> Color {
        f0 + (Color::new(1.0, 1.0, 1.0) - f0) * f32::powf(1.0 - cos_theta, 5.0)
    }

    fn geometry_schlick_ggx(n_dot: f32, roughness: f32) -> f32 {
        let k = (roughness + 1.0).powi(2) / 8.0;
        n_dot / (n_dot * (1.0 - k) + k)
    }

    // GGX sample (based on spherical coordinates)
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
        let n = rec.normal;
        let v = -vec3::unit_vector(r_in.direction());

        // Sample a halfway vector using VNDF
        let h = sample_vndf_ggx(v, self.roughness);
        let l = vec3::reflect(-v, h);
        if vec3::dot(l, n) <= 0.0 {
            return false;
        }

        let n_dot_v = vec3::dot(n, v).max(1e-4);
        let n_dot_l = vec3::dot(n, l).max(1e-4);
        let n_dot_h = vec3::dot(n, h).max(1e-4);
        let v_dot_h = vec3::dot(v, h).max(1e-4);

        let f0 = Color::new(0.04, 0.04, 0.04).lerp(self.albedo, self.metallic);
        let f = CookTorrance::fresnel_schlick(v_dot_h, f0);

        let a = self.roughness * self.roughness;
        let a2 = a * a;
        let denom = (n_dot_h * n_dot_h * (a2 - 1.0) + 1.0).powi(2);
        let d = a2 / (std::f32::consts::PI * denom);

        let g = CookTorrance::geometry_schlick_ggx(n_dot_v, self.roughness)
            * CookTorrance::geometry_schlick_ggx(n_dot_l, self.roughness);
        let specular = (f * d * g) / (4.0 * n_dot_v * n_dot_l + 1e-4);
        let kd = (Color::new(1.0, 1.0, 1.0) - f) * (1.0 - self.metallic);
        let diffuse = self.albedo / std::f32::consts::PI;

        *attenuation = kd * diffuse + specular;
        *scattered = Ray::new(rec.p, l);

        true
    }

    fn scatter_importance(&self, r_in: &Ray, rec: &HitRecord) -> Option<(Ray, Color, f32)> {
        let n = rec.normal;
        let v = -vec3::unit_vector(r_in.direction());

        let sample_specular = common::random() < 0.5;

        let (l, pdf_specular, pdf_diffuse, brdf) = if sample_specular {
            // === Sample GGX specular ===
            let h = sample_vndf_ggx(v, self.roughness);
            let l = vec3::reflect(-v, h);
            if vec3::dot(l, n) <= 0.0 {
                return None;
            }

            // PDFs
            let pdf_ggx = pdf_vndf_ggx(v, h, n, self.roughness);
            let cosine = vec3::dot(n, l).max(1e-4);
            let pdf_cosine = cosine / std::f32::consts::PI;

            // Fresnel term
            let f0 = Color::new(0.04, 0.04, 0.04).lerp(self.albedo, self.metallic);
            let f = Self::fresnel_schlick(vec3::dot(v, h), f0);

            // NDF
            let a = self.roughness * self.roughness;
            let a2 = a * a;
            let n_dot_h = vec3::dot(n, h).max(1e-4);
            let denom = (n_dot_h * n_dot_h * (a2 - 1.0) + 1.0).powi(2);
            let d = a2 / (std::f32::consts::PI * denom);

            // Geometry term
            fn geometry_schlick_ggx(n_dot: f32, roughness: f32) -> f32 {
                let k = (roughness + 1.0).powi(2) / 8.0;
                n_dot / (n_dot * (1.0 - k) + k)
            }
            let g = geometry_schlick_ggx(vec3::dot(n, v), self.roughness)
                * geometry_schlick_ggx(vec3::dot(n, l), self.roughness);

            let spec = (f * d * g) / (4.0 * vec3::dot(n, v) * vec3::dot(n, l) + 1e-4);

            let kd = (Color::new(1.0, 1.0, 1.0) - f) * (1.0 - self.metallic);
            let diffuse = self.albedo / std::f32::consts::PI;

            let brdf = kd * diffuse + spec;

            (l, pdf_ggx * 0.5, pdf_cosine * 0.5, brdf)
        } else {
            // === Sample cosine-weighted hemisphere (diffuse) ===
            let l_local = vec3::random_cosine_direction();
            let l = vec3::align_to_normal(l_local, n);
            if vec3::dot(l, n) <= 0.0 {
                return None;
            }

            let h = vec3::unit_vector(v + l);
            let pdf_cosine = vec3::dot(n, l).max(1e-4) / std::f32::consts::PI;
            let pdf_ggx = pdf_vndf_ggx(v, h, n, self.roughness);

            let f0 = Color::new(0.04, 0.04, 0.04).lerp(self.albedo, self.metallic);
            let f = CookTorrance::fresnel_schlick(vec3::dot(v, h), f0);

            let a = self.roughness * self.roughness;
            let a2 = a * a;
            let n_dot_h = vec3::dot(n, h).max(1e-4);
            let denom = (n_dot_h * n_dot_h * (a2 - 1.0) + 1.0).powi(2);
            let d = a2 / (std::f32::consts::PI * denom);

            let g = Self::geometry_schlick_ggx(vec3::dot(n, v), self.roughness)
                * Self::geometry_schlick_ggx(vec3::dot(n, l), self.roughness);

            let spec = (f * d * g) / (4.0 * vec3::dot(n, v) * vec3::dot(n, l) + 1e-4);
            let kd = (Color::new(1.0, 1.0, 1.0) - f) * (1.0 - self.metallic);
            let diffuse = self.albedo / std::f32::consts::PI;

            let brdf = kd * diffuse + spec;

            (l, pdf_ggx * 0.5, pdf_cosine * 0.5, brdf)
        };

        let weight = common::balance_heuristic(pdf_specular, pdf_diffuse);
        let final_pdf = pdf_specular + pdf_diffuse;
        let n_dot_l = vec3::dot(n, l).max(1e-4);

        let scattered = Ray::new(rec.p, l);
        Some((scattered, brdf * n_dot_l * weight, final_pdf.max(1e-4)))
    }
}

pub fn sample_vndf_ggx(view: Vec3, roughness: f32) -> Vec3 {
    // Transform view direction to hemisphere aligned with normal (Z+)
    let v = vec3::unit_vector(Vec3::new(
        roughness * view.x(),
        roughness * view.y(),
        view.z(),
    ));

    // Generate 2D random numbers
    let (u1, u2) = common::random2();

    // Construct orthonormal basis
    let lensq = v.x() * v.x() + v.y() * v.y();
    let (t1, t2) = if lensq > 0.0 {
        let inv_len = 1.0 / lensq.sqrt();
        (
            Vec3::new(-v.y() * inv_len, v.x() * inv_len, 0.0),
            Vec3::new(
                -v.z() * v.x() * inv_len,
                -v.z() * v.y() * inv_len,
                lensq * inv_len,
            ),
        )
    } else {
        // view is aligned with z-axis
        (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0))
    };

    // Sample point on hemisphere
    let r = u1.sqrt();
    let phi = 2.0 * std::f32::consts::PI * u2;
    let t1_coeff = r * phi.cos();
    let t2_coeff = r * phi.sin();
    let _s = 0.5 * (1.0 + v.z());
    let t3 = (1.0 - u1).sqrt();

    let h = t1 * t1_coeff + t2 * t2_coeff + v * t3;
    vec3::unit_vector(Vec3::new(
        roughness * h.x(),
        roughness * h.y(),
        h.z().max(1e-6),
    ))
}

pub fn pdf_vndf_ggx(view: Vec3, half: Vec3, normal: Vec3, roughness: f32) -> f32 {
    let a2 = roughness * roughness;
    let n_dot_h = vec3::dot(normal, half).max(1e-6);
    let v_dot_h = vec3::dot(view, half).max(1e-6);

    let d = a2 / (std::f32::consts::PI * (n_dot_h * n_dot_h * (a2 - 1.0) + 1.0).powi(2));
    d * n_dot_h / (4.0 * v_dot_h)
}
