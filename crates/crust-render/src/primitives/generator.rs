use crate::MaterialType;
use crate::document::DocObject;
use crate::primitives::Primitive;

use std::f32::consts::PI;
use utils::Vec3;

type Triangles = (
    Vec<Vec3>,
    Vec<Vec3>,
    Vec<(f32, f32)>,
    Vec<(usize, usize, usize)>,
);

trait Triangulable {
    fn generate(&self) -> Triangles;
    fn triangulate(&self, material: MaterialType) -> Vec<DocObject> {
        let (vertices, _normals, _uvs, indices) = self.generate();
        let mut objects = Vec::with_capacity(indices.len());

        for (i0, i1, i2) in indices {
            let v0 = vertices[i0];
            let v1 = vertices[i1];
            let v2 = vertices[i2];

            let prim = Primitive::new_triangle(v0, v1, v2);
            objects.push(DocObject::new(
                format!("uv_sphere_{}_{}_{}", 20, 20, i0),
                prim,
                material.clone(),
            ));
        }
        objects
    }
}

pub struct UVSphere {
    radius: f32,
    stacks: usize,
    sectors: usize,
}
impl Triangulable for UVSphere {
    fn generate(&self) -> Triangles {
        let mut vertices = Vec::new();
        let mut normals = Vec::new();
        let mut uvs = Vec::new();
        let mut indices = Vec::new();

        for i in 0..=self.stacks {
            let stack_angle = PI / 2.0 - i as f32 * PI / self.stacks as f32; // from +pi/2 to -pi/2
            let xy = self.radius * stack_angle.cos();
            let z = self.radius * stack_angle.sin();

            for j in 0..=self.sectors {
                let sector_angle = j as f32 * 2.0 * PI / self.sectors as f32; // from 0 to 2pi

                let x = xy * sector_angle.cos();
                let y = xy * sector_angle.sin();
                let position = Vec3::new(x, y, z);
                vertices.push(position);
                normals.push(position.unit_vector());

                let u = j as f32 / self.sectors as f32;
                let v = i as f32 / self.stacks as f32;
                uvs.push((u, v));
            }
        }

        // Indexing
        for i in 0..self.stacks {
            for j in 0..self.sectors {
                let first = i * (self.sectors + 1) + j;
                let second = first + self.sectors + 1;

                indices.push((first, second, first + 1));
                indices.push((first + 1, second, second + 1));
            }
        }

        (vertices, normals, uvs, indices)
    }
}
impl UVSphere {
    pub fn new(radius: f32, stacks: usize, sectors: usize) -> Self {
        UVSphere {
            radius,
            stacks,
            sectors,
        }
    }
    pub fn get_doc_object(&self, material: MaterialType) -> Vec<DocObject> {
        self.triangulate(material)
    }
}
pub struct UVTorus {
    pub position: Vec3,
    pub major_radius: f32,
    pub minor_radius: f32,
    pub segments: usize,
    pub sides: usize,
}
impl Triangulable for UVTorus {
    fn generate(&self) -> Triangles {
        let mut vertices = Vec::new();
        let mut normals = Vec::new();
        let mut uvs = Vec::new();
        let mut indices = Vec::new();

        for i in 0..=self.segments {
            let seg_angle = i as f32 / self.segments as f32 * 2.0 * PI;
            let cos_seg = seg_angle.cos();
            let sin_seg = seg_angle.sin();

            for j in 0..=self.sides {
                let side_angle = j as f32 / self.sides as f32 * 2.0 * PI;
                let cos_side = side_angle.cos();
                let sin_side = side_angle.sin();

                let x = (self.major_radius + self.minor_radius * cos_side) * cos_seg;
                let y = (self.major_radius + self.minor_radius * cos_side) * sin_seg;
                let z = self.minor_radius * sin_side;

                let position = Vec3::new(x, y, z) + self.position;
                vertices.push(position);

                let cx = self.major_radius * cos_seg + self.position.x();
                let cy = self.major_radius * sin_seg + self.position.y();
                let center = Vec3::new(cx, cy, self.position.z());
                normals.push((position - center).unit_vector());

                let u = i as f32 / self.segments as f32;
                let v = j as f32 / self.sides as f32;
                uvs.push((u, v));
            }
        }

        for i in 0..self.segments {
            for j in 0..self.sides {
                let first = i * (self.sides + 1) + j;
                let second = first + self.sides + 1;

                indices.push((first, second, first + 1));
                indices.push((first + 1, second, second + 1));
            }
        }

        (vertices, normals, uvs, indices)
    }
}
impl UVTorus {
    pub fn new(
        position: Vec3,
        major_radius: f32,
        minor_radius: f32,
        segments: usize,
        sides: usize,
    ) -> Self {
        UVTorus {
            position,
            major_radius,
            minor_radius,
            segments,
            sides,
        }
    }
    pub fn get_doc_object(&self, material: MaterialType) -> Vec<DocObject> {
        self.triangulate(material)
    }
}
