use std::path::PathBuf;

use crust_core::Scene;
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
        .bounds()
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
        3,
        "expected 3 geometries (sphere, floor, rect-light mesh), got {}",
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
fn loads_veach_mis_usda() {
    let scene =
        Scene::from_usd(&sample("veach_mis.usda")).expect("failed to open veach_mis.usda");

    // 4 plate meshes + floor + back wall + 4 light spheres.
    assert_eq!(
        scene.world.count(),
        10,
        "expected 10 hittables (4 plates, floor, wall, 4 light spheres), got {}",
        scene.world.count()
    );
    assert_eq!(
        scene.lights.count(),
        4,
        "expected 4 sphere lights, got {}",
        scene.lights.count()
    );
    assert_eq!(scene.settings.get_dimensions(), (960, 540));
    // The scene authors the article's balance heuristic; the token must
    // round-trip through `crust:samplingStrategy` (default is PowerMis, so
    // this fails if parsing silently falls back).
    assert_eq!(
        scene.settings.sampling_strategy(),
        crust_core::SamplingStrategy::BalanceMis
    );
}

#[test]
fn loads_fog_usda() {
    let scene = Scene::from_usd(&sample("fog.usda")).expect("failed to open fog.usda");

    // Room mesh BVH + ball sphere + two rect-light triangles; the Fog cube
    // must import as a volume region, NOT as geometry.
    assert_eq!(
        scene.world.count(),
        3,
        "expected 3 geometries (room, ball, rect-light mesh), got {}",
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
    let mut s = openqmc::pcg::Rng::new(1);
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
        2,
        "expected 2 geometries (room, rect-light mesh), got {}",
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

#[test]
fn loads_curves_usda() {
    let scene = Scene::from_usd(&sample("curves.usda")).expect("failed to open curves.usda");

    // Tuft instance + Tripod instance + floor instance + 2 light triangles.
    assert_eq!(
        scene.world.count(),
        4,
        "expected 4 geometries (2 curve batches, floor, rect-light mesh), got {}",
        scene.world.count()
    );

    // The linear Tripod strand's first segment rises from (1.6, 0, -0.5)
    // to (1.6, 0.8, -0.3) (after the prim's translate); a -Z ray at its
    // mid-height must hit it, and slightly to the side must miss it.
    let on_axis = crust_core::Ray::new(
        crust_core::Vec3A::new(1.6, 0.4, 5.0),
        -crust_core::Vec3A::Z,
    );
    let hit = scene
        .world
        .intersect(&on_axis, 0.001, f32::INFINITY)
        .expect("ray through the strand must hit");
    assert!(
        (hit.rec.t - 5.4).abs() < 0.1,
        "strand hit at t={} (expected ~5.4)",
        hit.rec.t
    );
    let wide = crust_core::Ray::new(
        crust_core::Vec3A::new(2.6, 0.4, 5.0),
        -crust_core::Vec3A::Z,
    );
    // The wide ray flies past every strand and over the floor edge... but the
    // floor extends to z=-8, so aim slightly upward to clear it entirely.
    let wide_up = crust_core::Ray::new(
        crust_core::Vec3A::new(2.6, 0.4, 5.0),
        (crust_core::Vec3A::new(2.6, 3.0, -8.0) - crust_core::Vec3A::new(2.6, 0.4, 5.0)).normalize(),
    );
    assert!(scene.world.intersect(&wide, 0.001, 4.0).is_none());
    assert!(scene.world.intersect(&wide_up, 0.001, f32::INFINITY).is_none());
}

#[test]
fn loads_motionblur_usda() {
    let scene =
        Scene::from_usd(&sample("motionblur.usda")).expect("failed to open motionblur.usda");

    // Mover sphere, Riser cube, floor, shadow card, 2 light triangles.
    assert_eq!(
        scene.world.count(),
        5,
        "expected 5 geometries, got {}",
        scene.world.count()
    );

    // The sphere starts at (-1.5, 0.6, 0) and streaks +1 in x over the
    // shutter: a time-0 ray down its start position hits, a time-1 ray at
    // the same spot misses, and a time-1 ray at the end position hits.
    let at = |x: f32, time: f32| {
        crust_core::Ray::new(
            crust_core::Vec3A::new(x, 0.6, 6.0),
            -crust_core::Vec3A::Z,
        )
        .with_time(time)
    };
    assert!(scene.world.intersect(&at(-1.5, 0.0), 0.001, 5.9).is_some());
    assert!(scene.world.intersect(&at(-1.5, 1.0), 0.001, 5.9).is_none());
    assert!(scene.world.intersect(&at(-0.5, 1.0), 0.001, 5.9).is_some());

    // The shadow card (crust:rayMask = 6) is invisible to camera rays but
    // opaque to shadow rays.
    let down = crust_core::Ray::new(
        crust_core::Vec3A::new(0.0, 5.0, 0.0),
        -crust_core::Vec3A::Y,
    );
    let cam_hit = scene
        .world
        .intersect(&down.clone().with_mask(crust_core::MASK_CAMERA), 0.001, f32::INFINITY)
        .expect("camera ray passes the card and hits the floor");
    assert!(
        (cam_hit.rec.t - 5.0).abs() < 1e-3,
        "camera ray should reach the floor at t=5, got {}",
        cam_hit.rec.t
    );
    let shadow_hit = scene
        .world
        .intersect(&down.clone().with_mask(crust_core::MASK_SHADOW), 0.001, f32::INFINITY)
        .expect("shadow ray must be blocked by the card");
    assert!(
        (shadow_hit.rec.t - 3.0).abs() < 1e-3,
        "shadow ray should stop at the card at t=3, got {}",
        shadow_hit.rec.t
    );
}

/// Instancing, both mechanisms. `samples/instancing.usda` holds three
/// natively-instanced towers (each a two-material prototype) and a
/// `PointInstancer` scattering six gems, one of which `invisibleIds` hides.
#[test]
fn loads_instancing_usda() {
    let scene = Scene::from_usd(&sample("instancing.usda"))
        .expect("failed to open instancing.usda");

    // 5 visible scatter instances + 3 towers x 2 prototype parts + floor
    // + the rect light's geometry.
    assert_eq!(
        scene.world.count(),
        13,
        "expected 13 geometries (5 scatter + 6 tower parts + floor + light), got {}",
        scene.world.count()
    );

    // The whole point: every placement is an instance, so the kernel sees
    // one top-level primitive per placement rather than a copy of the
    // prototype's triangles. The two rect-light triangles and the floor's
    // two are the only non-instanced prims.
    assert!(
        scene.world.primitive_count() <= 16,
        "geometry looks baked, not instanced: {} kernel primitives",
        scene.world.primitive_count()
    );
}

/// A `class` prototype must never be drawn in its own right — only through
/// the instances that reference it. Before instancing support the class's
/// contents rendered at the origin as an extra, phantom object.
#[test]
fn instancing_does_not_draw_the_class_prototype() {
    let scene = Scene::from_usd(&sample("instancing.usda"))
        .expect("failed to open instancing.usda");

    // `/World/_Tower` is authored at the origin. The towers are placed at
    // x = -4.2, -2.2 and -0.4, so nothing should occupy x = 0, and a ray
    // down the tower's height there must reach only the floor.
    let ray = crust_core::Ray::new(
        crust_core::Vec3A::new(0.0, 1.0, 6.0),
        -crust_core::Vec3A::Z,
    );
    assert!(
        scene.world.intersect(&ray, 0.001, 20.0).is_none(),
        "the class prototype was drawn at the origin"
    );
}

/// Instances must land where their transforms put them, and carry the
/// material bound inside the prototype.
#[test]
fn instances_are_placed_and_shaded_per_prototype_part() {
    let scene = Scene::from_usd(&sample("instancing.usda"))
        .expect("failed to open instancing.usda");

    // TowerA sits at x = -4.2 with its block spanning y in [0, 2] and its
    // emerald cap [2, 2.5] (prototype y in [-0.5, 1.5] / [1.5, 2.0], the
    // instance raised by 0.5).
    let shoot = |x: f32, y: f32| {
        crust_core::Ray::new(
            crust_core::Vec3A::new(x, y, 6.0),
            -crust_core::Vec3A::Z,
        )
    };
    let block = scene
        .world
        .intersect(&shoot(-4.2, 1.0), 0.001, 20.0)
        .expect("TowerA's block should be hit at x = -4.2");
    let cap = scene
        .world
        .intersect(&shoot(-4.2, 2.2), 0.001, 20.0)
        .expect("TowerA's cap should be hit above the block");
    assert_ne!(
        block.geom_id, cap.geom_id,
        "block and cap must stay separate geometries so both materials survive"
    );

    // Nothing between the towers.
    assert!(
        scene.world.intersect(&shoot(-3.4, 1.0), 0.001, 20.0).is_none(),
        "unexpected geometry between TowerA and TowerB"
    );

    // TowerC is scaled to 1.4 in y, so its cap reaches higher than
    // TowerA's: prototype y = 2.0 maps to 0.5 + 1.4 * 2.0 = 3.3.
    assert!(
        scene.world.intersect(&shoot(-0.4, 3.0), 0.001, 20.0).is_some(),
        "TowerC's non-uniform scale was not applied"
    );
    assert!(
        scene.world.intersect(&shoot(-4.2, 3.0), 0.001, 20.0).is_none(),
        "unscaled TowerA should not reach y = 3"
    );
}

/// `invisibleIds` prunes instances, and every visible one is placed.
#[test]
fn point_instancer_honours_invisible_ids() {
    let scene = Scene::from_usd(&sample("instancing.usda"))
        .expect("failed to open instancing.usda");

    // Six positions are authored; id 13 — the fourth, at x = 5.4 — is
    // hidden. Shoot straight down each gem's column from y = 4: a gem
    // stops the ray above the floor, its absence lets it run to the floor
    // at exactly t = 4. (The threshold is deliberately just shy of the
    // floor rather than near the gems' tops: per-instance rotations tilt
    // them, so the height at which a column meets a gem varies.)
    let hit_above = |x: f32, z: f32| {
        let ray = crust_core::Ray::new(
            crust_core::Vec3A::new(x, 4.0, z),
            -crust_core::Vec3A::Y,
        );
        scene
            .world
            .intersect(&ray, 0.001, 10.0)
            .is_some_and(|h| h.rec.t < 3.9) // anything above the floor at y = 0
    };
    assert!(hit_above(1.6, 0.0), "gem id 10 missing");
    assert!(hit_above(2.9, -1.1), "gem id 11 missing");
    assert!(hit_above(4.2, 0.4), "gem id 12 missing");
    assert!(!hit_above(5.4, -0.6), "gem id 13 is in invisibleIds but was drawn");
    assert!(hit_above(2.2, 1.6), "gem id 14 missing");
    assert!(hit_above(3.8, 2.1), "gem id 15 missing");
}

/// Nested instancing. `samples/nested_instancing.usda` puts a
/// `PointInstancer` inside another instancer's prototype, and a natively
/// instanced prim inside a second prototype.
#[test]
fn loads_nested_instancing_usda() {
    let scene = Scene::from_usd(&sample("nested_instancing.usda"))
        .expect("failed to open nested_instancing.usda");

    // The Branch prototype expands to 3 parts (leaf, bud husk, bud tip —
    // one per distinct geometry, since a part carries one material), so
    // the outer instancer's 5 placements attach 15. Plus 2 planters x 2
    // parts, the floor and the light.
    assert_eq!(
        scene.world.count(),
        21,
        "expected 21 geometries (5x3 grove + 2x2 planters + floor + light), got {}",
        scene.world.count()
    );
}

/// Nesting must *nest*, not flatten. Each branch's three leaves live in
/// one sub-scene placed once, so a branch costs one top-level primitive
/// per part — not one per leaf. Flattening would multiply the outer
/// instance count by the inner one, which is the blow-up instancing exists
/// to prevent.
#[test]
fn nested_instancing_does_not_flatten() {
    let scene = Scene::from_usd(&sample("nested_instancing.usda"))
        .expect("failed to open nested_instancing.usda");

    // 5 branches x 3 parts + 2 planters x 2 parts + floor (2 tris) +
    // light (2 tris). Flattening the inner instancer would put each of the
    // 5x4 = 20 nested placements at the top level instead.
    assert!(
        scene.world.primitive_count() <= 24,
        "nested instances look flattened: {} kernel primitives",
        scene.world.primitive_count()
    );
}

/// Two levels of instancing must compose transforms, and each nested part
/// must keep the material bound inside the innermost prototype.
#[test]
fn nested_instances_compose_transforms_and_keep_materials() {
    let scene = Scene::from_usd(&sample("nested_instancing.usda"))
        .expect("failed to open nested_instancing.usda");

    // Shoot along -Z through a point, from well in front of the grove.
    let at = |x: f32, y: f32| {
        let ray = crust_core::Ray::new(
            crust_core::Vec3A::new(x, y, 10.0),
            -crust_core::Vec3A::Z,
        );
        scene.world.intersect(&ray, 0.001, 40.0)
    };

    // Branch 0 is at (-6.4, 0, -1), unrotated and unscaled. Its first leaf
    // sits at branch-local (0.45, 1.0, 0) → world (-5.95, 1.0, -1).
    assert!(at(-5.95, 1.0).is_some(), "branch 0's first leaf is missing");
    // ...and nothing a metre to its left, where no leaf was placed.
    assert!(
        at(-5.95, 1.0 + 1.0).is_none(),
        "unexpected geometry above branch 0's first leaf"
    );

    // The bud sits on the branch axis at local y = 3.0, its tip 0.3 above.
    // Both are hit, and they must be *different* geometries: the husk
    // binds Leafy and the tip Blossom, so collapsing the nested prototype
    // into one part would lose a material.
    let husk = at(-6.4, 3.0).expect("branch 0's bud husk is missing");
    let tip = at(-6.4, 3.3).expect("branch 0's bud tip is missing");
    assert_ne!(
        husk.geom_id, tip.geom_id,
        "husk and tip collapsed into one geometry — a material was lost"
    );

    // Branch 1 is scaled 1.25 in y. The bud is on its rotation axis, so
    // the outer scale is the only thing moving it: local y = 3.0 → 3.75,
    // and the tip 3.3 → 4.125. That composes the outer instance's scale
    // with the inner instance's placement, two levels down.
    assert!(
        at(-3.2, 3.75).is_some(),
        "branch 1's bud is not where the outer scale puts it"
    );
    assert!(
        at(-3.2, 4.125).is_some(),
        "branch 1's bud tip is not where the outer scale puts it"
    );
    // Unscaled, it would have been at 3.0 / 3.3 — nothing should be there.
    assert!(
        at(-3.2, 3.3).is_none(),
        "branch 1 was placed as if unscaled"
    );
}

/// A multi-part prototype placed by ordinary native instancing: both
/// planters show their post and their orb, as separate geometries so both
/// materials survive.
#[test]
fn multi_part_prototype_keeps_every_part() {
    let scene = Scene::from_usd(&sample("nested_instancing.usda"))
        .expect("failed to open nested_instancing.usda");

    let at = |x: f32, y: f32| {
        let ray = crust_core::Ray::new(
            crust_core::Vec3A::new(x, y, 10.0),
            -crust_core::Vec3A::Z,
        );
        scene.world.intersect(&ray, 0.001, 40.0)
    };

    for x in [-1.9f32, 1.9] {
        // The post spans y in [0, 1.1] and the orb sits at y = 1.35.
        let post = at(x, 0.6).unwrap_or_else(|| panic!("planter post at x = {x} is missing"));
        let orb = at(x, 1.35).unwrap_or_else(|| panic!("planter orb at x = {x} is missing"));
        assert_ne!(
            post.geom_id, orb.geom_id,
            "post and orb must stay separate geometries so both materials survive"
        );
    }

    // The `class` prototype is authored at the origin and must not be
    // drawn there in its own right.
    assert!(
        at(0.0, 0.6).is_none(),
        "a class prototype was drawn at the origin"
    );
}

/// An `instanceable` prim *inside* another instance's prototype cannot be
/// read at all with openusd 0.5.0, so the importer must skip it rather
/// than abort.
///
/// The upstream bug: resolving such a prim's prototype — or reading the
/// type name of any prim beneath it — reaches
/// `pcp/instancing.rs::materialize_prototype`, whose `debug_assert!`
/// ("materialized prototype root's instanceable must be inert") fires.
/// Debug builds abort; release builds have the assertion compiled out.
/// The prim itself is safe to inspect (`is_instance`, `children`,
/// `type_name` all succeed) — only its *contents* are unreachable, which
/// is why there is no proxy-traversal fallback either.
///
/// It is a property of the composed stage, not of crust: it reproduces
/// with `class` and `def` prototypes alike, and single-level native
/// instancing and nested `PointInstancer`s are both unaffected.
///
/// This test pins graceful degradation — the outer instance still renders,
/// the unreadable inner one is dropped with a warning. When upstream fixes
/// it, this test will start seeing the inner geometry and should be
/// replaced with one asserting the nested content *is* imported.
#[test]
fn nested_native_instance_degrades_gracefully() {
    let dir = std::env::temp_dir().join("crust_nested_native_probe");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("nested_native.usda");
    std::fs::write(
        &path,
        r#"#usda 1.0
(defaultPrim = "W")
def Xform "W" {
    class Xform "_Inner" { def Sphere "s" { double radius = 0.5 } }
    class Xform "_Outer" {
        def Sphere "outer" { double radius = 0.4 }
        def Xform "i" (instanceable = true; references = </W/_Inner>) {
            double3 xformOp:translate = (3, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
    }
    def Xform "A" (instanceable = true; references = </W/_Outer>) {}
}
"#,
    )
    .expect("write probe stage");

    // The load must complete. Before the guard this aborted the process.
    let scene = Scene::from_usd(&path).expect("stage with a nested native instance must load");

    // The outer prototype's own geometry survives; only the unreadable
    // nested instance is missing.
    assert_eq!(
        scene.world.count(),
        1,
        "expected the outer sphere only (the nested instance is unreadable upstream), got {}",
        scene.world.count()
    );

    let hit = |x: f32| {
        let ray = crust_core::Ray::new(
            crust_core::Vec3A::new(x, 0.0, 10.0),
            -crust_core::Vec3A::Z,
        );
        scene.world.intersect(&ray, 0.001, 40.0).is_some()
    };
    assert!(hit(0.0), "the outer prototype's own sphere should render");
    assert!(
        !hit(3.0),
        "the nested instance is expected to be missing — if this now hits, \
         openusd has been fixed and the skip in collect_proto_parts can go"
    );

    let _ = std::fs::remove_file(&path);
}

/// A host that decodes nothing real, so the importer's asset plumbing can
/// be tested without an image dependency in `crust-core`: it records what
/// was asked for and hands back a synthetic two-texel map.
struct FakeAssets {
    requested: std::sync::Mutex<Vec<PathBuf>>,
}

impl crust_core::AssetLoader for FakeAssets {
    fn load_environment(&self, path: &std::path::Path) -> Option<crust_core::EnvironmentMap> {
        self.requested.lock().unwrap().push(path.to_path_buf());
        crust_core::EnvironmentMap::new(
            2,
            1,
            vec![
                crust_core::Vec3A::new(9.0, 0.0, 0.0),
                crust_core::Vec3A::new(0.0, 9.0, 0.0),
            ],
        )
    }
}

/// Lights at infinity carry no scene geometry, so they must reach the light
/// list without adding hittables.
#[test]
fn loads_domelight_usda() {
    let scene = Scene::from_usd(&sample("domelight.usda"))
        .expect("failed to open domelight.usda");

    assert_eq!(
        scene.lights.count(),
        2,
        "expected the dome and the distant sun, got {}",
        scene.lights.count()
    );
    // Two spheres and the floor — neither infinite light contributes
    // geometry.
    assert_eq!(
        scene.world.count(),
        3,
        "infinite lights must not add hittables, got {} geometries",
        scene.world.count()
    );
}

/// `inputs:texture:file` is resolved against the USD layer's directory and
/// handed to the host — `crust-core` never opens the file itself.
#[test]
fn dome_texture_is_resolved_and_requested_from_the_host() {
    let assets = FakeAssets {
        requested: std::sync::Mutex::new(Vec::new()),
    };
    let scene = Scene::from_usd_with_assets(&sample("domelight.usda"), &assets)
        .expect("failed to open domelight.usda");
    assert_eq!(scene.lights.count(), 2);

    let requested = assets.requested.lock().unwrap();
    assert_eq!(
        requested.len(),
        1,
        "expected exactly one environment request, got {requested:?}"
    );
    let path = &requested[0];
    assert!(
        path.ends_with("sky_env.exr"),
        "unexpected asset requested: {}",
        path.display()
    );
    assert!(
        path.is_absolute() || path.exists(),
        "the relative asset path was not resolved against the layer: {}",
        path.display()
    );
    assert!(
        path.exists(),
        "resolved path does not point at the checked-in map: {}",
        path.display()
    );
}

/// Both infinite lights must answer for escaping rays — that is the only
/// way a bounce ray can find them — and neither may claim scene geometry.
#[test]
fn infinite_lights_are_found_by_escaping_rays() {
    let scene = Scene::from_usd(&sample("domelight.usda"))
        .expect("failed to open domelight.usda");

    let mut dome_like = 0;
    let mut cone_like = 0;
    for light in &scene.lights.lights {
        assert_eq!(
            light.geom_id(),
            None,
            "a light at infinity must not claim scene geometry"
        );
        // A dome covers every direction; the sun covers only its cone.
        let covered = [
            crust_core::Vec3A::Y,
            -crust_core::Vec3A::Y,
            crust_core::Vec3A::X,
            -crust_core::Vec3A::Z,
        ]
        .iter()
        .filter(|d| light.escaped(crust_core::Vec3A::ZERO, **d).is_some())
        .count();
        if covered == 4 {
            dome_like += 1;
        } else {
            cone_like += 1;
        }

        // Whatever it is, sampling it must agree with `escaped` about the
        // pdf — the two MIS sides of one strategy.
        let s = light
            .sample_li(crust_core::Vec3A::ZERO, 0.37, 0.62)
            .expect("an infinite light is reachable from anywhere");
        assert!(s.distance.is_infinite(), "a light at infinity cannot be occluded");
        let (_, pdf) = light
            .escaped(crust_core::Vec3A::ZERO, s.direction)
            .expect("sample_li produced a direction escaped() does not cover");
        assert!(
            (pdf - s.pdf).abs() <= 1e-3 * s.pdf.max(pdf),
            "MIS sides disagree: sample_li {} vs escaped {}",
            s.pdf,
            pdf
        );
    }
    assert_eq!(dome_like, 1, "expected exactly one all-direction dome");
    assert_eq!(cone_like, 1, "expected exactly one cone-shaped distant light");
}
