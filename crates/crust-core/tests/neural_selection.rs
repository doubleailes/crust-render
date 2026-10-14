//! `crust:lightSelection = "neural"`: the power table weighted by each
//! light's visibility, as a small network trained by the `learned` pre-pass
//! predicts it.
//!
//! The scene is `learned_selection.rs`'s failure in miniature — a floor lit
//! by a dim sphere light it can see and by a far more powerful one sealed
//! inside an opaque shell — under a dim dome, so that the lights at infinity
//! (`LightList::infinite_at`) are covered too.

use crust_core::rt::Geometry;
use crust_core::{
    AreaLight, Buffer, Camera, DomeLight, Emissive, Light, LightList, LightSelection,
    MASK_INDIRECT, MASK_SHADOW, OpenPBR, RenderSettings, Renderer, Scene, SphereShape, Vec3A,
    WorldBuilder,
};
use std::sync::{Arc, Mutex};

const VISIBLE: Vec3A = Vec3A::new(0.0, 3.0, 0.0);
const SEALED: Vec3A = Vec3A::new(3.0, 1.0, 0.0);
const W: usize = 64;
const H: usize = 36;

fn camera(aspect: f32) -> Camera {
    Camera::new(
        Vec3A::new(0.0, 5.0, 7.0),
        Vec3A::ZERO,
        Vec3A::Y,
        50.0,
        aspect,
        0.0,
        8.0,
    )
}

fn floor(world: &mut WorldBuilder) {
    world.attach(
        Geometry::TriangleMesh {
            vertices: vec![
                [-6.0, 0.0, -6.0],
                [6.0, 0.0, -6.0],
                [6.0, 0.0, 6.0],
                [-6.0, 0.0, 6.0],
            ],
            indices: vec![[0, 2, 1], [0, 3, 2]],
            normals: None,
        },
        Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))),
    );
}

/// Adds a sphere light, seen by shadow and indirect rays.
fn sphere_light(world: &mut WorldBuilder, lights: &mut LightList, center: Vec3A, radiance: f32) {
    let emitter = Arc::new(Emissive::new(Vec3A::splat(radiance)));
    let id = world.attach_masked(
        Geometry::Sphere {
            center,
            radius: 0.3,
        },
        emitter.clone(),
        MASK_SHADOW | MASK_INDIRECT,
    );
    lights.add(AreaLight::new(
        SphereShape {
            center,
            radius: 0.3,
        },
        emitter,
        id,
    ));
}

fn settings(selection: LightSelection, w: usize, h: usize, spp: u32) -> RenderSettings {
    RenderSettings::default()
        .with_resolution(w, h)
        .with_samples_per_pixel(spp)
        .with_max_depth(4)
        .with_adaptive_sampling(spp, 0.0)
        .with_light_selection(selection)
}

/// The sealed-light scene under a dome, rendered with `selection`.
fn scene(selection: LightSelection, spp: u32) -> Renderer {
    scene_at(selection, spp, W, H)
}

/// [`scene`] at `w` × `h`.
fn scene_at(selection: LightSelection, spp: u32, w: usize, h: usize) -> Renderer {
    let mut world = WorldBuilder::new();
    floor(&mut world);
    world.attach(
        Geometry::Sphere {
            center: SEALED,
            radius: 0.8,
        },
        Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))),
    );
    let mut lights = LightList::new();
    sphere_light(&mut world, &mut lights, VISIBLE, 5.0);
    sphere_light(&mut world, &mut lights, SEALED, 400.0);
    lights.add(DomeLight::new(
        Vec3A::splat(0.05),
        None,
        glam::Mat3A::IDENTITY,
    ));
    Renderer::new(
        camera(w as f32 / h as f32),
        world.commit(),
        lights,
        settings(selection, w, h, spp),
    )
}

fn same(a: &Buffer, b: &Buffer, w: usize, h: usize) -> bool {
    (0..h).all(|y| (0..w).all(|x| a.get_pixel(x, y).to_array() == b.get_pixel(x, y).to_array()))
}

/// The network learns the sealed light down to (about) the defensive share,
/// where power gives it most of the picks.
///
/// At 256 × 144, not the other tests' 64 × 36: training takes a fixed eight
/// passes over the receivers, and the ~300 of a 64 × 36 frame make sixteen
/// Adam steps, after which every prediction is still near ½.
#[test]
fn the_sealed_light_is_predicted_hidden() {
    let r = scene_at(LightSelection::Neural, 1, 256, 144);
    assert_eq!(r.lights.selection(), LightSelection::Neural);
    let power = scene(LightSelection::Power, 1);
    let cache = r.lights.visibility_cache().expect("trained");
    let p = Vec3A::new(-1.0, 0.0, 1.0);
    let v = cache.visibility(p).expect("the floor is inside the bounds");
    assert!(v[1] < 0.1, "the sealed light: V̂ {}", v[1]);
    // Not yet saturated after the few steps a small frame trains for.
    assert!(v[0] > 0.75, "the visible light: V̂ {}", v[0]);
    let (sealed, by_power) = (r.lights.pmf_at(p, 1), power.lights.pmf_at(p, 1));
    assert!(
        by_power > 0.4,
        "power favours the sealed light ({by_power})"
    );
    // At least the defensive 0.3 split over three live lights, and well
    // under what power gave it.
    assert!(
        sealed >= 0.1 - 1e-6 && sealed < 0.5 * by_power,
        "sealed light pmf {sealed}, by power {by_power}"
    );
}

/// What NEE picks with at a vertex is, bit for bit, what the bounce side
/// weighs emission from that vertex with — a light hit by geometry, the dome
/// by escaping — whether the thread's memo holds that vertex or not.
#[test]
fn both_mis_sides_read_the_same_bits() {
    let r = scene(LightSelection::Neural, 1);
    let lights = &r.lights;
    let cache = lights.visibility_cache().expect("trained");
    let elsewhere = Vec3A::new(1.7, 0.0, -2.3);
    let bits = |v: f32| v.to_bits();
    let mut checked = 0;
    for z in -5i32..=5 {
        for x in -5..=5 {
            let p = Vec3A::new(
                x as f32 * 1.1,
                (x + z).rem_euclid(3) as f32 * 0.7,
                z as f32 * 1.1,
            );
            for i in 0..16 {
                let u = (i as f32 + 0.5) / 16.0;
                // Cold: the memo last held another vertex.
                lights.pmf_at(elsewhere, 0);
                let (index, pmf) = lights.pick_index_at(p, u).unwrap();
                // The pure function, without the memo.
                let want = match cache.table_uncached(p) {
                    Some((table, _)) => table[index],
                    None => lights.pmf(index),
                };
                assert_eq!(bits(pmf), bits(want), "{p:?}, light {index}");
                for cold in [false, true] {
                    if cold {
                        lights.pmf_at(elsewhere, 0);
                    }
                    let bounce = match lights.lights()[index].geom_id() {
                        Some(geom) => {
                            let (found, pmf) = lights.find_index_by_geom_at(geom, p).unwrap();
                            assert_eq!(found, index);
                            pmf
                        }
                        None => {
                            let (_, pmf) = lights.infinite_at(p).next().unwrap();
                            pmf
                        }
                    };
                    assert_eq!(bits(pmf), bits(bounce), "{p:?}, light {index}, cold {cold}");
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 11 * 11 * 16);
    // Outside the trained bounds the power table answers, on both sides.
    let far = Vec3A::splat(1e4);
    let power = scene(LightSelection::Power, 1);
    for index in 0..3 {
        assert_eq!(
            bits(lights.pmf_at(far, index)),
            bits(power.lights.pmf(index))
        );
    }
    assert_eq!(
        bits(lights.infinite_at(far).next().unwrap().1),
        bits(power.lights.pmf(2))
    );
}

/// The network is frozen before the first pass and trained from the scene
/// alone: tiles and scanlines, two trainings, and one thread against the
/// default pool all give the same image.
#[test]
fn neural_renders_are_deterministic_and_scheduling_free() {
    // At 16 spp, as every image comparison in the repository is.
    let a = scene(LightSelection::Neural, 16);
    let tiles = a.render_with_tiles();
    assert!(same(&tiles, &a.render(), W, H), "tiles vs scanlines");
    let b = scene(LightSelection::Neural, 16);
    assert!(same(&tiles, &b.render_with_tiles(), W, H), "two trainings");
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("pool")
        .install(|| scene(LightSelection::Neural, 16).render());
    assert!(same(&tiles, &one, W, H), "one thread vs the default pool");
}

/// A `tracing` subscriber that keeps every WARN message.
struct Warnings(Arc<Mutex<Vec<String>>>);

struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl tracing::Subscriber for Warnings {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() == tracing::Level::WARN {
            let mut message = Message(String::new());
            event.record(&mut message);
            self.0.lock().unwrap().push(message.0);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// Builds a floor lit by `count` small sphere lights in a row, renders it
/// under `selection` at 16 spp, and returns the image, the selection the
/// renderer ended up with, and the WARN lines it logged.
fn row_of_lights(count: usize, selection: LightSelection) -> (Buffer, LightSelection, Vec<String>) {
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let (image, picked) = tracing::subscriber::with_default(Warnings(warnings.clone()), || {
        let mut world = WorldBuilder::new();
        floor(&mut world);
        let mut lights = LightList::new();
        for k in 0..count {
            let x = -5.0 + 10.0 * (k as f32 + 0.5) / count as f32;
            sphere_light(
                &mut world,
                &mut lights,
                Vec3A::new(x, 1.5, 0.0),
                2.0 + k as f32,
            );
        }
        let r = Renderer::new(
            camera(1.0),
            world.commit(),
            lights,
            settings(selection, 24, 24, 16),
        );
        (r.render(), r.lights.selection())
    });
    let warnings = warnings.lock().unwrap().clone();
    (image, picked, warnings)
}

/// Spec "Too many lights": one WARN names the limit, and the image is the
/// power render's, bit for bit.
#[test]
fn too_many_lights_fall_back_to_power_with_one_warning() {
    let (neural, picked, warnings) = row_of_lights(65, LightSelection::Neural);
    assert_eq!(picked, LightSelection::Power);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("neural") && warnings[0].contains("64"),
        "{}",
        warnings[0]
    );
    let (power, _, warnings) = row_of_lights(65, LightSelection::Power);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(same(&neural, &power, 24, 24));
}

/// Spec "One light": nothing to learn, nothing logged at WARN, and the image
/// is the power render's, bit for bit.
#[test]
fn one_light_is_power_silently() {
    let (neural, picked, warnings) = row_of_lights(1, LightSelection::Neural);
    assert_eq!(picked, LightSelection::Power);
    assert!(warnings.is_empty(), "{warnings:?}");
    let (power, _, _) = row_of_lights(1, LightSelection::Power);
    assert!(same(&neural, &power, 24, 24));
}

/// `crust:lightSelection = "neural"` on the render settings selects it, and
/// a renderer built from the stage trains it.
#[test]
fn the_stage_setting_selects_neural() {
    let dir = std::env::temp_dir().join("crust_neural_selection_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("neural.usda");
    std::fs::write(
        &path,
        r#"#usda 1.0
(
    upAxis = "Y"
)

def Xform "World"
{
    def Mesh "Floor"
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 3, 2, 1]
        point3f[] points = [(-60, 0, -60), (60, 0, -60), (60, 0, 60), (-60, 0, 60)]
    }
    def SphereLight "A"
    {
        float inputs:radius = 0.3
        double3 xformOp:translate = (-2, 2, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def SphereLight "B"
    {
        float inputs:radius = 0.3
        double3 xformOp:translate = (2, 2, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Camera "Cam"
    {
        double3 xformOp:translate = (0, 4, 8)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
}

def Scope "Render"
{
    def RenderSettings "settings"
    {
        int2 resolution = (32, 24)
        token crust:lightSelection = "neural"
    }
}
"#,
    )
    .expect("write stage");
    let scene = Scene::from_usd(&path).expect("stage");
    assert_eq!(scene.settings.light_selection(), LightSelection::Neural);
    let r = Renderer::new(scene.camera, scene.world, scene.lights, scene.settings);
    assert_eq!(r.lights.selection(), LightSelection::Neural);
}
