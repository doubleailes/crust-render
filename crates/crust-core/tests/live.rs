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

/// A stage rendering through a named camera, holding `shape`.
fn shape_stage(name: &str, shape: &str) -> PathBuf {
    shape_stage_naming(name, shape, true)
}

/// A stage holding `shape` and one camera, which `RenderSettings.camera`
/// names when `named`.
fn shape_stage_naming(name: &str, shape: &str, named: bool) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_live_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(
        &path,
        format!(
            r#"#usda 1.0
def Xform "W"
{{
    def Camera "Cam"
    {{
        double3 xformOp:translate = (0, 0, 10)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def {shape} "Shape" {{}}
}}
def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        {}
        uniform int2 resolution = (64, 32)
    }}
}}
"#,
            if named { "rel camera = </W/Cam>" } else { "" }
        ),
    )
    .expect("write stage");
    path
}

/// A curved implicit shape sized from the render camera moves with the
/// camera and the resolution, so a settings change that moves the projection
/// re-imports rather than leave it tessellated for the old view; one that
/// leaves the projection as it was (an exposure) updates in place, and so
/// does anything on a cube, which has no radial count.
#[test]
fn a_settings_change_reimports_shapes_sized_on_screen() {
    let change = crust_core::live::Change {
        settings: true,
        ..Default::default()
    };
    let rebuild = |path: &Path| {
        let scene = Scene::from_usd(path).expect("load");
        let prov = scene.provenance.as_ref().expect("provenance");
        prov.rebuild(path, &crust_core::NoAssets, &scene.warnings, &change)
            .map(|_| ())
    };
    let cylinder = shape_stage("live_cylinder", "Cylinder");
    let scene = Scene::from_usd(&cylinder).expect("load");
    let prov = scene.provenance.as_ref().expect("provenance");
    // The same projection: in place.
    assert_eq!(
        prov.rebuild(&cylinder, &crust_core::NoAssets, &scene.warnings, &change)
            .map(|_| ()),
        Ok(())
    );
    // The edit: another resolution, so another pixel rate.
    let text = std::fs::read_to_string(&cylinder).expect("read stage");
    std::fs::write(&cylinder, text.replace("(64, 32)", "(128, 64)")).expect("write stage");
    match prov.rebuild(&cylinder, &crust_core::NoAssets, &scene.warnings, &change) {
        Err(crust_core::live::Structural(why)) => assert!(why.contains("implicit"), "{why}"),
        Ok(_) => panic!("a cylinder sized for another projection was updated in place"),
    }
    let cube = shape_stage("live_cube", "Cube");
    assert_eq!(rebuild(&cube), Ok(()));
}

/// A cylinder that took the fixed radial count because no camera was named
/// is sized from the screen by a fresh import once an edit names the camera
/// the scene already rendered through, so that settings change re-imports
/// too.
#[test]
fn naming_the_camera_reimports_shapes_sized_without_one() {
    let change = crust_core::live::Change {
        settings: true,
        ..Default::default()
    };
    let path = shape_stage_naming("live_cylinder_unnamed", "Cylinder", false);
    let scene = Scene::from_usd(&path).expect("load");
    assert_eq!(
        scene.camera_path.as_deref(),
        Some("/W/Cam"),
        "the first camera"
    );
    let prov = scene.provenance.as_ref().expect("provenance");
    // Unchanged, the settings still update in place.
    assert_eq!(
        prov.rebuild(&path, &crust_core::NoAssets, &scene.warnings, &change)
            .map(|_| ()),
        Ok(())
    );
    // The edit: the same layer, now naming that camera.
    shape_stage_naming("live_cylinder_unnamed", "Cylinder", true);
    match prov.rebuild(&path, &crust_core::NoAssets, &scene.warnings, &change) {
        Err(crust_core::live::Structural(why)) => assert!(why.contains("implicit"), "{why}"),
        Ok(_) => panic!("a cylinder sized without a camera was updated in place"),
    }
}

/// Light links take a light that illuminates nothing out of the list and
/// renumber the rest: the provenance names each light by its index after
/// that, and keeps none for the light taken out.
#[test]
fn light_indices_are_the_list_as_the_links_left_it() {
    let dir = std::env::temp_dir().join("crust_live_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let demoting = dir.join("live_demoted_light.usda");
    // `Dead` sits between two kept lights on either side, so whichever order
    // the traversal meets them in, two kept lights come after it.
    let light = |name: &str, x: f32, link: &str| {
        format!(
            r#"    def SphereLight "{name}"
    {{
        float inputs:radius = 0.2
        float inputs:intensity = 10
        double3 xformOp:translate = ({x}, 3, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        {link}
    }}
"#
        )
    };
    let nothing = "uniform bool collection:lightLink:includeRoot = 0";
    let lights: String = [
        light("K1", -2.0, ""),
        light("K2", -1.0, ""),
        light("Dead", 0.0, nothing),
        light("K3", 1.0, ""),
        light("K4", 2.0, ""),
    ]
    .concat();
    std::fs::write(
        &demoting,
        format!(
            r#"#usda 1.0
def Xform "W"
{{
{lights}
    def Mesh "Floor"
    {{
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-4, 0, 4), (4, 0, 4), (4, 0, -4), (-4, 0, -4)]
    }}
}}
"#
        ),
    )
    .expect("write stage");
    for path in [sample("light_linking.usda"), demoting.clone()] {
        let scene = Scene::from_usd(&path).expect("imports");
        let prov = consistent(&scene);
        let mut seen = 0;
        for (prim, origin) in prov.lights() {
            assert_eq!(
                scene.lights.name(origin.index),
                Some(prim),
                "{}: light {}",
                path.display(),
                origin.index
            );
            seen += 1;
        }
        assert_eq!(seen, scene.lights.count(), "{}", path.display());
    }
    let scene = Scene::from_usd(&demoting).expect("imports");
    let prov = scene.provenance.as_ref().unwrap();
    assert_eq!(scene.lights.count(), 4, "Dead illuminates nothing");
    assert!(prov.light("/W/Dead").is_none());
}
