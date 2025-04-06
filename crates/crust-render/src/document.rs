use crate::Material;
use crate::MaterialType;
use crate::bvhnode::BVHNode;
use crate::camera::Camera;
use crate::hittable::Hittable;
use crate::hittable_list::HittableList;
use crate::instance::Instance;
use crate::light::{self, LightList};
use crate::scene_cache::GLOBAL_OBJ_CACHE;
use crate::tracer::RenderSettings;
use crate::{SmoothTriangle, Sphere, Triangle};
use obj::{Obj, load_obj};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::{fs::File, io::BufReader};
use tracing::{debug, error, warn};
use utils::{Mat4, Point3, Vec3};

#[derive(Debug, Deserialize, Serialize)]
pub struct Document {
    pub(crate) camera: Camera,
    pub(crate) object_list: ObjectList,
    pub(crate) settings: RenderSettings,
}

impl Document {
    pub fn new(camera: Camera, object_list: ObjectList, settings: RenderSettings) -> Self {
        Self {
            camera,
            object_list,
            settings,
        }
    }

    pub fn camera(&self) -> Camera {
        self.camera
    }

    pub fn object_list(&self) -> &ObjectList {
        &self.object_list
    }

    pub fn settings(&self) -> RenderSettings {
        self.settings
    }
    pub fn get_world(&self) -> (HittableList, LightList) {
        let mut world = HittableList::new();
        let mut lights = LightList::new();
        for object in &self.object_list.objects {
            let mat_type = object.material();
            let material: Arc<dyn Material> = mat_type.get_material();
            if mat_type.is_emissive() {
                let emissive = match mat_type.get_emissive() {
                    Some(emissive) => emissive,
                    None => {
                        warn!(
                            "Emissive material is missing emissive value for object: {}",
                            object.name
                        );
                        continue;
                    }
                };
                let light: Arc<dyn light::Light> = Arc::new(emissive.clone());
                lights.add(light);
            }
            match object.object() {
                Primitive::Sphere { center, radius } => {
                    let obj = Sphere::new(*center, *radius, material.clone());
                    world.add(Box::new(obj));
                }
                Primitive::Obj {
                    path,
                    transform,
                    smooth,
                } => {
                    let shared_bvh = load_obj_bvh(&path, material.clone(), *smooth);

                    world.add(Box::new(Instance {
                        object: shared_bvh,
                        transform: *transform,
                        inverse_transform: transform.inverse(),
                    }) as Box<dyn Hittable>);
                }
                Primitive::Alembic {
                    path,
                    transform,
                    sample,
                    smooth,
                } => {
                    let bvh = load_alembic_bvh(path, material.clone(), *sample as u32, *smooth);

                    world.add(Box::new(Instance {
                        object: bvh,
                        transform: *transform,
                        inverse_transform: transform.inverse(),
                    }) as Box<dyn Hittable>);
                }
            }
        }
        (world, lights)
    }
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut writer = std::io::BufWriter::new(file);
        let r = match ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default()) {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to serialize Document: {}", e);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Failed to serialize Document",
                ));
            }
        };
        writer.write_all(r.as_bytes())?;
        writer.flush()?;
        Ok(())
    }
    pub fn read(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);
        let doc: Document = match ron::de::from_reader(reader) {
            Ok(doc) => doc,
            Err(e) => {
                error!("Failed to deserialize Document: {}", e);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Failed to deserialize Document",
                ));
            }
        };
        Ok(doc)
    }
}
#[derive(Debug, Deserialize, Serialize)]
pub struct ObjectList {
    objects: Vec<DocObject>,
}
impl ObjectList {
    pub fn new(objects: Vec<DocObject>) -> Self {
        Self { objects }
    }

    pub fn add(&mut self, object: DocObject) {
        self.objects.push(object);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DocObject {
    name: String,
    object: Primitive,
    material: MaterialType,
}
impl DocObject {
    pub fn new(name: String, object: Primitive, material: MaterialType) -> Self {
        Self {
            name,
            object,
            material,
        }
    }

    pub fn object(&self) -> &Primitive {
        &self.object
    }

    pub fn material(&self) -> &MaterialType {
        &self.material
    }
}
#[derive(Debug, Deserialize, Serialize)]
pub enum Primitive {
    Sphere {
        center: Point3,
        radius: f32,
    },
    Obj {
        path: String,
        transform: Mat4,
        smooth: bool,
    },
    Alembic {
        path: String,
        transform: Mat4,
        sample: isize,
        smooth: bool,
    },
}
impl Primitive {
    pub fn new_sphere(center: Point3, radius: f32) -> Self {
        Self::Sphere { center, radius }
    }

    pub fn new_obj(path: String, transform: Mat4, smooth: bool) -> Self {
        Self::Obj {
            path,
            transform,
            smooth,
        }
    }
    pub fn new_alembic(path: String, transform: Mat4, sample: isize, smooth: bool) -> Self {
        Self::Alembic {
            path,
            transform,
            sample,
            smooth,
        }
    }
}

pub fn load_obj_bvh(path: &str, material: Arc<dyn Material>, smooth: bool) -> Arc<dyn Hittable> {
    {
        let cache = GLOBAL_OBJ_CACHE.read().unwrap();
        if let Some(bvh) = cache.get(path) {
            return bvh.clone();
        }
    }

    let file = File::open(path).expect("OBJ file not found");
    let input = BufReader::new(file);
    let obj: Obj = load_obj(input).expect("Failed to parse OBJ");

    let vertices: Vec<Point3> = obj.vertices.iter().map(|v| v.position.into()).collect();
    let normals: Vec<Vec3> = obj.vertices.iter().map(|n| n.normal.into()).collect();
    let indices: Vec<u32> = obj.indices.iter().map(|&i| i as u32).collect();

    let mut tris: Vec<Arc<dyn Hittable>> = Vec::with_capacity(indices.len() / 3);
    for i in (0..indices.len()).step_by(3) {
        let v0 = vertices[indices[i] as usize];
        let v1 = vertices[indices[i + 1] as usize];
        let v2 = vertices[indices[i + 2] as usize];

        let n0 = normals[indices[i] as usize];
        let n1 = normals[indices[i + 1] as usize];
        let n2 = normals[indices[i + 2] as usize];
        let tri: Arc<dyn Hittable> = match smooth {
            true => Arc::new(SmoothTriangle::new(
                v0,
                v1,
                v2,
                n0,
                n1,
                n2,
                material.clone(),
            )),
            false => Arc::new(Triangle::new(v0, v1, v2, material.clone())),
        };
        tris.push(tri);
    }

    let bvh = BVHNode::build(tris);

    let mut cache = GLOBAL_OBJ_CACHE.write().unwrap();
    cache.insert(path.to_string(), bvh.clone());

    bvh
}

pub fn load_alembic_bvh(
    path: &str,
    material: Arc<dyn Material>,
    sample: u32,
    smooth: bool,
) -> Arc<dyn Hittable> {
    use ogawa_rs::*;
    use std::fs::File;
    let cache_key = format!("{path}#{}", sample);

    // 1. Try cache first
    {
        let cache = GLOBAL_OBJ_CACHE.read().unwrap();
        if let Some(bvh) = cache.get(&cache_key) {
            return bvh.clone();
        }
    }

    let file = File::open(path).expect("Alembic file not found");
    let mut reader = MemMappedReader::new(file).expect("Failed to map Alembic file");
    let archive = Archive::new(&mut reader).expect("Invalid Alembic archive");

    let mut stack = vec![archive.load_root_object(&mut reader).unwrap()];
    let mut bvh_nodes: Vec<Arc<dyn Hittable>> = Vec::new();

    while let Some(current) = stack.pop() {
        let current_name: &str = current.header.full_name.as_str();
        debug!("Current object: {:?}", current_name);
        match Schema::parse(&current, &mut reader, &archive) {
            Ok(Schema::PolyMesh(mesh)) => {
                let vertices: Vec<Point3> = mesh
                    .load_vertices_sample(sample, &mut reader)
                    .unwrap()
                    .iter()
                    .map(|p| p.clone().into())
                    .collect();

                let face_indices = mesh.load_faceindices_sample(sample, &mut reader).unwrap();
                let _counts = mesh.load_facecounts_sample(sample, &mut reader).unwrap();

                // Optional normals
                let maybe_normals: Option<Vec<Vec3>> = if mesh.has_normals() {
                    match mesh.has_normals() {
                        true => {
                            let normals = mesh.load_normals_sample(sample, &mut reader).unwrap();
                            Some(normals.iter().map(|n| n.clone().into()).collect())
                        }
                        false => {
                            warn!("Normals present but failed to load sample.");
                            None
                        }
                    }
                } else {
                    None
                };

                let mut index_offset = 0;
                for &face_vertex_count in &_counts {
                    // Ignore degenerate polygons
                    if face_vertex_count < 3 {
                        index_offset += face_vertex_count as usize;
                        continue;
                    }

                    // Fan triangulation: (v0, vi, vi+1)
                    for i in 1..(face_vertex_count - 1) {
                        let i0 = face_indices[index_offset] as usize;
                        let i1 = face_indices[index_offset + i as usize] as usize;
                        let i2 = face_indices[index_offset + i as usize + 1] as usize;

                        let v0 = vertices[i0];
                        let v1 = vertices[i1];
                        let v2 = vertices[i2];

                        let tri: Arc<dyn Hittable> = match (&maybe_normals, smooth) {
                            (Some(normals), true) => {
                                let n0 = normals[i0];
                                let n1 = normals[i1];
                                let n2 = normals[i2];
                                Arc::new(SmoothTriangle::new(
                                    v0,
                                    v1,
                                    v2,
                                    n0,
                                    n1,
                                    n2,
                                    material.clone(),
                                ))
                            }
                            _ => Arc::new(Triangle::new(v0, v1, v2, material.clone())),
                        };

                        bvh_nodes.push(tri);
                    }

                    index_offset += face_vertex_count as usize;
                }
            }
            _ => {
                for i in (0..current.child_count()).rev() {
                    let child = current
                        .load_child(
                            i,
                            &mut reader,
                            &archive.indexed_meta_data,
                            &archive.time_samplings,
                        )
                        .unwrap();
                    stack.push(child);
                }
            }
        }
    }

    // 2. Cache result
    let bvh = BVHNode::build(bvh_nodes);
    let mut cache = GLOBAL_OBJ_CACHE.write().unwrap();
    cache.insert(cache_key.to_string(), bvh.clone());

    bvh
}
