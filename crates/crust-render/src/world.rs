use crate::hittable_list::HittableList;
use crate::light::LightList;
use crate::material::Material;
use crate::material::{CookTorrance, Dielectric, Disney, Emissive, Lambertian, Metal};
use std::sync::Arc;
use utils::Color;
use utils::Point3;
use crate::Sphere;

#[allow(dead_code)]
pub fn random_scene() -> (HittableList, LightList) {
    let mut world = HittableList::new();
    let mut lights = LightList::new();

    let ground_material = Arc::new(Lambertian::new(Color::new(0.5, 0.5, 0.5)));
    world.add(Box::new(Sphere::new(
        Point3::new(0.0, -1000.0, 0.0),
        1000.0,
        ground_material,
    )));

    for a in -11..11 {
        for b in -11..11 {
            let choose_mat = utils::random();
            let center = Point3::new(
                a as f32 + 0.9 * utils::random(),
                0.2,
                b as f32 + 0.9 * utils::random(),
            );

            if (center - Point3::new(4.0, 0.2, 0.0)).length() > 0.9 {
                if choose_mat < 0.3 {
                    // Diffuse
                    let albedo = Color::random() * Color::random();
                    let sphere_material = Arc::new(Lambertian::new(albedo));
                    world.add(Box::new(Sphere::new(center, 0.2, sphere_material)));
                } else if choose_mat < 0.8 {
                    // Cook-Torrance
                    let albedo = Color::random_range(0.5, 1.0);
                    let roughness = utils::random_range(0.0, 0.5);
                    let metallic = utils::random_range(0.0, 1.0);
                    let sphere_material = Arc::new(CookTorrance::new(albedo, roughness, metallic));
                    world.add(Box::new(Sphere::new(center, 0.2, sphere_material)));
                } else if choose_mat < 0.95 {
                    // Metal
                    let albedo = Color::random_range(0.5, 1.0);
                    let fuzz = utils::random_range(0.0, 0.5);
                    let sphere_material = Arc::new(Metal::new(albedo, fuzz));
                    world.add(Box::new(Sphere::new(center, 0.2, sphere_material)));
                } else {
                    // Glass
                    let sphere_material = Arc::new(Dielectric::new(1.5));
                    world.add(Box::new(Sphere::new(center, 0.2, sphere_material)));
                }
            }
        }
    }

    let material1 = Arc::new(Dielectric::new(1.5));
    world.add(Box::new(Sphere::new(
        Point3::new(0.0, 1.0, 0.0),
        1.0,
        material1,
    )));

    let material2 = Arc::new(Lambertian::new(Color::new(0.4, 0.2, 0.1)));
    world.add(Box::new(Sphere::new(
        Point3::new(-4.0, 1.0, 0.0),
        1.0,
        material2,
    )));

    let material3 = Arc::new(Metal::new(Color::new(0.7, 0.6, 0.5), 0.0));
    world.add(Box::new(Sphere::new(
        Point3::new(4.0, 1.0, 0.0),
        1.0,
        material3,
    )));

    let light: Arc<Emissive> = Arc::new(Emissive::new(
        Color::new(10.0, 10.0, 10.0),
        Point3::new(0.0, 7.0, 0.0),
        1.0,
    ));
    world.add(Box::new(Sphere::new(
        light.position(),
        light.radius(),
        light.clone(),
    )));

    lights.add(light);
    let light2 = Arc::new(Emissive::new(
        Color::new(20.0, 10.0, 7.0),
        Point3::new(-4.0, 7.0, 0.0),
        1.0,
    ));
    world.add(Box::new(Sphere::new(
        light2.position(),
        light2.radius(),
        light2.clone(),
    )));
    lights.add(light2);

    (world, lights)
}

pub fn simple_scene() -> (HittableList, LightList) {
    let mut world = HittableList::new();
    let mut lights = LightList::new();

    let ground_material = Arc::new(Lambertian::new(Color::new(0.8, 0.5, 0.5)));
    world.add(Box::new(Sphere::new(
        Point3::new(0.0, -1000.0, 0.0),
        1000.0,
        ground_material,
    )));

    // Deterministic grid of spheres with preset materials
    for a in -2..3 {
        for b in -2..3 {
            let center = Point3::new(a as f32, 0.2, b as f32);
            let material: Arc<dyn Material> = match (a + b) % 4 {
                0 => Arc::new(Lambertian::new(Color::new(0.8, 0.3, 0.3))),
                1 => Arc::new(Metal::new(Color::new(0.7, 0.6, 0.5), 0.1)),
                2 => Arc::new(Dielectric::new(1.5)),
                _ => Arc::new(CookTorrance::new(Color::new(0.9, 0.9, 0.9), 0.2, 0.5)),
            };

            world.add(Box::new(Sphere::new(center, 0.2, material)));
        }
    }

    // Center spheres
    let material1 = Arc::new(Dielectric::new(1.5));
    world.add(Box::new(Sphere::new(
        Point3::new(0.0, 1.0, 0.0),
        1.0,
        material1,
    )));

    let material2 = Arc::new(Lambertian::new(Color::new(0.4, 0.2, 0.1)));
    world.add(Box::new(Sphere::new(
        Point3::new(-4.0, 1.0, 0.0),
        1.0,
        material2,
    )));

    let material3 = Arc::new(Metal::new(Color::new(0.7, 0.6, 0.5), 0.0));
    world.add(Box::new(Sphere::new(
        Point3::new(4.0, 1.0, 0.0),
        1.0,
        material3,
    )));

    let material4 = Arc::new(CookTorrance::new(Color::new(0.5, 0.5, 0.5), 0.2, 0.0));
    world.add(Box::new(Sphere::new(
        Point3::new(0.0, 1.0, 4.0),
        1.0,
        material4,
    )));

    let material5 = Arc::new(Disney::new(
        Color::new(0.5, 0.5, 0.5),
        0.0,
        0.2,
        0.5,
        0.5,
        0.0,
        0.5,
        0.0,
        1.0,
    ));
    world.add(Box::new(Sphere::new(
        Point3::new(0.0, 1.0, -4.0),
        1.0,
        material5,
    )));

    // Lights
    let light1 = Arc::new(Emissive::new(
        Color::new(10.0, 10.0, 10.0),
        Point3::new(0.0, 7.0, 0.0),
        1.0,
    ));
    world.add(Box::new(Sphere::new(
        light1.position(),
        light1.radius(),
        light1.clone(),
    )));
    lights.add(light1);

    let light2 = Arc::new(Emissive::new(
        Color::new(20.0, 10.0, 7.0),
        Point3::new(-4.0, 7.0, 0.0),
        1.0,
    ));
    world.add(Box::new(Sphere::new(
        light2.position(),
        light2.radius(),
        light2.clone(),
    )));
    lights.add(light2);

    (world, lights)
}
