//! Generates `samples/openpbr_showcase.ron` — seven OpenPBR spheres, each
//! demonstrating a different lobe of the shader.
//!
//! Run with:
//!
//!     cargo run --release -p crust-core --example openpbr_showcase
//!
//! Then render:
//!
//!     cargo run --release -p crust-render -- samples/openpbr_showcase.ron
//!
//! The camera and lighting rig mirrors `samples/scene.ron`.

use std::path;

use crust_core::{
    Camera, DocObject, Document, MaterialType, ObjectList, OpenPBR, Primitive, Vec3A,
};

fn main() {
    const ASPECT_RATIO: f32 = 16.0 / 9.0;
    const IMAGE_WIDTH: usize = 640;
    const IMAGE_HEIGHT: usize = (IMAGE_WIDTH as f32 / ASPECT_RATIO) as usize;

    let lookfrom = Vec3A::new(0.0, 3.0, 22.0);
    let lookat = Vec3A::new(0.0, 1.0, 0.0);
    let vup = Vec3A::new(0.0, 1.0, 0.0);
    let dist_to_focus = 22.0;
    let aperture = 0.05;

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
        crust_core::RenderSettings::new(128, 32, IMAGE_WIDTH, IMAGE_HEIGHT, 32, 0.05, 0);

    let mut object_list = ObjectList::new(vec![]);

    // Ground.
    object_list.add(DocObject::new(
        "ground".to_string(),
        Primitive::Sphere {
            center: Vec3A::new(0.0, -1000.0, 0.0),
            radius: 1000.0,
        },
        MaterialType::Lambertian(crust_core::Lambertian::new(Vec3A::new(0.5, 0.5, 0.5))),
    ));

    // Seven demo spheres on the x-axis.
    let sphere_y = 1.0;
    let radius = 1.0;
    let spacing = 2.5;
    let showcase: [(f32, &str, OpenPBR); 7] = [
        (
            -3.0 * spacing,
            "chrome_metal",
            OpenPBR {
                base_metalness: 1.0,
                base_color: Vec3A::new(0.95, 0.95, 0.95),
                specular_roughness: 0.02,
                ..OpenPBR::default()
            },
        ),
        (
            -2.0 * spacing,
            "red_plastic",
            OpenPBR {
                base_color: Vec3A::new(0.8, 0.1, 0.1),
                base_metalness: 0.0,
                specular_ior: 1.5,
                specular_roughness: 0.3,
                ..OpenPBR::default()
            },
        ),
        (
            -1.0 * spacing,
            "red_car_paint",
            OpenPBR {
                base_color: Vec3A::new(0.8, 0.05, 0.05),
                base_metalness: 0.9,
                specular_roughness: 0.25,
                coat_weight: 1.0,
                coat_roughness: 0.02,
                coat_ior: 1.5,
                ..OpenPBR::default()
            },
        ),
        (
            0.0,
            "velvet_cloth",
            OpenPBR {
                base_color: Vec3A::new(0.35, 0.15, 0.55),
                base_diffuse_roughness: 0.9,
                specular_roughness: 0.9,
                fuzz_weight: 1.0,
                fuzz_color: Vec3A::new(1.0, 1.0, 1.0),
                fuzz_roughness: 0.3,
                ..OpenPBR::default()
            },
        ),
        (
            1.0 * spacing,
            "green_glass_dispersion",
            OpenPBR {
                base_color: Vec3A::new(1.0, 1.0, 1.0),
                specular_roughness: 0.02,
                specular_ior: 1.55,
                transmission_weight: 1.0,
                transmission_color: Vec3A::new(0.7, 0.9, 0.75),
                transmission_depth: 1.0,
                transmission_dispersion_scale: 0.6,
                transmission_dispersion_abbe_number: 20.0,
                ..OpenPBR::default()
            },
        ),
        (
            2.0 * spacing,
            "waxy_subsurface",
            OpenPBR {
                base_color: Vec3A::new(0.9, 0.75, 0.55),
                subsurface_weight: 1.0,
                subsurface_color: Vec3A::new(0.95, 0.55, 0.35),
                subsurface_radius: 1.5,
                subsurface_radius_scale: Vec3A::new(1.0, 0.5, 0.25),
                specular_roughness: 0.5,
                ..OpenPBR::default()
            },
        ),
        (
            3.0 * spacing,
            "soap_bubble",
            OpenPBR {
                base_color: Vec3A::new(0.8, 0.8, 0.8),
                specular_roughness: 0.02,
                specular_ior: 1.33,
                thin_film_weight: 1.0,
                thin_film_thickness: 0.55,
                thin_film_ior: 1.4,
                transmission_weight: 0.4,
                transmission_color: Vec3A::new(0.95, 0.95, 1.0),
                geometry_thin_walled: true,
                ..OpenPBR::default()
            },
        ),
    ];

    for (x, name, mat) in showcase {
        object_list.add(DocObject::new(
            name.to_string(),
            Primitive::Sphere {
                center: Vec3A::new(x, sphere_y, 0.0),
                radius,
            },
            MaterialType::OpenPBR(mat),
        ));
    }

    // Two overhead area lights.
    let l1_c = Vec3A::new(-2.0, 8.0, 3.0);
    object_list.add(DocObject::new(
        "light_1".to_string(),
        Primitive::Sphere {
            center: l1_c,
            radius: 1.5,
        },
        MaterialType::Emissive(crust_core::Emissive::new(
            Vec3A::new(15.0, 15.0, 15.0),
            l1_c,
            1.5,
        )),
    ));
    let l2_c = Vec3A::new(4.0, 8.0, -2.0);
    object_list.add(DocObject::new(
        "light_2".to_string(),
        Primitive::Sphere {
            center: l2_c,
            radius: 1.5,
        },
        MaterialType::Emissive(crust_core::Emissive::new(
            Vec3A::new(18.0, 12.0, 9.0),
            l2_c,
            1.5,
        )),
    ));

    let doc = Document::new(cam, object_list, render_settings);
    let out = path::Path::new("samples/openpbr_showcase.ron");
    doc.write(out).unwrap();
    println!("wrote {}", out.display());
}
