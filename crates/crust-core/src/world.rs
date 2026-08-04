use crate::RenderSettings;
use crate::camera::Camera;
use crate::light::{AreaLight, LightList, SphereShape};
use crate::material::{Emissive, Material, OpenPBR};
use crate::ray::{MASK_ALL, MASK_CAMERA};
use crate::rt_world::{World, WorldBuilder};
use crust_rt::Geometry;
use glam::Vec3A;
use std::sync::Arc;
use utils::{random_range3, random3};

/// Attaches an analytic sphere with its material.
fn add_sphere(world: &mut WorldBuilder, center: Vec3A, radius: f32, material: Arc<dyn Material>) {
    world.attach(Geometry::Sphere { center, radius }, material);
}

/// Adds a sphere light to the scene: emissive sphere geometry in `world`
/// plus an `AreaLight` over the same surface in `lights`, tied together by
/// the geometry id (one surface, both roles). The geometry is masked out
/// of camera rays — the industry default for light sources — while every
/// other ray category still sees it (occlusion and the bounce side of MIS).
fn add_sphere_light(
    world: &mut WorldBuilder,
    lights: &mut LightList,
    color: Vec3A,
    center: Vec3A,
    radius: f32,
) {
    let material = Arc::new(Emissive::new(color));
    let geom_id = world.attach_masked(
        Geometry::Sphere { center, radius },
        material.clone(),
        MASK_ALL & !MASK_CAMERA,
    );
    lights.add(Arc::new(AreaLight::new(
        Box::new(SphereShape { center, radius }),
        material,
        geom_id,
    )));
}

#[allow(dead_code)]
pub fn random_scene() -> (World, LightList) {
    let mut world = WorldBuilder::new();
    let mut lights = LightList::new();

    let ground_material = Arc::new(OpenPBR::diffuse(Vec3A::new(0.5, 0.5, 0.5)));
    add_sphere(&mut world, Vec3A::new(0.0, -1000.0, 0.0), 1000.0, ground_material);

    for a in -11..11 {
        for b in -11..11 {
            let choose_mat = utils::random();
            let center = Vec3A::new(
                a as f32 + 0.9 * utils::random(),
                0.2,
                b as f32 + 0.9 * utils::random(),
            );

            if (center - Vec3A::new(4.0, 0.2, 0.0)).length() > 0.9 {
                let sphere_material: Arc<dyn Material> = if choose_mat < 0.3 {
                    // Diffuse
                    Arc::new(OpenPBR::diffuse(random3() * random3()))
                } else if choose_mat < 0.8 {
                    // Glossy
                    Arc::new(OpenPBR::glossy(
                        random_range3(0.5, 1.0),
                        utils::random_range(0.0, 0.5),
                        utils::random_range(0.0, 1.0),
                    ))
                } else if choose_mat < 0.95 {
                    // Metal
                    Arc::new(OpenPBR::metal(
                        random_range3(0.5, 1.0),
                        utils::random_range(0.0, 0.5),
                    ))
                } else {
                    // Glass
                    Arc::new(OpenPBR::glass(1.5))
                };
                add_sphere(&mut world, center, 0.2, sphere_material);
            }
        }
    }

    let material1 = Arc::new(OpenPBR::glass(1.5));
    add_sphere(&mut world, Vec3A::new(0.0, 1.0, 0.0), 1.0, material1);

    let material2 = Arc::new(OpenPBR::diffuse(Vec3A::new(0.4, 0.2, 0.1)));
    add_sphere(&mut world, Vec3A::new(-4.0, 1.0, 0.0), 1.0, material2);

    let material3 = Arc::new(OpenPBR::metal(Vec3A::new(0.7, 0.6, 0.5), 0.0));
    add_sphere(&mut world, Vec3A::new(4.0, 1.0, 0.0), 1.0, material3);

    add_sphere_light(
        &mut world,
        &mut lights,
        Vec3A::new(10.0, 10.0, 10.0),
        Vec3A::new(0.0, 7.0, 0.0),
        1.0,
    );
    add_sphere_light(
        &mut world,
        &mut lights,
        Vec3A::new(20.0, 10.0, 7.0),
        Vec3A::new(-4.0, 7.0, 0.0),
        1.0,
    );

    (world.commit(), lights)
}

pub fn simple_scene() -> (World, LightList) {
    let mut world = WorldBuilder::new();
    let mut lights = LightList::new();

    let ground_material = Arc::new(OpenPBR::diffuse(Vec3A::new(0.8, 0.5, 0.5)));
    add_sphere(&mut world, Vec3A::new(0.0, -1000.0, 0.0), 1000.0, ground_material);

    // Deterministic grid of spheres with preset materials
    for a in -2..3 {
        for b in -2..3 {
            let center = Vec3A::new(a as f32, 0.2, b as f32);
            let material: Arc<dyn Material> = match (a + b) % 4 {
                0 => Arc::new(OpenPBR::diffuse(Vec3A::new(0.8, 0.3, 0.3))),
                1 => Arc::new(OpenPBR::metal(Vec3A::new(0.7, 0.6, 0.5), 0.1)),
                2 => Arc::new(OpenPBR::glass(1.5)),
                _ => Arc::new(OpenPBR::glossy(Vec3A::new(0.9, 0.9, 0.9), 0.2, 0.5)),
            };

            add_sphere(&mut world, center, 0.2, material);
        }
    }

    // Center spheres
    let material1 = Arc::new(OpenPBR::glass(1.5));
    add_sphere(&mut world, Vec3A::new(0.0, 1.0, 0.0), 1.0, material1);

    let material2 = Arc::new(OpenPBR::diffuse(Vec3A::new(0.4, 0.2, 0.1)));
    add_sphere(&mut world, Vec3A::new(-4.0, 1.0, 0.0), 1.0, material2);

    let material3 = Arc::new(OpenPBR::metal(Vec3A::new(0.7, 0.6, 0.5), 0.0));
    add_sphere(&mut world, Vec3A::new(4.0, 1.0, 0.0), 1.0, material3);

    let material4 = Arc::new(OpenPBR::glossy(Vec3A::new(0.5, 0.5, 0.5), 0.2, 0.0));
    add_sphere(&mut world, Vec3A::new(0.0, 1.0, 4.0), 1.0, material4);

    let material5 = Arc::new(OpenPBR {
        base_color: Vec3A::new(0.5, 0.5, 0.5),
        specular_roughness: 0.2,
        coat_weight: 1.0,
        ..OpenPBR::default()
    });
    add_sphere(&mut world, Vec3A::new(0.0, 1.0, -4.0), 1.0, material5);

    // Lights
    add_sphere_light(
        &mut world,
        &mut lights,
        Vec3A::new(10.0, 10.0, 10.0),
        Vec3A::new(0.0, 7.0, 0.0),
        1.0,
    );
    add_sphere_light(
        &mut world,
        &mut lights,
        Vec3A::new(20.0, 10.0, 7.0),
        Vec3A::new(-4.0, 7.0, 0.0),
        1.0,
    );

    (world.commit(), lights)
}

pub fn get_settings() -> (Camera, RenderSettings) {
    const ASPECT_RATIO: f32 = 16.0 / 9.0;
    const IMAGE_WIDTH: usize = 400;
    const IMAGE_HEIGHT: usize = (IMAGE_WIDTH as f32 / ASPECT_RATIO) as usize;
    let lookfrom = Vec3A::new(15.0, 3.0, 3.0);
    let lookat = Vec3A::new(0.0, 1.0, 0.0);
    let vup = Vec3A::new(0.0, 1.0, 0.0);
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
    let render_settings = RenderSettings::new(64, 32, IMAGE_WIDTH, IMAGE_HEIGHT, 32, 0.05, 0);

    (cam, render_settings)
}
