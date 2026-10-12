//! The in-place update entry points (`crust_core::live`): the provenance an
//! import records, and the material and light swaps a world and light list
//! take or refuse. The bit-identity of an in-place update with a fresh import
//! is pinned end to end by the `crust mcp` session's tests.

use crust_core::live::Provenance;
use crust_core::{Emissive, LightKind, Material, OpenPBR, Scene, Vec3A};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

fn ptr(m: &Arc<dyn Material>) -> *const () {
    Arc::as_ptr(m) as *const ()
}

/// What every provenance must say of its scene: each material's slots hold
/// one `Arc`, no slot belongs to two materials, each slot's prim is known,
/// and each area light's geometry carries the light's own `Emissive`.
fn consistent(scene: &Scene) -> &Provenance {
    let prov = scene
        .provenance
        .as_ref()
        .expect("an imported scene has one");
    let materials = scene.world.materials();
    let mut owner = vec![None; materials.len()];
    for (path, origin) in prov.materials() {
        assert!(!origin.slots.is_empty(), "{path}");
        let first = ptr(&materials[origin.slots[0] as usize]);
        for &slot in &origin.slots {
            assert_eq!(ptr(&materials[slot as usize]), first, "{path} slot {slot}");
            assert!(
                owner[slot as usize].replace(path).is_none(),
                "slot {slot} twice"
            );
            assert!(prov.prim_of_geometry(slot).is_some(), "slot {slot}'s prim");
        }
    }
    for (path, origin) in prov.lights() {
        let light = &scene.lights.lights()[origin.index];
        if let LightKind::Area(area) = light {
            assert_eq!(origin.geoms.clone().count(), 1, "{path}");
            let slot = &materials[origin.geoms.start as usize];
            let emitter: Arc<dyn Material> = area.emitter().clone();
            assert_eq!(ptr(slot), ptr(&emitter), "{path}: one Emissive");
        } else {
            assert!(origin.geoms.is_empty(), "{path}");
        }
    }
    prov
}

#[test]
fn the_cornell_box_maps_every_material_and_light() {
    let scene = Scene::from_usd(&sample("cornellbox.usda")).expect("imports");
    let prov = consistent(&scene);
    let looks = prov
        .material("/scene/Looks/initialShadingGroup")
        .expect("the shared material");
    assert_eq!(looks.slots.len(), 5, "its five meshes");
    assert_eq!(scene.world.count(), 5);
    assert_eq!(prov.light("/scene/Sky").map(|l| l.index), Some(0));
    assert_eq!(prov.camera(), Some("/scene/camera1"));
}

#[test]
fn an_instanced_stage_maps_its_slots() {
    let scene = Scene::from_usd(&sample("instancing.usda")).expect("imports");
    let prov = consistent(&scene);
    assert!(prov.materials().count() >= 2);
    let key = prov.light("/World/Key").expect("the rect light");
    assert_eq!(key.geoms.clone().count(), 1);
    assert_eq!(prov.render_settings(), Some("/Render/settings"));
}

/// A stage streamed one masked chunk at a time (five subtrees under one
/// root) binding one material from every chunk: every slot is mapped to it.
#[test]
fn a_streamed_import_maps_a_shared_material_from_every_chunk() {
    let dir = std::env::temp_dir().join("crust_live_streamed");
    std::fs::create_dir_all(&dir).unwrap();
    let mut stage = String::from(
        "#usda 1.0\ndef Xform \"World\" {\n    def Scope \"Looks\" {\n        \
         def Material \"M\" {\n            token outputs:surface.connect = \
         </World/Looks/M/S.outputs:surface>\n            def Shader \"S\" {\n                \
         uniform token info:id = \"crust:openpbr\"\n                \
         color3f inputs:baseColor = (0.5, 0.2, 0.1)\n                token outputs:surface\n            \
         }\n        }\n    }\n",
    );
    for i in 0..5 {
        stage.push_str(&format!(
            "    def Sphere \"S{i}\" (prepend apiSchemas = [\"MaterialBindingAPI\"]) {{\n        \
             double3 xformOp:translate = ({i}, 0, 0)\n        \
             uniform token[] xformOpOrder = [\"xformOp:translate\"]\n        \
             rel material:binding = </World/Looks/M>\n    }}\n"
        ));
    }
    stage.push_str("}\n");
    let path = dir.join("streamed.usda");
    std::fs::write(&path, stage).unwrap();
    let scene = Scene::from_usd(&path).expect("imports");
    let prov = consistent(&scene);
    let m = prov
        .material("/World/Looks/M")
        .expect("the shared material");
    assert_eq!(m.slots.len(), 5, "one slot per chunk's sphere");
    assert_eq!(prov.prim_of_geometry(m.slots[4]), Some("/World/S4"));
}

fn swap_check(scene: &Scene, slots: &[u32], new: Arc<dyn Material>) -> bool {
    scene.world.check_materials(&[(slots, &new)]).is_ok()
}

/// A colour change keeps every flag the world was built on; a cutout or a
/// start of emission does not.
#[test]
fn a_material_swap_is_refused_when_it_moves_a_flag() {
    let scene = Scene::from_usd(&sample("cornellbox.usda")).expect("imports");
    let slots = scene
        .provenance
        .as_ref()
        .unwrap()
        .material("/scene/Looks/initialShadingGroup")
        .unwrap()
        .slots
        .clone();
    let slots = slots.as_slice();
    let old = scene.world.material(slots[0]);
    assert_eq!(old.kind(), "OpenPBR", "the default grey");

    let recoloured = OpenPBR::diffuse(Vec3A::new(0.1, 0.6, 0.2));
    assert!(swap_check(&scene, slots, Arc::new(recoloured.clone())));

    let cutout = OpenPBR {
        geometry_opacity: 0.5,
        ..recoloured.clone()
    };
    assert!(!swap_check(&scene, slots, Arc::new(cutout)));

    let emitting = OpenPBR {
        emission_luminance: 1.0,
        emission_color: Vec3A::ONE,
        ..recoloured
    };
    assert!(!swap_check(&scene, slots, Arc::new(emitting)));

    let mut world_scene = scene;
    let new: Arc<dyn Material> = Arc::new(OpenPBR::diffuse(Vec3A::new(0.9, 0.1, 0.1)));
    world_scene.world.replace_materials(&[(slots, &new)]);
    for &slot in slots {
        assert_eq!(
            ptr(&world_scene.world.materials()[slot as usize]),
            ptr(&new)
        );
    }
}

/// A light replaced in the list keeps its place, and its `Emissive` is the
/// one its geometry's slot holds, so the light and the surface answer
/// `radiance_toward` alike.
#[test]
fn a_replaced_light_and_its_geometry_share_one_emissive() {
    let mut scene = Scene::from_usd(&sample("instancing.usda")).expect("imports");
    let key = scene
        .provenance
        .as_ref()
        .unwrap()
        .light("/World/Key")
        .unwrap()
        .clone();
    let LightKind::Area(old) = scene.lights.lights()[key.index].clone() else {
        panic!("a rect light is an area light");
    };
    let brighter = Arc::new(Emissive::light(Vec3A::new(5.0, 4.0, 3.0), None));
    // The same shape and geometry, a new emitter: what a rebuild hands back.
    let light = LightKind::Area(crust_core::AreaLight::new(
        old.shape().clone(),
        brighter.clone(),
        key.geoms.start,
    ));
    scene
        .lights
        .check_replace(key.index, &light)
        .expect("same kind and geometry");
    let slot: Arc<dyn Material> = brighter.clone();
    scene
        .world
        .check_materials(&[(&[key.geoms.start], &slot)])
        .expect("an emitter for an emitter");
    scene.lights.replace(key.index, light);
    scene
        .world
        .replace_materials(&[(&[key.geoms.start], &slot)]);

    let LightKind::Area(now) = &scene.lights.lights()[key.index] else {
        unreachable!()
    };
    let in_slot = &scene.world.materials()[key.geoms.start as usize];
    assert_eq!(
        ptr(in_slot),
        ptr(&(now.emitter().clone() as Arc<dyn Material>))
    );
    let (p, dir) = (Vec3A::ZERO, Vec3A::Y);
    assert_eq!(
        now.emitter().radiance_toward(p, dir, true),
        brighter.radiance_toward(p, dir, true)
    );
    assert_eq!(in_slot.emitted(), now.emitter().emitted());

    // A light at infinity cannot take an area light's place.
    let sky = scene
        .provenance
        .as_ref()
        .unwrap()
        .light("/World/Sky")
        .unwrap()
        .index;
    let area = scene.lights.lights()[key.index].clone();
    assert!(scene.lights.check_replace(sky, &area).is_err());
}
