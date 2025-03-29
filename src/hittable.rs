use crate::ray::Ray;
use crate::vec3::{self, Point3, Vec3};
use std::sync::Arc;
use wide::f32x8;
use crate::vec3x8::Vec3x8;

use crate::material::Material;

#[derive(Clone, Default)]
pub struct HitRecord {
    pub p: Point3,
    pub normal: Vec3,
    pub mat: Option<Arc<dyn Material>>,
    pub t: f32,
    pub front_face: bool,
}

impl HitRecord {
    pub fn new() -> HitRecord {
        Default::default()
    }
    pub fn set_face_normal(&mut self, r: &Ray, outward_normal: Vec3) {
        self.front_face = vec3::dot(r.direction(), outward_normal) < 0.0;
        self.normal = if self.front_face {
            outward_normal
        } else {
            -outward_normal
        };
    }
}

pub trait Hittable: Send + Sync {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32, rec: &mut HitRecord) -> bool;
}

#[derive(Clone)]
pub struct HitRecordx8 {
    pub p: Vec3x8,
    pub normal: Vec3x8,
    pub t: f32x8,
    pub front_face: [bool; 8],
    pub material: [Option<Arc<dyn Material>>; 8],
}

impl HitRecordx8 {
    pub fn new() -> Self {
        Self {
            p: Vec3x8::zero(),
            normal: Vec3x8::zero(),
            t: f32x8::splat(f32::INFINITY),
            front_face: [true; 8],
            material: [None, None, None, None, None, None, None, None],
        }
    }

    pub fn set_face_normal(&mut self, ray_dir: &Vec3x8, outward_normal: &Vec3x8) {
        use crate::vec3::*;

        let dot = dot_vec3x8(*ray_dir, *outward_normal);
        for i in 0..8 {
            let is_front = dot.extract(i) < 0.0;
            self.front_face[i] = is_front;
            self.normal.x = self.normal.x.replace(i,
                if is_front { outward_normal.x.extract(i) } else { -outward_normal.x.extract(i) }
            );
            self.normal.y = self.normal.y.replace(i,
                if is_front { outward_normal.y.extract(i) } else { -outward_normal.y.extract(i) }
            );
            self.normal.z = self.normal.z.replace(i,
                if is_front { outward_normal.z.extract(i) } else { -outward_normal.z.extract(i) }
            );
        }
    }

    pub fn update_lane(&mut self, lane: usize, p: Vec3, normal: Vec3, t: f32, front: bool, mat: Arc<dyn Material>) {
        self.p.x = self.p.x.replace(lane, p.x());
        self.p.y = self.p.y.replace(lane, p.y());
        self.p.z = self.p.z.replace(lane, p.z());

        self.normal.x = self.normal.x.replace(lane, normal.x());
        self.normal.y = self.normal.y.replace(lane, normal.y());
        self.normal.z = self.normal.z.replace(lane, normal.z());

        self.t = self.t.replace(lane, t);
        self.front_face[lane] = front;
        self.material[lane] = Some(mat);
    }
}
