use std::path;

use crust_render::{Camera, DocObject, Document, MaterialType, ObjectList, Primitive, Vec3};

fn main() {
    const ASPECT_RATIO: f32 = 16.0 / 9.0;
    const IMAGE_WIDTH: usize = 400;
    const IMAGE_HEIGHT: usize = (IMAGE_WIDTH as f32 / ASPECT_RATIO) as usize;
    let lookfrom = Vec3::new(13.0, 2.0, 3.0);
    let lookat = Vec3::new(0.0, 0.0, 0.0);
    let vup = Vec3::new(0.0, 1.0, 0.0);
    let dist_to_focus = 10.0;
    let aperture = 0.1;

    let cam = Camera::new(
        lookfrom,
        lookat,
        vup,
        20.0,
        ASPECT_RATIO,
        aperture,
        dist_to_focus,
    );
    let render_settings =
        crust_render::RenderSettings::new(100, 32, IMAGE_WIDTH, IMAGE_HEIGHT, 32, 0.05, 0);
    let mut object_list = ObjectList::new(vec![]);
    // Add objects to the object_list here
    let ground: DocObject = DocObject::new(
        "ground".to_string(),
        Primitive::Sphere {
            center: Vec3::new(0.0, -1000.0, 0.0),
            radius: 1000.0,
        },
        MaterialType::Lambertian(crust_render::Lambertian::new(Vec3::new(0.5, 0.5, 0.5))),
    );
    object_list.add(ground);
    // Deterministic grid of spheres with preset materials
    for a in -2..3 {
        for b in -2..3 {
            let center = Vec3::new(a as f32, 0.2, b as f32);
            let material: MaterialType = match (a + b) % 4 {
                0 => MaterialType::Lambertian(crust_render::Lambertian::new(Vec3::new(
                    0.8, 0.3, 0.3,
                ))),
                1 => MaterialType::Metal(crust_render::Metal::new(Vec3::new(0.7, 0.6, 0.5), 0.1)),
                2 => MaterialType::Dielectric(crust_render::Dielectric::new(1.5)),
                _ => MaterialType::CookTorrance(crust_render::CookTorrance::new(
                    Vec3::new(0.9, 0.9, 0.9),
                    0.2,
                    0.5,
                )),
            };
            let doc_object = DocObject::new(
                format!("sphere_{}_{}", a, b),
                Primitive::Sphere {
                    center,
                    radius: 0.2,
                },
                material,
            );
            object_list.add(doc_object);
        }
    }
    let doc_o1 = DocObject::new(
        "center_sphere_1".to_string(),
        Primitive::Sphere {
            center: Vec3::new(0.0, 1.0, 0.0),
            radius: 1.0,
        },
        MaterialType::Dielectric(crust_render::Dielectric::new(1.5)),
    );
    object_list.add(doc_o1);
    let doc_o2 = DocObject::new(
        "center_sphere_2".to_string(),
        Primitive::Sphere {
            center: Vec3::new(-4.0, 1.0, 0.0),
            radius: 1.0,
        },
        MaterialType::Lambertian(crust_render::Lambertian::new(Vec3::new(0.4, 0.2, 0.1))),
    );
    object_list.add(doc_o2);
    let doc_o3 = DocObject::new(
        "center_sphere_3".to_string(),
        Primitive::Sphere {
            center: Vec3::new(4.0, 1.0, 0.0),
            radius: 1.0,
        },
        MaterialType::Metal(crust_render::Metal::new(Vec3::new(0.7, 0.6, 0.5), 0.0)),
    );
    object_list.add(doc_o3);
    let doc_o4 = DocObject::new(
        "center_sphere_4".to_string(),
        Primitive::Sphere {
            center: Vec3::new(0.0, 1.0, 4.0),
            radius: 1.0,
        },
        MaterialType::CookTorrance(crust_render::CookTorrance::new(
            Vec3::new(0.5, 0.5, 0.5),
            0.2,
            0.0,
        )),
    );
    object_list.add(doc_o4);
    let doc_o5 = DocObject::new(
        "center_sphere_5".to_string(),
        Primitive::Sphere {
            center: Vec3::new(0.0, 1.0, -4.0),
            radius: 1.0,
        },
        MaterialType::Disney(crust_render::Disney::new(
            Vec3::new(0.5, 0.5, 0.5),
            0.0,
            0.2,
            0.5,
            0.5,
            0.0,
            0.5,
            0.0,
            1.0,
        )),
    );
    object_list.add(doc_o5);
    // Add a light source
    let light_center = Vec3::new(0.0, 7.0, 0.0);
    let light_radius = 1.0;
    let light_1 = DocObject::new(
        "light_1".to_string(),
        Primitive::Sphere {
            center: light_center,
            radius: light_radius,
        },
        MaterialType::Emissive(crust_render::Emissive::new(
            Vec3::new(10.0, 10.0, 10.0),
            light_center,
            light_radius,
        )),
    );
    object_list.add(light_1);
    let light_center = Vec3::new(-4.0, 7.0, 0.0);
    let light_2 = DocObject::new(
        "light_2".to_string(),
        Primitive::Sphere {
            center: light_center,
            radius: light_radius,
        },
        MaterialType::Emissive(crust_render::Emissive::new(
            Vec3::new(20.0, 10.0, 7.0),
            light_center,
            light_radius,
        )),
    );
    object_list.add(light_2);
    // Create a new document
    let doc = Document::new(cam, object_list, render_settings);
    let path = path::Path::new("samples/scene.ron");
    doc.write(path).unwrap();
}
