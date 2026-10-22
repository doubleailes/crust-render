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

/// What a geometry's table was decoded for is part of what a swap must keep:
/// a material that starts reading a primvar is refused on a curves slot (its
/// strand table lacks the primvar) and on a mesh's (its primvar table does).
#[test]
fn a_material_swap_is_refused_when_it_reads_a_new_primvar() {
    let dir = std::env::temp_dir().join("crust_live_curve_swap");
    std::fs::create_dir_all(&dir).unwrap();
    let mtlx = dir.join("looks.mtlx");
    std::fs::write(
        &mtlx,
        r#"<?xml version="1.0"?>
<materialx version="1.39">
  <constant name="grey" type="color3"><input name="value" type="color3" value="0.5, 0.5, 0.5" /></constant>
  <constant name="red" type="color3"><input name="value" type="color3" value="0.9, 0.1, 0.1" /></constant>
  <geompropvalue name="tint" type="color3"><input name="geomprop" type="string" value="tint" /></geompropvalue>
  <standard_surface name="s_plain" type="surfaceshader"><input name="base_color" type="color3" nodename="grey" /></standard_surface>
  <standard_surface name="s_red" type="surfaceshader"><input name="base_color" type="color3" nodename="red" /></standard_surface>
  <standard_surface name="s_reads" type="surfaceshader"><input name="base_color" type="color3" nodename="tint" /></standard_surface>
  <surfacematerial name="plain" type="material"><input name="surfaceshader" type="surfaceshader" nodename="s_plain" /></surfacematerial>
  <surfacematerial name="red" type="material"><input name="surfaceshader" type="surfaceshader" nodename="s_red" /></surfacematerial>
  <surfacematerial name="reads" type="material"><input name="surfaceshader" type="surfaceshader" nodename="s_reads" /></surfacematerial>
</materialx>
"#,
    )
    .unwrap();
    let stage = dir.join("stage.usda");
    std::fs::write(
        &stage,
        format!(
            r#"#usda 1.0
def Xform "World"
{{
    def Material "Plain" (prepend references = @{}@</MaterialX/Materials/plain>) {{ }}
    def BasisCurves "Curve" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token type = "linear"
        int[] curveVertexCounts = [2]
        point3f[] points = [(0, 0, 0), (1, 0, 0)]
        rel material:binding = </World/Plain>
    }}
    def Mesh "Card" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 2, 0), (1, 2, 0), (0, 3, 0)]
        rel material:binding = </World/Plain>
    }}
}}
"#,
            mtlx.display()
        ),
    )
    .unwrap();
    let scene = Scene::from_usd(&stage).expect("imports");
    let prov = consistent(&scene);
    let slot = |prim: &str| {
        let slots = &prov.material("/World/Plain").expect("bound").slots;
        *slots
            .iter()
            .find(|&&s| prov.prim_of_geometry(s) == Some(prim))
            .unwrap_or_else(|| panic!("{prim}'s slot"))
    };
    let (curve, card) = (slot("/World/Curve"), slot("/World/Card"));
    let load = |name: &str| -> Arc<dyn Material> {
        crust_core::materialx::load(
            &mtlx,
            Some(name),
            &crust_core::mtlx::Host::new(&|_, _| None),
        )
        .unwrap_or_else(|e| panic!("{name}: {e:?}"))
        .material
    };
    assert!(swap_check(&scene, &[curve], load("red")), "same needs");
    assert!(
        !swap_check(&scene, &[curve], load("reads")),
        "a new primvar"
    );
    assert!(
        !swap_check(&scene, &[card], load("reads")),
        "a new mesh primvar"
    );
    assert!(
        swap_check(&scene, &[card], load("red")),
        "same needs on a mesh"
    );
}

/// A stage with a mesh authoring `primvars:rest`, whose settings target
/// `products` (one of `</Render/plain>` and `</Render/withRest>`).
fn primvar_stage(products: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_live_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("live_primvars.usda");
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
    def Mesh "M"
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)]
        point3f[] primvars:rest = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)] (
            interpolation = "vertex"
        )
    }}
}}
def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </W/Cam>
        uniform int2 resolution = (32, 32)
        rel products = [{products}]
    }}
    def RenderProduct "plain"
    {{
        token productName = "plain.exr"
        rel orderedVars = [</Render/depth>]
    }}
    def RenderProduct "withRest"
    {{
        token productName = "rest.exr"
        rel orderedVars = [</Render/depth>, </Render/Pref>]
    }}
    def RenderVar "depth"
    {{
        uniform token dataType = "float"
        uniform string sourceName = "depth"
    }}
    def RenderVar "Pref"
    {{
        uniform token dataType = "point3f"
        uniform string sourceName = "rest"
        uniform token sourceType = "primvar"
    }}
}}
"#
        ),
    )
    .expect("write stage");
    path
}

/// The geometry keeps the primvars the products asked for when it was read:
/// retargeting `products` to one with a primvar var re-imports, rather than
/// render a channel the geometry never kept; the same products update in
/// place. A fresh import of the retargeted layer keeps the primvar.
#[test]
fn a_settings_change_asking_for_new_primvars_reimports() {
    let change = crust_core::live::Change {
        settings: true,
        ..Default::default()
    };
    let path = primvar_stage("</Render/plain>");
    let scene = Scene::from_usd(&path).expect("load");
    assert!(!scene.world.has_primvars());
    let prov = scene.provenance.as_ref().expect("provenance");
    assert_eq!(
        prov.rebuild(&path, &crust_core::NoAssets, &scene.warnings, &change)
            .map(|_| ()),
        Ok(())
    );
    primvar_stage("</Render/withRest>");
    match prov.rebuild(&path, &crust_core::NoAssets, &scene.warnings, &change) {
        Err(crust_core::live::Structural(why)) => assert!(why.contains("primvars"), "{why}"),
        Ok(_) => panic!("a product asking for a primvar the geometry never kept updated in place"),
    }
    let fresh = Scene::from_usd(&path).expect("load");
    assert!(fresh.world.has_primvars());
    // And with the primvar kept, the same products update in place.
    let prov = fresh.provenance.as_ref().expect("provenance");
    assert_eq!(
        prov.rebuild(&path, &crust_core::NoAssets, &fresh.warnings, &change)
            .map(|_| ()),
        Ok(())
    );
}

/// The primvar warnings are raised reading geometry, for the primvars it
/// kept, which a settings refresh cannot change without being refused: a
/// scene carrying them still takes a settings edit in place.
#[test]
fn a_primvar_warning_does_not_block_a_settings_edit() {
    let change = crust_core::live::Change {
        settings: true,
        ..Default::default()
    };
    let path = primvar_stage("</Render/withRest>");
    let text = std::fs::read_to_string(&path).expect("read stage");
    // `rest` moves to the mesh's parent (not inherited), and a second mesh
    // authors it with two components (a mismatch).
    let text = text
        .replace(
            "def Xform \"W\"\n{",
            "def Xform \"W\"\n{\n    point3f primvars:rest = (0, 0, 0) (interpolation = \"constant\")",
        )
        .replace(
            "point3f[] primvars:rest = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)] (\n            interpolation = \"vertex\"\n        )",
            "",
        )
        .replace(
            "    def Mesh \"M\"",
            "    def Mesh \"N\"\n    {\n        int[] faceVertexCounts = [3]\n        int[] faceVertexIndices = [0, 1, 2]\n        point3f[] points = [(2, 0, 0), (3, 0, 0), (2, 1, 0)]\n        float2[] primvars:rest = [(0, 0)] (interpolation = \"constant\")\n    }\n    def Mesh \"M\"",
        );
    let path = path.with_file_name("live_primvar_warnings.usda");
    std::fs::write(&path, text).expect("write stage");
    let scene = Scene::from_usd(&path).expect("load");
    let codes: Vec<&str> = scene.warnings.iter().map(|w| w.code.as_str()).collect();
    assert!(codes.contains(&"primvar.not_inherited"), "{codes:?}");
    assert!(codes.contains(&"primvar.mismatch"), "{codes:?}");
    let prov = scene.provenance.as_ref().expect("provenance");
    assert_eq!(
        prov.rebuild(&path, &crust_core::NoAssets, &scene.warnings, &change)
            .map(|_| ()),
        Ok(())
    );
}

/// A stage rendering a sphere under a dome through `/W/Cam`, a 36 × 24
/// aperture on a 2:1 image, its settings authoring `extra`; a Catmull-Clark
/// cube refined adaptively when `adaptive`.
fn framing_stage(name: &str, extra: &str, adaptive: bool) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_live_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    let (cage, edge) = if adaptive {
        (
            r#"
    def Mesh "Cage"
    {
        uniform token subdivisionScheme = "catmullClark"
        int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]
        int[] faceVertexIndices = [0, 1, 3, 2, 2, 3, 5, 4, 4, 5, 7, 6, 6, 7, 1, 0, 1, 7, 5, 3, 6, 0, 2, 4]
        point3f[] points = [(-1, -1, 1), (1, -1, 1), (-1, 1, 1), (1, 1, 1), (-1, 1, -1), (1, 1, -1), (-1, -1, -1), (1, -1, -1)]
        double3 xformOp:translate = (2, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#,
            "float crust:subdivisionEdgeLength = 2",
        )
    } else {
        ("", "")
    };
    std::fs::write(
        &path,
        format!(
            r#"#usda 1.0
def Xform "W"
{{
    def Camera "Cam"
    {{
        float horizontalAperture = 36
        float verticalAperture = 24
        double3 xformOp:translate = (0, 0, 10)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def Sphere "Ball" {{}}
    def DomeLight "Sky" {{}}{cage}
}}
def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </W/Cam>
        uniform int2 resolution = (64, 32)
        {edge}
        {extra}
    }}
}}
"#
        ),
    )
    .expect("write stage");
    path
}

fn beauty_bits(scene: Scene) -> Vec<[u32; 3]> {
    let settings = scene.settings.with_samples_per_pixel(16).with_max_depth(3);
    let buffer =
        crust_core::Renderer::new(scene.camera, scene.world, scene.lights, settings).render();
    let (w, h) = buffer.size();
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| buffer.get_pixel(x, y).to_array().map(f32::to_bits))
        .collect()
}

const HEIGHT_FIT: &str = "uniform token aspectRatioConformPolicy = \"adjustApertureHeight\"";

/// An edit of `aspectRatioConformPolicy` alone re-frames the camera in
/// place when no geometry read the camera, and renders bit for bit what a
/// fresh import of the edited layer renders.
#[test]
fn a_conform_policy_edit_applies_in_place_as_a_reimport_would() {
    let change = crust_core::live::Change {
        settings: true,
        ..Default::default()
    };
    let path = framing_stage("live_framing", "", false);
    let before = beauty_bits(Scene::from_usd(&path).expect("load"));
    let mut scene = Scene::from_usd(&path).expect("load");
    framing_stage("live_framing", HEIGHT_FIT, false);
    let update = scene
        .provenance
        .as_ref()
        .expect("provenance")
        .rebuild(&path, &crust_core::NoAssets, &scene.warnings, &change)
        .expect("a policy edit applies in place");
    assert!(update.has_settings());
    update.apply(&mut scene).expect("applies");
    let fresh = Scene::from_usd(&path).expect("load the edited layer");
    assert_eq!(format!("{:?}", scene.camera), format!("{:?}", fresh.camera));
    let after = beauty_bits(scene);
    assert_eq!(after, beauty_bits(fresh));
    assert_ne!(after, before, "the edit re-framed the render");
}

/// Adaptive subdivision tessellated for the old frame, so a policy edit that
/// moves the conformed aperture re-imports.
#[test]
fn a_conform_policy_edit_reimports_adaptive_subdivision() {
    let change = crust_core::live::Change {
        settings: true,
        ..Default::default()
    };
    let path = framing_stage("live_framing_adaptive", "", true);
    let scene = Scene::from_usd(&path).expect("load");
    assert!(scene.stats.subdivision.adaptive.is_some(), "adaptive");
    let prov = scene.provenance.as_ref().expect("provenance");
    assert_eq!(
        prov.rebuild(&path, &crust_core::NoAssets, &scene.warnings, &change)
            .map(|_| ()),
        Ok(())
    );
    framing_stage("live_framing_adaptive", HEIGHT_FIT, true);
    match prov.rebuild(&path, &crust_core::NoAssets, &scene.warnings, &change) {
        Err(crust_core::live::Structural(why)) => {
            assert!(why.contains("another camera projection"), "{why}")
        }
        Ok(_) => panic!("adaptive subdivision for another frame was updated in place"),
    }
}

/// `framing_stage`'s adaptive stage with a second camera, `/W/Dicer`, which
/// `crust:dicingCamera` names when `dicing` is given; the render camera at
/// `cam_z`.
fn dicing_stage(name: &str, dicing: Option<&str>, cam_z: f32) -> PathBuf {
    let path = framing_stage(name, "", true);
    let text = std::fs::read_to_string(&path).expect("read stage");
    let rel = dicing.map_or(String::new(), |d| format!("rel crust:dicingCamera = <{d}>"));
    let text = text
        .replace(
            "double3 xformOp:translate = (0, 0, 10)",
            &format!("double3 xformOp:translate = (0, 0, {cam_z})"),
        )
        .replace(
            "    def Sphere \"Ball\" {}",
            "    def Camera \"Dicer\"\n    {\n        float horizontalAperture = 36\n        \
             float verticalAperture = 24\n        double3 xformOp:translate = (0, 1, 12)\n        \
             uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    }\n    \
             def Sphere \"Ball\" {}",
        )
        .replace(
            "rel camera = </W/Cam>",
            &format!("rel camera = </W/Cam>\n        {rel}"),
        );
    std::fs::write(&path, text).expect("write stage");
    path
}

/// With a dicing camera, moving the render camera applies in place as a
/// fresh import would; retargeting `crust:dicingCamera` is structural.
#[test]
fn a_dicing_camera_keeps_a_render_camera_move_in_place() {
    let change = crust_core::live::Change {
        settings: true,
        ..Default::default()
    };
    let name = "live_dicing";
    let path = dicing_stage(name, Some("/W/Dicer"), 10.0);
    let mut scene = Scene::from_usd(&path).expect("load");
    assert!(scene.stats.subdivision.adaptive.is_some(), "adaptive");
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
    let before = format!("{:?}", scene.camera);
    dicing_stage(name, Some("/W/Dicer"), 7.0);
    let prov = scene.provenance.clone().expect("provenance");
    let update = prov
        .rebuild(&path, &crust_core::NoAssets, &scene.warnings, &change)
        .expect("a render camera move applies in place");
    update.apply(&mut scene).expect("applies");
    assert_ne!(format!("{:?}", scene.camera), before, "the camera moved");
    let fresh = Scene::from_usd(&path).expect("load the edited layer");
    assert_eq!(format!("{:?}", scene.camera), format!("{:?}", fresh.camera));
    assert_eq!(beauty_bits(scene), beauty_bits(fresh));

    // Without a dicing camera the same move re-tessellates.
    let path = dicing_stage("live_dicing_none", None, 10.0);
    let scene = Scene::from_usd(&path).expect("load");
    dicing_stage("live_dicing_none", None, 7.0);
    let prov = scene.provenance.as_ref().expect("provenance");
    match prov.rebuild(&path, &crust_core::NoAssets, &scene.warnings, &change) {
        Err(crust_core::live::Structural(why)) => {
            assert!(why.contains("another camera projection"), "{why}")
        }
        Ok(_) => panic!("a move of the dicing render camera was updated in place"),
    }

    // Retargeting the relationship moves what tessellation reads.
    let path = dicing_stage("live_dicing_retarget", Some("/W/Dicer"), 10.0);
    let scene = Scene::from_usd(&path).expect("load");
    dicing_stage("live_dicing_retarget", Some("/W/Cam"), 10.0);
    let prov = scene.provenance.as_ref().expect("provenance");
    match prov.rebuild(&path, &crust_core::NoAssets, &scene.warnings, &change) {
        Err(crust_core::live::Structural(why)) => {
            assert!(why.contains("the dicing camera changed"), "{why}")
        }
        Ok(_) => panic!("a retargeted dicing camera was updated in place"),
    }
}
