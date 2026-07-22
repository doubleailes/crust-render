use std::path::PathBuf;

use crust_core::{Hittable, Scene};
use openusd::schemas::shade::{Material as UsdMaterial, MaterialBindingAPI};
use openusd::sdf;
use openusd::usd::{PrimPredicate, Stage};

fn sample(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crust-core is at <workspace>/crates/crust-core → samples/ two dirs up.
    root.parent().unwrap().parent().unwrap().join("samples").join(name)
}

#[test]
fn loads_cornellbox_usda() {
    let scene = Scene::from_usd(&sample("cornellbox.usda"))
        .expect("failed to open cornellbox.usda");

    // The Cornell box fixture ships with meshes; whatever material dispatch
    // ends up doing, we should have at least one hittable in the world.
    assert!(
        scene.world.count() > 0,
        "no hittables imported from cornellbox.usda"
    );

    // Render settings should have positive dimensions after fallback.
    let (w, h) = scene.settings.get_dimensions();
    assert!(w > 0 && h > 0, "resolved dimensions must be positive");

    // Print diagnostics on failure (only visible with --nocapture)
    eprintln!(
        "cornellbox: world={} lights={} dims={:?}",
        scene.world.count(),
        scene.lights.count(),
        (w, h),
    );
}

#[test]
fn loads_openpbr_showcase_usda() {
    let scene = Scene::from_usd(&sample("openpbr_showcase.usda"))
        .expect("failed to open openpbr_showcase.usda");

    // 8 spheres in the scene (1 ground + 7 material spheres) plus 2 sphere
    // lights whose geometry is also added → 10 hittables. Allow slack for
    // future changes: at minimum both light spheres and both material spheres
    // should be there.
    assert!(
        scene.world.count() >= 10,
        "expected at least 10 hittables, got {}",
        scene.world.count()
    );
    // Two SphereLights → two Light entries.
    assert_eq!(
        scene.lights.count(),
        2,
        "expected 2 lights (SphereLight × 2), got {}",
        scene.lights.count()
    );
    // RenderSettings authored 640×360.
    assert_eq!(scene.settings.get_dimensions(), (640, 360));

    eprintln!(
        "openpbr_showcase: world={} lights={} dims={:?}",
        scene.world.count(),
        scene.lights.count(),
        scene.settings.get_dimensions(),
    );
}

/// Regression guard for xformOp composition: the Maya-authored Cornell box
/// (`pCube1`: translate `(0,2,0)` then scale 4, i.e. a multi-op stack)
/// must land with its shell spanning x,z ∈ [-2,2] and y ∈ [0,4]. openusd
/// 0.5.0's `local_to_parent_transform` composes such stacks in the wrong
/// order (translation came back scaled → shell at y ∈ [6,10], props shrunk
/// toward the origin), which is why `usd_import` composes the individual
/// `xformOp:*` attributes itself.
#[test]
fn cornellbox_transforms_compose_correctly() {
    let scene = Scene::from_usd(&sample("cornellbox.usda"))
        .expect("failed to open cornellbox.usda");
    let bbox = scene
        .world
        .bounding_box()
        .expect("cornellbox world must be bounded");

    let tol = 0.1;
    assert!(
        (bbox.minimum.y).abs() < tol && (bbox.maximum.y - 4.0).abs() < tol,
        "box shell must span y in [0, 4], got [{}, {}]",
        bbox.minimum.y,
        bbox.maximum.y
    );
    for (min, max, axis) in [
        (bbox.minimum.x, bbox.maximum.x, "x"),
        (bbox.minimum.z, bbox.maximum.z, "z"),
    ] {
        assert!(
            (min + 2.0).abs() < tol && (max - 2.0).abs() < tol,
            "box shell must span {axis} in [-2, 2], got [{min}, {max}]"
        );
    }
}

#[test]
fn loads_rectlight_usda() {
    let scene =
        Scene::from_usd(&sample("rectlight.usda")).expect("failed to open rectlight.usda");

    // Ball sphere + floor mesh BVH + two triangles of rect-light geometry.
    assert_eq!(
        scene.world.count(),
        4,
        "expected 4 hittables (sphere, floor, 2 light triangles), got {}",
        scene.world.count()
    );
    // The RectLight must import as a real light, not warn-and-skip.
    assert_eq!(
        scene.lights.count(),
        1,
        "expected 1 light (RectLight), got {}",
        scene.lights.count()
    );
    assert_eq!(scene.settings.get_dimensions(), (64, 64));
}

/// Regression guard: every material in the ported showcase must decode to
/// the `crust:openpbr` shader id, and every scene sphere must bind one of
/// them. When this test drifts (renamed shader ids, missing material
/// binding, openusd stops surfacing `info:id`), the loader silently falls
/// back to a grey diffuse OpenPBR — which is what happened before this fix.
#[test]
fn openpbr_showcase_materials_all_decode() {
    let stage = Stage::open(sample("openpbr_showcase.usda").to_str().unwrap())
        .expect("open showcase stage");

    let mut prims: Vec<sdf::Path> = Vec::new();
    stage
        .traverse(PrimPredicate::DEFAULT_PROXIES, |p| prims.push(p.clone()))
        .unwrap();

    // Every Material's surface shader must resolve to `crust:openpbr`.
    let mut mats = 0;
    for p in &prims {
        if let Ok(Some(mat)) = UsdMaterial::get(&stage, p.clone()) {
            let shader = mat
                .compute_surface_source()
                .unwrap()
                .unwrap_or_else(|| panic!("Material {} has no surface shader", p));
            let id = shader
                .id()
                .unwrap()
                .unwrap_or_else(|| panic!("Shader at {} has no info:id", shader.path()));
            assert_eq!(
                id, "crust:openpbr",
                "Material {} shader id was {:?}, expected `crust:openpbr`",
                p, id
            );
            mats += 1;
        }
    }
    assert_eq!(mats, 7, "expected 7 authored materials, saw {}", mats);

    // Every sphere prim under /World/Scene except the ground must bind one.
    let mut bound = 0;
    for p in &prims {
        if let Ok(Some(bind)) = MaterialBindingAPI::get(&stage, p.clone()) {
            if let Ok(Some(_mat_path)) = bind.direct_binding("") {
                bound += 1;
            }
        }
    }
    assert_eq!(
        bound, 7,
        "expected 7 bound spheres (ground has no binding), saw {}",
        bound
    );
}
