use wide::f32x8;
use crate::vec3::Vec3;
use std::ops::{Add, Sub, Mul, Div};

#[derive(Clone, Copy, Debug)]
pub struct Vec3x8 {
    pub x: f32x8,
    pub y: f32x8,
    pub z: f32x8,
}

impl Vec3x8 {
    pub fn new(x: f32x8, y: f32x8, z: f32x8) -> Self {
        Vec3x8 { x, y, z }
    }

    pub fn splat(v: Vec3) -> Self {
        Self {
            x: f32x8::splat(v.x()),
            y: f32x8::splat(v.y()),
            z: f32x8::splat(v.z()),
        }
    }

    pub fn zero() -> Self {
        let zero = f32x8::splat(0.0);
        Self::new(zero, zero, zero)
    }

    pub fn length_squared(&self) -> f32x8 {
        self.x * self.x + self.y * self.y + self.z * self.z
    }

    pub fn dot(a: Self, b: Self) -> f32x8 {
        a.x * b.x + a.y * b.y + a.z * b.z
    }

    pub fn reflect(v: Self, n: Self) -> Self {
        v - n * Self::dot(v, n) * f32x8::splat(2.0)
    }

    pub fn normalize(self) -> Self {
        let inv_len = self.length_squared().sqrt().recip();
        Self {
            x: self.x * inv_len,
            y: self.y * inv_len,
            z: self.z * inv_len,
        }
    }
}

impl Add for Vec3x8 {
    type Output = Vec3x8;

    fn add(self, rhs: Vec3x8) -> Vec3x8 {
        Vec3x8 {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
            z: self.z + rhs.z,
        }
    }
}

impl Sub for Vec3x8 {
    type Output = Vec3x8;

    fn sub(self, rhs: Vec3x8) -> Vec3x8 {
        Vec3x8 {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
            z: self.z - rhs.z,
        }
    }
}

impl Mul<f32x8> for Vec3x8 {
    type Output = Vec3x8;

    fn mul(self, rhs: f32x8) -> Vec3x8 {
        Vec3x8 {
            x: self.x * rhs,
            y: self.y * rhs,
            z: self.z * rhs,
        }
    }
}

impl Mul<Vec3x8> for f32x8 {
    type Output = Vec3x8;

    fn mul(self, rhs: Vec3x8) -> Vec3x8 {
        rhs * self
    }
}

impl Div<f32x8> for Vec3x8 {
    type Output = Vec3x8;

    fn div(self, rhs: f32x8) -> Vec3x8 {
        Vec3x8 {
            x: self.x / rhs,
            y: self.y / rhs,
            z: self.z / rhs,
        }
    }
}

pub fn dot_vec3x8(a: Vec3x8, b: Vec3x8) -> f32x8 {
    a.x * b.x + a.y * b.y + a.z * b.z
}

pub fn unit_vector_x8(v: Vec3x8) -> Vec3x8 {
    let length_squared = v.x * v.x + v.y * v.y + v.z * v.z;
    let inv_length = length_squared.rsqrt(); // Or 1.0 / sqrt
    Vec3x8 {
        x: v.x * inv_length,
        y: v.y * inv_length,
        z: v.z * inv_length,
    }
}
