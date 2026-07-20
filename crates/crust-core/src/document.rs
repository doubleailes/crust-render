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
use glam::{Mat4, Vec3A};
use obj::{Obj, load_obj};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::{fs::File, io::BufReader};
use tracing::{debug, error, warn};

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
                    let shared_bvh = load_obj_bvh(path, material.clone(), *smooth);

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
                Primitive::Usd {
                    path,
                    transform,
                    prim_path,
                } => {
                    #[cfg(feature = "usd")]
                    {
                        let bvh = load_usd_bvh(path, material.clone(), prim_path.as_deref());
                        world.add(Box::new(Instance {
                            object: bvh,
                            transform: *transform,
                            inverse_transform: transform.inverse(),
                        }) as Box<dyn Hittable>);
                    }
                    #[cfg(not(feature = "usd"))]
                    {
                        let _ = (transform, prim_path);
                        error!(
                            "Primitive::Usd '{}' requires building with `--features usd` (openusd)",
                            path
                        );
                    }
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
        center: Vec3A,
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
    Usd {
        path: String,
        transform: Mat4,
        #[serde(default)]
        prim_path: Option<String>,
    },
}
impl Primitive {
    pub fn new_sphere(center: Vec3A, radius: f32) -> Self {
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
    pub fn new_usd(path: String, transform: Mat4, prim_path: Option<String>) -> Self {
        Self::Usd {
            path,
            transform,
            prim_path,
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

    let vertices: Vec<Vec3A> = obj.vertices.iter().map(|v| v.position.into()).collect();
    let normals: Vec<Vec3A> = obj.vertices.iter().map(|n| n.normal.into()).collect();
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
                let vertices: Vec<Vec3A> = mesh
                    .load_vertices_sample(sample, &mut reader)
                    .unwrap()
                    .iter()
                    .map(|p| (*p).into())
                    .collect();

                let face_indices = mesh.load_faceindices_sample(sample, &mut reader).unwrap();
                let _counts = mesh.load_facecounts_sample(sample, &mut reader).unwrap();

                // Optional normals
                let maybe_normals: Option<Vec<Vec3A>> = if mesh.has_normals() {
                    match mesh.has_normals() {
                        true => {
                            let normals = mesh.load_normals_sample(sample, &mut reader).unwrap();
                            Some(normals.iter().map(|n| (*n).into()).collect())
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
    if bvh_nodes.is_empty() {
        warn!("No valid geometry found in Alembic file: {}", path);
        return Arc::new(HittableList::new());
    }
    let bvh = BVHNode::build(bvh_nodes);
    let mut cache = GLOBAL_OBJ_CACHE.write().unwrap();
    cache.insert(cache_key.to_string(), bvh.clone());

    bvh
}

/// Load a USD stage and produce a BVH of triangulated meshes with world-space
/// baked vertex positions. Xform hierarchies are honored by composing each
/// prim's `local_to_parent_transform` down from the requested root.
///
/// The outer `Primitive::Usd::transform` still applies on top of the returned
/// BVH via the standard `Instance` wrapper.
#[cfg(feature = "usd")]
pub fn load_usd_bvh(
    path: &str,
    material: Arc<dyn Material>,
    prim_path: Option<&str>,
) -> Arc<dyn Hittable> {
    use glam::Mat4 as GMat4;
    use openusd::gf::{Matrix4d, Vec3f};
    use openusd::schemas::geom::{Mesh, PointBased, Xform, Xformable};
    use openusd::sdf;
    use openusd::usd::{Prim, Stage};

    let root_key = prim_path.unwrap_or("/");
    let cache_key = format!("usd:{}#{}", path, root_key);

    {
        let cache = GLOBAL_OBJ_CACHE.read().unwrap();
        if let Some(bvh) = cache.get(&cache_key) {
            return bvh.clone();
        }
    }

    let stage = match Stage::open(path) {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to open USD stage {}: {}", path, e);
            return Arc::new(HittableList::new());
        }
    };

    let root_sdf_path = match prim_path {
        Some(p) => match sdf::path(p) {
            Ok(sp) => sp,
            Err(e) => {
                error!("Invalid USD prim path '{}': {}", p, e);
                return Arc::new(HittableList::new());
            }
        },
        None => sdf::Path::abs_root(),
    };
    let root_prim = stage.prim_at(root_sdf_path);

    // USD authors 4x4 matrices as row-vector row-major (translation in the
    // last row: indices 12..15). glam::Mat4 is column-major with column-vector
    // convention, so USD's row-major layout is exactly the column-major layout
    // of the transposed matrix — which is what we want for M * v evaluation.
    fn usd_mat_to_glam(m: Matrix4d) -> GMat4 {
        let a = m.0;
        GMat4::from_cols_array(&[
            a[0] as f32,
            a[1] as f32,
            a[2] as f32,
            a[3] as f32,
            a[4] as f32,
            a[5] as f32,
            a[6] as f32,
            a[7] as f32,
            a[8] as f32,
            a[9] as f32,
            a[10] as f32,
            a[11] as f32,
            a[12] as f32,
            a[13] as f32,
            a[14] as f32,
            a[15] as f32,
        ])
    }

    fn local_matrix_for(stage: &Stage, prim: &Prim) -> GMat4 {
        // Xform is the common non-geometry Xformable schema.
        if let Ok(Some(xf)) = Xform::get(stage, prim.path().clone()) {
            if let Ok(m) = xf.local_to_parent_transform(0.0) {
                return usd_mat_to_glam(m);
            }
        }
        // Mesh is Xformable too — pick up an authored transform on the mesh prim.
        if let Ok(Some(m)) = Mesh::get(stage, prim.path().clone()) {
            if let Ok(mat) = m.local_to_parent_transform(0.0) {
                return usd_mat_to_glam(mat);
            }
        }
        GMat4::IDENTITY
    }

    let mut tris: Vec<Arc<dyn Hittable>> = Vec::new();
    let mut stack: Vec<(Prim, GMat4)> = vec![(root_prim, GMat4::IDENTITY)];

    while let Some((prim, parent_world)) = stack.pop() {
        let local = local_matrix_for(&stage, &prim);
        // resets_xform_stack ignores the parent transform for this prim.
        let resets = Xform::get(&stage, prim.path().clone())
            .ok()
            .flatten()
            .and_then(|xf| xf.resets_xform_stack().ok())
            .or_else(|| {
                Mesh::get(&stage, prim.path().clone())
                    .ok()
                    .flatten()
                    .and_then(|m| m.resets_xform_stack().ok())
            })
            .unwrap_or(false);
        let world = if resets { local } else { parent_world * local };

        if let Ok(Some(mesh)) = Mesh::get(&stage, prim.path().clone()) {
            let points: Option<Vec<Vec3f>> = mesh
                .points_attr()
                .get::<sdf::Value>()
                .ok()
                .flatten()
                .and_then(|v| match v {
                    sdf::Value::Vec3fVec(v) => Some(v),
                    _ => None,
                });
            let counts: Option<Vec<i32>> = mesh
                .face_vertex_counts_attr()
                .get::<sdf::Value>()
                .ok()
                .flatten()
                .and_then(|v| match v {
                    sdf::Value::IntVec(v) => Some(v),
                    _ => None,
                });
            let indices: Option<Vec<i32>> = mesh
                .face_vertex_indices_attr()
                .get::<sdf::Value>()
                .ok()
                .flatten()
                .and_then(|v| match v {
                    sdf::Value::IntVec(v) => Some(v),
                    _ => None,
                });

            if let (Some(points), Some(counts), Some(indices)) = (points, counts, indices) {
                let verts: Vec<Vec3A> = points
                    .iter()
                    .map(|p| {
                        let v = world.transform_point3(glam::Vec3::new(p.x, p.y, p.z));
                        Vec3A::new(v.x, v.y, v.z)
                    })
                    .collect();

                let mut offset = 0usize;
                for &fc in &counts {
                    let fc = fc as usize;
                    if fc < 3 || offset + fc > indices.len() {
                        offset += fc;
                        continue;
                    }
                    // Fan-triangulate: (v0, vi, vi+1) for i in 1..fc-1.
                    for k in 1..(fc - 1) {
                        let i0 = indices[offset] as usize;
                        let i1 = indices[offset + k] as usize;
                        let i2 = indices[offset + k + 1] as usize;
                        if i0 >= verts.len() || i1 >= verts.len() || i2 >= verts.len() {
                            continue;
                        }
                        tris.push(Arc::new(Triangle::new(
                            verts[i0],
                            verts[i1],
                            verts[i2],
                            material.clone(),
                        )));
                    }
                    offset += fc;
                }
            } else {
                debug!(
                    "USD Mesh at {} missing points / faceVertexCounts / faceVertexIndices",
                    prim.path()
                );
            }
        }

        if let Ok(children) = prim.children() {
            for child in children {
                stack.push((child, world));
            }
        }
    }

    if tris.is_empty() {
        warn!("No triangulated geometry found in USD file: {}", path);
        return Arc::new(HittableList::new());
    }

    let bvh = BVHNode::build(tris);
    let mut cache = GLOBAL_OBJ_CACHE.write().unwrap();
    cache.insert(cache_key, bvh.clone());

    bvh
}

#[cfg(all(test, feature = "usd"))]
mod usd_tests {
    use super::*;
    use crate::material::Emissive;
    use crate::ray::Ray;

    const QUAD_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{
    def Xform "Group" (
        kind = "component"
    )
    {
        double3 xformOp:translate = (10, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]

        def Mesh "Quad"
        {
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [(0, 0, 0), (1, 0, 0.01), (1, 1, 0.01), (0, 1, 0)]
        }
    }
}
"#;

    fn write_fixture() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("crust-usd-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!(
            "quad_{}.usda",
            std::process::id() // avoid collisions between concurrent test runs
        ));
        std::fs::write(&path, QUAD_USDA).unwrap();
        path
    }

    fn cast(bvh: &Arc<dyn Hittable>, origin: Vec3A) -> bool {
        use crate::hittable::HitRecord;
        let ray = Ray::new(origin, Vec3A::new(0.0, 0.0, -1.0));
        let mut rec = HitRecord::default();
        bvh.hit(&ray, 1e-3, 1e6, &mut rec)
    }

    #[test]
    fn load_usd_bvh_parses_a_translated_quad() {
        let path = write_fixture();

        let mat: Arc<dyn Material> = Arc::new(Emissive::new(
            Vec3A::new(1.0, 1.0, 1.0),
            Vec3A::ZERO,
            0.0,
        ));

        let bvh = load_usd_bvh(path.to_str().unwrap(), mat, None);

        assert!(
            cast(&bvh, Vec3A::new(10.5, 0.5, 5.0)),
            "expected the translated quad at x=10 to be hit"
        );
        assert!(
            !cast(&bvh, Vec3A::new(0.5, 0.5, 5.0)),
            "expected untranslated origin to miss — Xform translate was ignored?"
        );
    }
}
