use crate::common;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter, Result};
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub};
use std::ops::{Index, IndexMut};

#[derive(Copy, Clone, Default, Deserialize, Serialize, Debug)]
pub struct Vec3 {
    e: [f32; 3],
}

impl Vec3 {
    pub fn new(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3 { e: [x, y, z] }
    }
    pub fn random() -> Vec3 {
        Vec3::new(common::random(), common::random(), common::random())
    }

    pub fn random_range(min: f32, max: f32) -> Vec3 {
        Vec3::new(
            common::random_range(min, max),
            common::random_range(min, max),
            common::random_range(min, max),
        )
    }

    pub fn x(&self) -> f32 {
        self.e[0]
    }

    pub fn y(&self) -> f32 {
        self.e[1]
    }

    pub fn z(&self) -> f32 {
        self.e[2]
    }
    // This the length of the vector
    pub fn length(&self) -> f32 {
        f32::sqrt(self.length_squared())
    }
    // This is the square of the length of the vector
    pub fn length_squared(&self) -> f32 {
        self.e[0] * self.e[0] + self.e[1] * self.e[1] + self.e[2] * self.e[2]
    }
    pub fn near_zero(&self) -> bool {
        const EPS: f32 = 1.0e-8;
        // Return true if the vector is close to zero in all dimensions
        self.e[0].abs() < EPS && self.e[1].abs() < EPS && self.e[2].abs() < EPS
    }
    // Linear interpolation between two Vec3s: self and other, by t ∈ [0,1]
    pub fn lerp(self, other: Vec3, t: f32) -> Vec3 {
        self * (1.0 - t) + other * t
    }
    pub fn clamp(self, min: f32, max: f32) -> Vec3 {
        Vec3::new(
            common::clamp(self.x(), min, max),
            common::clamp(self.y(), min, max),
            common::clamp(self.z(), min, max),
        )
    }
    pub fn unit_vector(self) -> Vec3 {
        self / self.length()
    }
    pub fn rotate(self, r_x: f32, r_y: f32, r_z: f32) -> Vec3 {
        let cos_x = f32::cos(r_x);
        let sin_x = f32::sin(r_x);
        let cos_y = f32::cos(r_y);
        let sin_y = f32::sin(r_y);
        let cos_z = f32::cos(r_z);
        let sin_z = f32::sin(r_z);

        Vec3::new(
            self.x() * (cos_y * cos_z) + self.y() * (cos_x * sin_z + sin_x * sin_y * cos_z)
                - self.z() * (sin_x * sin_z - cos_x * sin_y * cos_z),
            self.x() * (-cos_y * sin_z)
                + self.y() * (cos_x * cos_z - sin_x * sin_y * sin_z)
                + self.z() * (sin_x * cos_z + cos_x * sin_y * sin_z),
            self.x() * (sin_y) - self.y() * (sin_x * cos_y) + self.z() * (cos_x * cos_y),
        )
    }
}
impl From<[f32; 3]> for Vec3 {
    fn from(arr: [f32; 3]) -> Self {
        Vec3::new(arr[0], arr[1], arr[2])
    }
}
pub fn align_to_normal(local: Vec3, normal: Vec3) -> Vec3 {
    // Assume Z-up in local, rotate to match `normal`
    let up = if normal.z().abs() < 0.999 {
        Vec3::new(0.0, 0.0, 1.0)
    } else {
        Vec3::new(1.0, 0.0, 0.0)
    };

    let tangent = unit_vector(cross(up, normal));
    let bitangent = cross(normal, tangent);

    local.x() * tangent + local.y() * bitangent + local.z() * normal
}

// Type alias
pub type Point3 = Vec3;

// Output formatting
impl Display for Vec3 {
    fn fmt(&self, f: &mut Formatter) -> Result {
        write!(f, "{} {} {}", self.e[0], self.e[1], self.e[2])
    }
}

impl Index<usize> for Vec3 {
    type Output = f32;

    fn index(&self, i: usize) -> &Self::Output {
        &self.e[i]
    }
}

impl IndexMut<usize> for Vec3 {
    fn index_mut(&mut self, i: usize) -> &mut Self::Output {
        &mut self.e[i]
    }
}

// -Vec3
impl Neg for Vec3 {
    type Output = Vec3;

    fn neg(self) -> Vec3 {
        Vec3::new(-self.x(), -self.y(), -self.z())
    }
}

// Vec3 += Vec3
impl AddAssign for Vec3 {
    fn add_assign(&mut self, v: Vec3) {
        *self = *self + v;
    }
}

// Vec3 *= f32
impl MulAssign<f32> for Vec3 {
    fn mul_assign(&mut self, t: f32) {
        *self = *self * t;
    }
}

// Vec3 /= f32
impl DivAssign<f32> for Vec3 {
    fn div_assign(&mut self, t: f32) {
        *self = *self / t;
    }
}

// Vec3 + Vec3
impl Add for Vec3 {
    type Output = Vec3;

    fn add(self, v: Vec3) -> Vec3 {
        Vec3::new(self.x() + v.x(), self.y() + v.y(), self.z() + v.z())
    }
}

// Vec3 - Vec3
impl Sub for Vec3 {
    type Output = Vec3;

    fn sub(self, v: Vec3) -> Vec3 {
        Vec3::new(self.x() - v.x(), self.y() - v.y(), self.z() - v.z())
    }
}

// Vec3 * Vec3
impl Mul for Vec3 {
    type Output = Vec3;

    fn mul(self, v: Vec3) -> Vec3 {
        Vec3::new(self.x() * v.x(), self.y() * v.y(), self.z() * v.z())
    }
}

// f32 * Vec3
impl Mul<Vec3> for f32 {
    type Output = Vec3;

    fn mul(self, v: Vec3) -> Vec3 {
        Vec3::new(self * v.x(), self * v.y(), self * v.z())
    }
}

// Vec3 * f32
impl Mul<f32> for Vec3 {
    type Output = Vec3;

    fn mul(self, t: f32) -> Vec3 {
        Vec3::new(self.x() * t, self.y() * t, self.z() * t)
    }
}

// Vec3 / f32
impl Div<f32> for Vec3 {
    type Output = Vec3;

    fn div(self, t: f32) -> Vec3 {
        Vec3::new(self.x() / t, self.y() / t, self.z() / t)
    }
}

pub fn dot(u: Vec3, v: Vec3) -> f32 {
    u.e[0] * v.e[0] + u.e[1] * v.e[1] + u.e[2] * v.e[2]
}

pub fn cross(u: Vec3, v: Vec3) -> Vec3 {
    Vec3::new(
        u.e[1] * v.e[2] - u.e[2] * v.e[1],
        u.e[2] * v.e[0] - u.e[0] * v.e[2],
        u.e[0] * v.e[1] - u.e[1] * v.e[0],
    )
}

pub fn unit_vector(v: Vec3) -> Vec3 {
    v / v.length()
}

pub fn random_in_unit_sphere() -> Vec3 {
    loop {
        let p = Vec3::random_range(-1.0, 1.0);
        if p.length_squared() >= 1.0 {
            continue;
        }
        return p;
    }
}

pub fn random_unit_vector() -> Vec3 {
    unit_vector(random_in_unit_sphere())
}

pub fn random_in_unit_disk() -> Vec3 {
    loop {
        let p = Vec3::new(
            common::random_range(-1.0, 1.0),
            common::random_range(-1.0, 1.0),
            0.0,
        );
        if p.length_squared() >= 1.0 {
            continue;
        }
        return p;
    }
}

pub fn reflect(v: Vec3, n: Vec3) -> Vec3 {
    v - 2.0 * dot(v, n) * n
}

pub fn refract(uv: Vec3, n: Vec3, etai_over_etat: f32) -> Vec3 {
    let cos_theta = f32::min(dot(-uv, n), 1.0);
    let r_out_perp = etai_over_etat * (uv + cos_theta * n);
    let r_out_parallel = -f32::sqrt(f32::abs(1.0 - r_out_perp.length_squared())) * n;
    r_out_perp + r_out_parallel
}

pub fn random_cosine_direction() -> Vec3 {
    let r1 = common::random();
    let r2 = common::random();
    let z = f32::sqrt(1.0 - r2);

    let phi = 2.0 * std::f32::consts::PI * r1;
    let x = f32::cos(phi) * f32::sqrt(r2);
    let y = f32::sin(phi) * f32::sqrt(r2);

    Vec3::new(x, y, z)
}
