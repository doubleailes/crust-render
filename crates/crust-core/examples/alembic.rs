use std::path;

use crust_core::{Camera, DocObject, Document, Mat4, MaterialType, ObjectList, Primitive, Vec3A};

fn main() {
    const ASPECT_RATIO: f32 = 16.0 / 9.0;
    const IMAGE_WIDTH: usize = 400;
    const IMAGE_HEIGHT: usize = (IMAGE_WIDTH as f32 / ASPECT_RATIO) as usize;
    let lookfrom = Vec3A::new(0.0, 3.0, 13.0);
    let lookat = Vec3A::new(0.0, 1.0, 0.0);
    let vup = Vec3A::new(0.0, 1.0, 0.0);
    let dist_to_focus = 10.0;
    let aperture = 0.1;

    let cam = Camera::new(
        lookfrom,
        lookat,
        vup,
        45.0,
        ASPECT_RATIO,
        aperture,
        dist_to_focus,
    );
    let render_settings =
        crust_core::RenderSettings::new(64, 32, IMAGE_WIDTH, IMAGE_HEIGHT, 32, 0.0, 0);
    let mut object_list = ObjectList::new(vec![]);
    // Add objects to the object_list here
    let ground: DocObject = DocObject::new(
        "ground".to_string(),
        Primitive::Sphere {
            center: Vec3A::new(0.0, -1000.0, 0.0),
            radius: 1000.0,
        },
        MaterialType::Lambertian(crust_core::Lambertian::new(Vec3A::new(0.5, 0.5, 0.5))),
    );
    object_list.add(ground);
    let position: Mat4 = Mat4::IDENTITY;
    let p = Primitive::new_alembic("samples/capsule.abc".to_string(), position, 0, true);
    let teapot_material = crust_core::Disney::new(
        Vec3A::new(0.5, 0.5, 0.5),
        0.0,
        0.2,
        1.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
    );
    let teapot = DocObject::new(
        "teapot".to_string(),
        p,
        MaterialType::Disney(teapot_material),
    );
    object_list.add(teapot);
    // Add a light source
    let light_center = Vec3A::new(0.0, 7.0, 0.0);
    let light_radius = 1.0;
    let light_1 = DocObject::new(
        "light_1".to_string(),
        Primitive::Sphere {
            center: light_center,
            radius: light_radius,
        },
        MaterialType::Emissive(crust_core::Emissive::new(
            Vec3A::new(10.0, 10.0, 10.0),
            light_center,
            light_radius,
        )),
    );
    object_list.add(light_1);
    let light_center = Vec3A::new(-4.0, 7.0, 0.0);
    let light_2 = DocObject::new(
        "light_2".to_string(),
        Primitive::Sphere {
            center: light_center,
            radius: light_radius,
        },
        MaterialType::Emissive(crust_core::Emissive::new(
            Vec3A::new(20.0, 10.0, 7.0),
            light_center,
            light_radius,
        )),
    );
    object_list.add(light_2);
    // Create a new document
    let doc = Document::new(cam, object_list, render_settings);
    let path = path::Path::new("samples/box.ron");
    doc.write(path).unwrap();
}
