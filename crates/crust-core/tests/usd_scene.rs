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

#[test]
fn loads_fog_usda() {
    let scene = Scene::from_usd(&sample("fog.usda")).expect("failed to open fog.usda");

    // Room mesh BVH + ball sphere + two rect-light triangles; the Fog cube
    // must import as a volume region, NOT as geometry.
    assert_eq!(
        scene.world.count(),
        4,
        "expected 4 hittables (room, ball, 2 light triangles), got {}",
        scene.world.count()
    );
    assert_eq!(scene.lights.count(), 1);
    assert_eq!(scene.volumes.len(), 1, "expected 1 volume region");

    let fog = &scene.volumes[0];
    assert!(fog.is_homogeneous());
    assert!((fog.g - 0.3).abs() < 1e-6);
    assert!((fog.sigma_s - crust_core::Vec3A::splat(0.15)).abs().max_element() < 1e-6);
    // The homogeneous fast path must yield exact Beer-Lambert through the
    // 4-unit room: e^{-(0.15+0.01)·4} in the red channel.
    let mut s = sampler::RngSampler::default();
    let volumes = crust_core::Volumes::new(scene.volumes);
    let ray = crust_core::Ray::new(crust_core::Vec3A::new(0.0, 2.0, 10.0), -crust_core::Vec3A::Z);
    let tr = volumes.transmittance(&ray, 1e-3, 100.0, &mut s);
    let expect = (-(0.15f32 + 0.01) * 4.0).exp();
    assert!(
        (tr.x - expect).abs() < 1e-4,
        "fog transmittance {} vs analytic {}",
        tr.x,
        expect
    );
}

#[test]
fn loads_smoke_usda() {
    let scene = Scene::from_usd(&sample("smoke.usda")).expect("failed to open smoke.usda");

    // Room mesh + two light triangles; all three volume cubes must import
    // as regions, not geometry.
    assert_eq!(
        scene.world.count(),
        3,
        "expected 3 hittables (room, 2 light triangles), got {}",
        scene.world.count()
    );
    assert_eq!(scene.lights.count(), 1);
    assert_eq!(
        scene.volumes.len(),
        3,
        "expected 3 volume regions (smoke, ember, grid puff)"
    );

    // Prim traversal order is an implementation detail — identify the
    // regions by their properties instead.
    let smoke = scene
        .volumes
        .iter()
        .find(|v| !v.is_homogeneous() && (v.g - 0.2).abs() < 1e-6)
        .expect("smoke plume region");
    // densityScale is folded into the coefficients: σs = 0.8 · 12.
    assert!((smoke.sigma_s.x - 9.6).abs() < 1e-4);

    let ember = scene
        .volumes
        .iter()
        .find(|v| v.emission.max_element() > 0.0)
        .expect("emissive ember region");
    assert!(ember.is_homogeneous());

    // The grid puff has positive density at its center, zero at a corner.
    let grid = scene
        .volumes
        .iter()
        .find(|v| !v.is_homogeneous() && v.g.abs() < 1e-6)
        .expect("grid puff region");
    let center = crust_core::Vec3A::new(1.1, 2.6, -0.8);
    assert!(grid.density(center) > 0.3);
    assert!(grid.density(center + crust_core::Vec3A::splat(0.49)) < 1e-3);
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
