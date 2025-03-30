use crate::color::Color;
use crate::common;
use crate::vec3;
use crate::vec3::Vec3;
use std::f32::consts::PI;

pub fn fresnel_schlick(cos_theta: f32, f0: Color) -> Color {
    f0 + (Color::new(1.0, 1.0, 1.0) - f0) * f32::powf(1.0 - cos_theta, 5.0)
}

pub fn geometry_schlick_ggx(n_dot: f32, roughness: f32) -> f32 {
    let k = (roughness + 1.0).powi(2) / 8.0;
    n_dot / (n_dot * (1.0 - k) + k)
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

pub fn schlick_weight(cos_theta: f32) -> f32 {
    (1.0 - cos_theta).powf(5.0)
}

// Disney Diffuse
pub fn disney_diffuse(base_color: Color, roughness: f32, n: Vec3, v: Vec3, l: Vec3, h: Vec3) -> Color {
    let n_dot_l = vec3::dot(n, l).max(0.0);
    let n_dot_v = vec3::dot(n, v).max(0.0);
    let l_dot_h = vec3::dot(l, h).max(0.0);

    let fd90 = 0.5 + 2.0 * l_dot_h * l_dot_h * roughness;
    let light_scatter = schlick_weight(n_dot_l);
    let view_scatter = schlick_weight(n_dot_v);

    base_color * (1.0 / PI) * (1.0 + (fd90 - 1.0) * light_scatter) * (1.0 + (fd90 - 1.0) * view_scatter)
}

// GTR1 distribution for clearcoat
pub fn gtr1(n_dot_h: f32, alpha: f32) -> f32 {
    let a2 = alpha * alpha;
    let denom = PI * ((n_dot_h * n_dot_h * (a2 - 1.0) + 1.0).powi(2));
    (a2 - 1.0) / denom.max(1e-4)
}

// Clearcoat Fresnel approx
pub fn fresnel_schlick_scalar(cos_theta: f32, f0: f32) -> f32 {
    f0 + (1.0 - f0) * (1.0 - cos_theta).powf(5.0)
}