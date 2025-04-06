use crate::{Point3, Vec3};
use std::ops::Mul;
#[derive(Debug, Clone, Copy)]
pub struct Mat4 {
    // 4x4 matrix as flat array or rows
    pub data: [[f32; 4]; 4],
}

impl Mat4 {
    pub fn identity() -> Self {
        Mat4 {
            data: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    pub fn translate(x: f32, y: f32, z: f32) -> Self {
        let mut m = Self::identity();
        m.data[0][3] = x;
        m.data[1][3] = y;
        m.data[2][3] = z;
        m
    }

    pub fn scale(s: f32) -> Self {
        let mut m = Self::identity();
        m.data[0][0] = s;
        m.data[1][1] = s;
        m.data[2][2] = s;
        m
    }

    pub fn transform_point(&self, p: Point3) -> Point3 {
        let x = self.data[0][0] * p.x()
            + self.data[0][1] * p.y()
            + self.data[0][2] * p.z()
            + self.data[0][3];
        let y = self.data[1][0] * p.x()
            + self.data[1][1] * p.y()
            + self.data[1][2] * p.z()
            + self.data[1][3];
        let z = self.data[2][0] * p.x()
            + self.data[2][1] * p.y()
            + self.data[2][2] * p.z()
            + self.data[2][3];
        Point3::new(x, y, z)
    }

    pub fn transform_direction(&self, v: Vec3) -> Vec3 {
        let x = self.data[0][0] * v.x() + self.data[0][1] * v.y() + self.data[0][2] * v.z();
        let y = self.data[1][0] * v.x() + self.data[1][1] * v.y() + self.data[1][2] * v.z();
        let z = self.data[2][0] * v.x() + self.data[2][1] * v.y() + self.data[2][2] * v.z();
        Vec3::new(x, y, z)
    }
    pub fn inverse(&self) -> Self {
        let mut inv = Self::identity();
        inv.data[0][0] = 1.0 / self.data[0][0]; // inverse scale
        inv.data[1][1] = 1.0 / self.data[1][1];
        inv.data[2][2] = 1.0 / self.data[2][2];

        inv.data[0][3] = -self.data[0][3] * inv.data[0][0];
        inv.data[1][3] = -self.data[1][3] * inv.data[1][1];
        inv.data[2][3] = -self.data[2][3] * inv.data[2][2];

        inv
    }
}

impl Mul for Mat4 {
    type Output = Self;

    fn mul(self, rhs: Self) -> Self::Output {
        let mut result = Mat4::identity();

        for i in 0..4 {
            for j in 0..4 {
                result.data[i][j] = (0..4).map(|k| self.data[i][k] * rhs.data[k][j]).sum();
            }
        }

        result
    }
}
