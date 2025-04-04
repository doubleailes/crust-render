use std::io::Write;

use crate::Material;
use crate::MaterialType;
use crate::camera::Camera;
use crate::hittable_list::HittableList;
use crate::light::{self, LightList};
use crate::primitives::{Object, Primitive};
use crate::tracer::RenderSettings;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tracing::error;
use tracing::warn;

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
                    let obj = Object::new_sphere(*center, *radius, material);
                    world.add(Box::new(obj));
                }
                Primitive::Triangle { v0, v1, v2 } => {
                    let obj = Object::new_triangle(*v0, *v1, *v2, material);
                    world.add(Box::new(obj));
                }
                Primitive::Mesh { vertices, indices } => {
                    let obj = Object::new_mesh(vertices.clone(), indices.clone(), material);
                    world.add(Box::new(obj));
                }
                Primitive::Obj { path } => {
                    let obj = Object::new_obj(path.clone(), material);
                    world.add(Box::new(obj));
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
