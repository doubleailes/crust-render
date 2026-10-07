use std::path::PathBuf;

use crust_core::{Light, Ray, Scene, Vec3A};
use openusd::sdf;
use openusd::usd::{PrimPredicate, Stage};
use openusd_schemas::shade::{Material as UsdMaterial, MaterialBindingAPI, TerminalSource};

fn sample(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crust-core is at <workspace>/crates/crust-core → samples/ two dirs up.
    root.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("samples")
        .join(name)
}

#[test]
fn loads_cornellbox_usda() {
    let scene =
        Scene::from_usd(&sample("cornellbox.usda")).expect("failed to open cornellbox.usda");

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
    // Two SphereLights and the sky dome → three Light entries.
    assert_eq!(
        scene.lights.count(),
        3,
        "expected 3 lights (SphereLight × 2 + DomeLight), got {}",
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
    let scene =
        Scene::from_usd(&sample("cornellbox.usda")).expect("failed to open cornellbox.usda");
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
    let scene = Scene::from_usd(&sample("rectlight.usda")).expect("failed to open rectlight.usda");

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
    let scene = Scene::from_usd(&sample("veach_mis.usda")).expect("failed to open veach_mis.usda");

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

/// `crust:pixelFilter` / `crust:pixelFilterRadius` round-trip into
/// [`crust_core::PixelFilter`]; absent attrs keep the default (triangle,
/// radius 1.0). Fails if parsing silently falls back.
#[test]
fn pixel_filter_settings_round_trip() {
    // No scene authors the attr — the default must hold.
    let scene = Scene::from_usd(&sample("cornellbox.usda")).expect("failed to open cornellbox");
    assert_eq!(
        scene.settings.pixel_filter(),
        crust_core::PixelFilter::Triangle { radius: 1.0 }
    );

    let dir = std::env::temp_dir().join("crust_pixel_filter_probe");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("pixel_filter.usda");
    std::fs::write(
        &path,
        r#"#usda 1.0
(defaultPrim = "W")
def Xform "W" { def Sphere "s" { double radius = 0.5 } }
def Scope "Render" {
    def RenderSettings "settings" {
        int2 resolution = (64, 64)
        token crust:pixelFilter = "mitchell"
        float crust:pixelFilterRadius = 1.25
    }
}
"#,
    )
    .expect("write probe stage");
    let scene = Scene::from_usd(&path).expect("stage with a pixel filter must load");
    assert_eq!(
        scene.settings.pixel_filter(),
        crust_core::PixelFilter::Mitchell { radius: 1.25 }
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
    assert!(
        (fog.sigma_s - crust_core::Vec3A::splat(0.15))
            .abs()
            .max_element()
            < 1e-6
    );
    // The homogeneous fast path must yield exact Beer-Lambert through the
    // 4-unit room: e^{-(0.15+0.01)·4} in the red channel.
    let mut s = openqmc::pcg::Rng::new(1);
    let volumes = crust_core::Volumes::new(scene.volumes);
    let ray = crust_core::Ray::new(
        crust_core::Vec3A::new(0.0, 2.0, 10.0),
        -crust_core::Vec3A::Z,
    );
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
            // Since openusd 0.7 the terminal resolves against an explicit
            // render-context list (the empty string is the universal context)
            // and yields every source driving it rather than one shader.
            let terminal = mat
                .compute_surface_source(&[""])
                .unwrap()
                .unwrap_or_else(|| panic!("Material {} has no surface shader", p));
            let shader = terminal
                .sources()
                .iter()
                .find_map(TerminalSource::shader)
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
        if let Ok(Some(bind)) = MaterialBindingAPI::get(&stage, p.clone())
            && let Ok(Some(_mat_path)) = bind.direct_binding("")
        {
            bound += 1;
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
    let on_axis =
        crust_core::Ray::new(crust_core::Vec3A::new(1.6, 0.4, 5.0), -crust_core::Vec3A::Z);
    let hit = scene
        .world
        .intersect(&on_axis, 0.001, f32::INFINITY)
        .expect("ray through the strand must hit");
    assert!(
        (hit.rec.t - 5.4).abs() < 0.1,
        "strand hit at t={} (expected ~5.4)",
        hit.rec.t
    );
    let wide = crust_core::Ray::new(crust_core::Vec3A::new(2.6, 0.4, 5.0), -crust_core::Vec3A::Z);
    // The wide ray flies past every strand and over the floor edge... but the
    // floor extends to z=-8, so aim slightly upward to clear it entirely.
    let wide_up = crust_core::Ray::new(
        crust_core::Vec3A::new(2.6, 0.4, 5.0),
        (crust_core::Vec3A::new(2.6, 3.0, -8.0) - crust_core::Vec3A::new(2.6, 0.4, 5.0))
            .normalize(),
    );
    assert!(scene.world.intersect(&wide, 0.001, 4.0).is_none());
    assert!(
        scene
            .world
            .intersect(&wide_up, 0.001, f32::INFINITY)
            .is_none()
    );
}

/// Light source geometry is camera-invisible by default (the industry
/// convention); `crust:light:cameraVisible` opts a light back in, and an
/// authored `crust:rayMask` wins outright. Shadow and indirect rays see
/// every light either way — occlusion and the bounce side of MIS depend
/// on that.
/// A curve hit's shading tangent is the strand's own direction: the cubic
/// "Tuft" strand's Bézier derivative at the point hit, not an arbitrary
/// frame around the normal.
#[test]
fn curve_hits_shade_along_the_strand() {
    let scene = Scene::from_usd(&sample("curves.usda")).expect("failed to open curves.usda");
    // The first Tuft strand's control points, and its midpoint (u = 0.5).
    let cp = [
        Vec3A::new(-0.6, 0.0, 0.0),
        Vec3A::new(-0.7, 0.6, 0.1),
        Vec3A::new(-0.4, 1.2, -0.1),
        Vec3A::new(-0.8, 1.7, 0.0),
    ];
    let mid = (cp[0] + cp[3]) * 0.125 + (cp[1] + cp[2]) * 0.375;
    let ray = Ray::new(mid + Vec3A::Z * 5.0, -Vec3A::Z);
    let hit = scene
        .world
        .intersect(&ray, 0.001, f32::INFINITY)
        .expect("the ray aims at the strand's midpoint");
    assert!((hit.rec.t - 5.0).abs() < 0.1, "t = {}", hit.rec.t);
    let derivative = ((cp[1] - cp[0]) + (cp[2] - cp[1]) * 2.0 + (cp[3] - cp[2])) * 0.75;
    let along = hit.rec.tangent.dot(derivative.normalize());
    assert!(
        along > 0.995,
        "tangent {:?}, strand {derivative:?}",
        hit.rec.tangent
    );
    assert!((hit.rec.tangent.length() - 1.0).abs() < 1e-5);
}

#[test]
fn light_geometry_camera_visibility() {
    let scene = Scene::from_usd(&sample("light_visibility.usda"))
        .expect("failed to open light_visibility.usda");

    // Floor + three light spheres, all three in the light list.
    assert_eq!(
        scene.world.count(),
        4,
        "expected 4 geometries, got {}",
        scene.world.count()
    );
    assert_eq!(
        scene.lights.count(),
        3,
        "expected 3 lights, got {}",
        scene.lights.count()
    );

    // Each light: radius 0.5 sphere at (x, 2, 0); a ray straight down from
    // (x, 5, 0) meets its top at t = 2.5 and the floor at t = 5.
    let down =
        |x: f32| crust_core::Ray::new(crust_core::Vec3A::new(x, 5.0, 0.0), -crust_core::Vec3A::Y);
    let hit_t = |x: f32, mask: crust_core::RayMask| {
        scene
            .world
            .intersect(&down(x).with_mask(mask), 0.001, f32::INFINITY)
            .expect("the floor backstops every ray")
            .rec
            .t
    };

    // Camera rays: the unauthored light is skipped (floor at t=5); the
    // bool-visible and rayMask-override lights are hit (t=2.5).
    assert!((hit_t(-3.0, crust_core::MASK_CAMERA) - 5.0).abs() < 1e-3);
    assert!((hit_t(0.0, crust_core::MASK_CAMERA) - 2.5).abs() < 1e-3);
    assert!((hit_t(3.0, crust_core::MASK_CAMERA) - 2.5).abs() < 1e-3);

    // Indirect rays see all three light surfaces; shadow rays only the two
    // the camera sees — the hidden one is a transparent emitter.
    for x in [-3.0, 0.0, 3.0] {
        assert!((hit_t(x, crust_core::MASK_INDIRECT) - 2.5).abs() < 1e-3);
    }
    assert!((hit_t(-3.0, crust_core::MASK_SHADOW) - 5.0).abs() < 1e-3);
    assert!((hit_t(0.0, crust_core::MASK_SHADOW) - 2.5).abs() < 1e-3);
    assert!((hit_t(3.0, crust_core::MASK_SHADOW) - 2.5).abs() < 1e-3);
    let geom = |x: f32| {
        scene
            .world
            .intersect(
                &down(x).with_mask(crust_core::MASK_INDIRECT),
                0.001,
                f32::INFINITY,
            )
            .expect("a light")
            .geom_id
    };
    assert!(scene.world.is_transparent_emitter(geom(-3.0)));
    assert!(!scene.world.is_transparent_emitter(geom(0.0)));
    assert!(!scene.world.is_transparent_emitter(geom(3.0)));
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
        crust_core::Ray::new(crust_core::Vec3A::new(x, 0.6, 6.0), -crust_core::Vec3A::Z)
            .with_time(time)
    };
    assert!(scene.world.intersect(&at(-1.5, 0.0), 0.001, 5.9).is_some());
    assert!(scene.world.intersect(&at(-1.5, 1.0), 0.001, 5.9).is_none());
    assert!(scene.world.intersect(&at(-0.5, 1.0), 0.001, 5.9).is_some());

    // The shadow card (crust:rayMask = 6) is invisible to camera rays but
    // opaque to shadow rays.
    let down = crust_core::Ray::new(crust_core::Vec3A::new(0.0, 5.0, 0.0), -crust_core::Vec3A::Y);
    let cam_hit = scene
        .world
        .intersect(
            &down.clone().with_mask(crust_core::MASK_CAMERA),
            0.001,
            f32::INFINITY,
        )
        .expect("camera ray passes the card and hits the floor");
    assert!(
        (cam_hit.rec.t - 5.0).abs() < 1e-3,
        "camera ray should reach the floor at t=5, got {}",
        cam_hit.rec.t
    );
    let shadow_hit = scene
        .world
        .intersect(
            &down.clone().with_mask(crust_core::MASK_SHADOW),
            0.001,
            f32::INFINITY,
        )
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
    let scene =
        Scene::from_usd(&sample("instancing.usda")).expect("failed to open instancing.usda");

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
    let scene =
        Scene::from_usd(&sample("instancing.usda")).expect("failed to open instancing.usda");

    // `/World/_Tower` is authored at the origin. The towers are placed at
    // x = -4.2, -2.2 and -0.4, so nothing should occupy x = 0, and a ray
    // down the tower's height there must reach only the floor.
    let ray = crust_core::Ray::new(crust_core::Vec3A::new(0.0, 1.0, 6.0), -crust_core::Vec3A::Z);
    assert!(
        scene.world.intersect(&ray, 0.001, 20.0).is_none(),
        "the class prototype was drawn at the origin"
    );
}

/// Instances must land where their transforms put them, and carry the
/// material bound inside the prototype.
#[test]
fn instances_are_placed_and_shaded_per_prototype_part() {
    let scene =
        Scene::from_usd(&sample("instancing.usda")).expect("failed to open instancing.usda");

    // TowerA sits at x = -4.2 with its block spanning y in [0, 2] and its
    // emerald cap [2, 2.5] (prototype y in [-0.5, 1.5] / [1.5, 2.0], the
    // instance raised by 0.5).
    let shoot = |x: f32, y: f32| {
        crust_core::Ray::new(crust_core::Vec3A::new(x, y, 6.0), -crust_core::Vec3A::Z)
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
        scene
            .world
            .intersect(&shoot(-3.4, 1.0), 0.001, 20.0)
            .is_none(),
        "unexpected geometry between TowerA and TowerB"
    );

    // TowerC is scaled to 1.4 in y, so its cap reaches higher than
    // TowerA's: prototype y = 2.0 maps to 0.5 + 1.4 * 2.0 = 3.3.
    assert!(
        scene
            .world
            .intersect(&shoot(-0.4, 3.0), 0.001, 20.0)
            .is_some(),
        "TowerC's non-uniform scale was not applied"
    );
    assert!(
        scene
            .world
            .intersect(&shoot(-4.2, 3.0), 0.001, 20.0)
            .is_none(),
        "unscaled TowerA should not reach y = 3"
    );
}

/// `invisibleIds` prunes instances, and every visible one is placed.
#[test]
fn point_instancer_honours_invisible_ids() {
    let scene =
        Scene::from_usd(&sample("instancing.usda")).expect("failed to open instancing.usda");

    // Six positions are authored; id 13 — the fourth, at x = 5.4 — is
    // hidden. Shoot straight down each gem's column from y = 4: a gem
    // stops the ray above the floor, its absence lets it run to the floor
    // at exactly t = 4. (The threshold is deliberately just shy of the
    // floor rather than near the gems' tops: per-instance rotations tilt
    // them, so the height at which a column meets a gem varies.)
    let hit_above = |x: f32, z: f32| {
        let ray = crust_core::Ray::new(crust_core::Vec3A::new(x, 4.0, z), -crust_core::Vec3A::Y);
        scene
            .world
            .intersect(&ray, 0.001, 10.0)
            .is_some_and(|h| h.rec.t < 3.9) // anything above the floor at y = 0
    };
    assert!(hit_above(1.6, 0.0), "gem id 10 missing");
    assert!(hit_above(2.9, -1.1), "gem id 11 missing");
    assert!(hit_above(4.2, 0.4), "gem id 12 missing");
    assert!(
        !hit_above(5.4, -0.6),
        "gem id 13 is in invisibleIds but was drawn"
    );
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

    // The Branch prototype expands to one part of 3 slots (leaf, bud husk,
    // bud tip — one per distinct geometry, since each binds a material), so
    // the outer instancer's 5 placements take 15 geom_ids. Plus 2 planters x
    // 2 parts, the floor and the light.
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

    // 5 branches (one instance each, all three slots inside) + 2 planters x
    // 2 parts + floor (2 tris) + light (2 tris). Flattening the inner
    // instancer would put each of the 5x4 = 20 nested placements at the top
    // level instead; splitting it per part would put 5x3.
    assert!(
        scene.world.primitive_count() <= 13,
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
        let ray = crust_core::Ray::new(crust_core::Vec3A::new(x, y, 10.0), -crust_core::Vec3A::Z);
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
        let ray = crust_core::Ray::new(crust_core::Vec3A::new(x, y, 10.0), -crust_core::Vec3A::Z);
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

/// Parts in [`many_part_stage`]'s tree: above the importer's top-level
/// grouping threshold, as a Moana bay cedar (16 181 parts) is.
const TREE_PARTS: usize = 70;

/// A stage whose `_Tree` class is [`TREE_PARTS`] small quads in a row along
/// +X (part `i` spans `x ∈ [0.5 i, 0.5 i + 0.4]`, `y ∈ [0, 0.4]`), binding
/// `Even` and `Odd` alternately, plus whatever `placements` authors.
fn many_part_stage(name: &str, placements: &str) -> PathBuf {
    let mut parts = String::new();
    for i in 0..TREE_PARTS {
        let (x0, x1) = (0.5 * i as f32, 0.5 * i as f32 + 0.4);
        let look = if i % 2 == 0 { "Even" } else { "Odd" };
        parts.push_str(&format!(
            r#"        def Mesh "part{i}" (prepend apiSchemas = ["MaterialBindingAPI"]) {{
            uniform token subdivisionScheme = "none"
            rel material:binding = </W/Looks/{look}>
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [({x0}, 0, 0), ({x1}, 0, 0), ({x1}, 0.4, 0), ({x0}, 0.4, 0)]
        }}
"#
        ));
    }
    let look = |name: &str, c: &str| {
        format!(
            r#"        def Material "{name}" {{
            token outputs:surface.connect = </W/Looks/{name}/S.outputs:surface>
            def Shader "S" {{
                uniform token info:id = "crust:openpbr"
                color3f inputs:baseColor = ({c})
                token outputs:surface
            }}
        }}
"#
        )
    };
    let dir = std::env::temp_dir().join("crust_many_part_prototypes");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!(
            "#usda 1.0\n(defaultPrim = \"W\")\ndef Xform \"W\" {{\n    class Xform \"_Tree\" {{\n{parts}    }}\n{placements}\n    def Scope \"Looks\" {{\n{}{}    }}\n}}\n",
            look("Even", "0.8, 0.1, 0.1"),
            look("Odd", "0.1, 0.1, 0.8"),
        ),
    )
    .expect("write stage");
    path
}

/// Every part of every tree at `origins` is hit, as a geometry of its own
/// whose material is the one it binds: parts alternate `Even` / `Odd`, so
/// the material a hit resolves to must alternate with them.
fn assert_every_part_shades_as_bound(scene: &Scene, origins: &[(f32, f32)]) {
    let addr = |m: &dyn crust_core::Material| m as *const dyn crust_core::Material as *const ();
    let mut ids = std::collections::HashSet::new();
    let mut looks = [None, None];
    for &(ox, oy) in origins {
        for i in 0..TREE_PARTS {
            let (x, y) = (ox + 0.5 * i as f32 + 0.2, oy + 0.2);
            let ray = Ray::new(Vec3A::new(x, y, 10.0), -Vec3A::Z);
            let hit = scene
                .world
                .intersect(&ray, 0.001, 40.0)
                .unwrap_or_else(|| panic!("tree at ({ox}, {oy}): part {i} is missing"));
            assert!(
                ids.insert(hit.geom_id),
                "tree at ({ox}, {oy}): part {i} reported geom_id {} twice",
                hit.geom_id
            );
            let look = looks[i % 2].get_or_insert(addr(hit.mat));
            assert_eq!(
                *look,
                addr(hit.mat),
                "tree at ({ox}, {oy}): part {i} resolved to the wrong material"
            );
        }
    }
    assert_ne!(looks[0], looks[1], "both parities resolved to one material");
}

/// A prototype of many parts, natively instanced, is placed as *one*
/// top-level instance per placement — its parts get a BVH of their own
/// inside it — while every part still resolves to its own material.
///
/// One instance per part used to put that many boxes into the root BVH per
/// placement; a Moana bay cedar is 16 181.
#[test]
fn many_part_native_prototype_is_one_instance_per_placement() {
    let path = many_part_stage(
        "native.usda",
        r#"    def Xform "A" (instanceable = true; references = </W/_Tree>) {}
    def Xform "B" (instanceable = true; references = </W/_Tree>) {
        double3 xformOp:translate = (0, 3, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#,
    );
    let scene = Scene::from_usd(&path).expect("load");
    assert_eq!(scene.world.primitive_breakdown().instances, 2);
    assert_eq!(
        scene.world.count(),
        2 * TREE_PARTS,
        "one geom_id per part per placement"
    );
    assert_every_part_shades_as_bound(&scene, &[(0.0, 0.0), (0.0, 3.0)]);
}

/// A scatter of many-part prototypes inside a prototype — the Moana
/// island's isDunesB shape — nests as scatter → tree → part. The outer
/// placement is one top-level instance, and each part keeps its material
/// in every tree of the scatter.
///
/// It used to be one top-level instance per *part*, each holding that part
/// in every tree, so each spanned the whole scatter: 70 identical boxes
/// here, 64 724 over the island's dunes (`docs/moana_profile.md`).
#[test]
fn nested_scatter_of_many_part_prototypes_groups_per_tree() {
    let path = many_part_stage(
        "nested.usda",
        // Prototypes live under the instancer that places them, as on the
        // island: the streaming importer composes one top-level subtree at a
        // time, and a sibling prototype would be outside the mask.
        r#"    def PointInstancer "Groves" {
        rel prototypes = [</W/Groves/Protos/Grove>]
        int[] protoIndices = [0, 0]
        point3f[] positions = [(0, 0, 0), (40, 0, 0)]
        def Scope "Protos" {
            def PointInstancer "Grove" {
                rel prototypes = [</W/Groves/Protos/Grove/Protos/Tree>]
                int[] protoIndices = [0, 0, 0]
                point3f[] positions = [(0, 0, 0), (0, 3, 0), (0, 6, 0)]
                def Scope "Protos" {
                    def Xform "Tree" (references = </W/_Tree>) {}
                }
            }
        }
    }"#,
    );
    let scene = Scene::from_usd(&path).expect("load");
    assert_eq!(
        scene.world.primitive_breakdown().instances,
        2,
        "one top-level instance per grove, not one per part"
    );
    assert_eq!(scene.world.count(), 2 * TREE_PARTS);
    // Within one grove the three trees share their part's slot, so check
    // one tree per grove for distinct ids, and every tree for materials.
    assert_every_part_shades_as_bound(&scene, &[(0.0, 0.0), (40.0, 3.0)]);
    let addr = |m: &dyn crust_core::Material| m as *const dyn crust_core::Material as *const ();
    for oy in [0.0f32, 3.0, 6.0] {
        let even = |i: usize| {
            let ray = Ray::new(Vec3A::new(0.5 * i as f32 + 0.2, oy + 0.2, 10.0), -Vec3A::Z);
            scene.world.intersect(&ray, 0.001, 40.0).expect("part")
        };
        assert_eq!(addr(even(0).mat), addr(even(2).mat));
        assert_ne!(addr(even(0).mat), addr(even(1).mat));
    }
}

/// A prototype that a nested scatter places only at zero scale (the "hide
/// this instance" idiom) draws nothing, so it must take no slots: every slot
/// is reserved again at each outer placement, and ids no hit can reach
/// would multiply with the scatter.
#[test]
fn nested_scatter_reserves_no_slots_for_hidden_prototypes() {
    let path = many_part_stage(
        "hidden.usda",
        r#"    def PointInstancer "Groves" {
        rel prototypes = [</W/Groves/Protos/Grove>]
        int[] protoIndices = [0, 0]
        point3f[] positions = [(0, 0, 0), (40, 0, 0)]
        def Scope "Protos" {
            def PointInstancer "Grove" {
                rel prototypes = [
                    </W/Groves/Protos/Grove/Protos/Tree>,
                    </W/Groves/Protos/Grove/Protos/Pebble>
                ]
                int[] protoIndices = [0, 1]
                point3f[] positions = [(0, 0, 0), (0, 3, 0)]
                float3[] scales = [(0, 0, 0), (1, 1, 1)]
                def Scope "Protos" {
                    def Xform "Tree" (references = </W/_Tree>) {}
                    def Sphere "Pebble" { double radius = 0.2 }
                }
            }
        }
    }"#,
    );
    let scene = Scene::from_usd(&path).expect("load");
    assert_eq!(
        scene.world.count(),
        2,
        "one slot (the pebble) per grove; the hidden tree's {TREE_PARTS} must not be reserved"
    );
    let ray = Ray::new(Vec3A::new(0.0, 3.0, 10.0), -Vec3A::Z);
    assert!(
        scene.world.intersect(&ray, 0.001, 40.0).is_some(),
        "the pebble is drawn"
    );
    let ray = Ray::new(Vec3A::new(0.2, 0.2, 10.0), -Vec3A::Z);
    assert!(
        scene.world.intersect(&ray, 0.001, 40.0).is_none(),
        "the tree is hidden"
    );
}

/// Writes `body` as a `.usda` under the temp dir and loads it.
fn load_inline(name: &str, body: &str) -> Scene {
    let dir = std::env::temp_dir().join("crust_usd_scene_inline");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, format!("#usda 1.0\n{body}")).expect("write stage");
    let scene = Scene::from_usd(&path).expect("inline stage must load");
    let _ = std::fs::remove_file(&path);
    scene
}

/// The centre of a scene's bounds, which for a stage of one small prim is
/// where that prim was placed.
fn bounds_centre(scene: &Scene) -> Vec3A {
    let b = scene.world.bounds().expect("a bounded world");
    (b.minimum + b.maximum) * 0.5
}

fn assert_near(got: Vec3A, want: Vec3A, what: &str) {
    assert!(
        (got - want).abs().max_element() < 1e-3,
        "{what}: expected {want}, got {got}"
    );
}

/// A single-axis op composes on prim types other than `Xform`: it used to
/// read as identity off the six types the local composer could fall back
/// for, leaving the prim at its parent's origin.
#[test]
fn single_axis_ops_place_non_xform_prims() {
    let curves = load_inline(
        "translatex_curves",
        r#"def Xform "W" {
    double3 xformOp:translate = (0, 0, 5)
    uniform token[] xformOpOrder = ["xformOp:translate"]
    def BasisCurves "C" {
        uniform token type = "linear"
        int[] curveVertexCounts = [2]
        point3f[] points = [(0, -1, 0), (0, 1, 0)]
        float[] widths = [0.1] (interpolation = "constant")
        double xformOp:translateX = 2
        uniform token[] xformOpOrder = ["xformOp:translateX"]
    }
}
"#,
    );
    assert_near(
        bounds_centre(&curves),
        Vec3A::new(2.0, 0.0, 5.0),
        "BasisCurves",
    );

    let light = load_inline(
        "translatex_disk",
        r#"def Xform "W" {
    double3 xformOp:translate = (0, 0, 5)
    uniform token[] xformOpOrder = ["xformOp:translate"]
    def DiskLight "D" {
        float inputs:radius = 0.5
        double xformOp:translateX = 2
        uniform token[] xformOpOrder = ["xformOp:translateX"]
    }
}
"#,
    );
    assert_eq!(light.world.primitive_breakdown().disks, 1);
    assert_near(
        bounds_centre(&light),
        Vec3A::new(2.0, 0.0, 5.0),
        "DiskLight",
    );
}

/// A leading `!resetXformStack!` drops the inherited transform on a light,
/// not only on the six types the old dispatch listed.
#[test]
fn a_leading_reset_drops_the_parent_on_a_light() {
    let scene = load_inline(
        "reset_disk",
        r#"def Xform "W" {
    double3 xformOp:translate = (10, 0, 0)
    uniform token[] xformOpOrder = ["xformOp:translate"]
    def DiskLight "D" {
        float inputs:radius = 0.5
        double3 xformOp:translate = (0, 3, 0)
        uniform token[] xformOpOrder = ["!resetXformStack!", "xformOp:translate"]
    }
}
"#,
    );
    assert_near(
        bounds_centre(&scene),
        Vec3A::new(0.0, 3.0, 0.0),
        "reset DiskLight",
    );
}

/// The pivot pair with `!invert!`, placing a triangle whose corners are the
/// unit axes: the corners land where C++ USD's matrix for the same stack
/// (row-vector `0 2 0 0 / -2 0 0 0 / 0 0 2 0 / 5 2 0 1`) puts them.
#[test]
fn a_pivot_stack_places_geometry_as_cpp_usd_does() {
    let scene = load_inline(
        "pivot_stack",
        r#"def Mesh "M" {
    int[] faceVertexCounts = [3]
    int[] faceVertexIndices = [0, 1, 2]
    point3f[] points = [(1, 0, 0), (0, 1, 0), (0, 0, 1)]
    double3 xformOp:translate = (2, 3, 0)
    double3 xformOp:translate:pivot = (1, 1, 0)
    float3 xformOp:rotateXYZ = (0, 0, 90)
    float3 xformOp:scale = (2, 2, 2)
    uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:translate:pivot", "xformOp:rotateXYZ", "xformOp:scale", "!invert!xformOp:translate:pivot"]
}
"#,
    );
    // (1,0,0) → (5,4,0), (0,1,0) → (3,2,0), (0,0,1) → (5,2,2).
    let b = scene.world.bounds().expect("a bounded world");
    assert_near(b.minimum, Vec3A::new(3.0, 2.0, 0.0), "minimum");
    assert_near(b.maximum, Vec3A::new(5.0, 4.0, 2.0), "maximum");
}

/// A stage whose prototype `_Outer` holds a sphere at its origin and the
/// native instances `nested` of `_Inner` (a sphere at its origin), placed by
/// `placements` under one `Set` — one top-level subtree, so one stage of the
/// streaming import, within which a prototype is shared.
fn nested_native_stage(name: &str, nested: &str, placements: &str) -> Scene {
    load_inline(
        name,
        &format!(
            r#"(defaultPrim = "W")
def Xform "W" {{
    class Xform "_Inner" {{ def Sphere "s" {{ double radius = 0.5 }} }}
    class Xform "_Outer" {{
        def Sphere "outer" {{ double radius = 0.4 }}
{nested}
    }}
    def Xform "Set" {{
{placements}
    }}
}}
"#
        ),
    )
}

/// One nested instance of `_Inner`, translated by `(x, 0, 0)`.
fn nested_at(name: &str, x: f32, extra: &str) -> String {
    format!(
        r#"        def Xform "{name}" (instanceable = true; references = </W/_Inner>) {{
            double3 xformOp:translate = ({x}, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
            {extra}
        }}"#
    )
}

/// One placement of `_Outer`, translated by `(0, y, 0)`.
fn outer_at(name: &str, y: f32) -> String {
    format!(
        r#"    def Xform "{name}" (instanceable = true; references = </W/_Outer>) {{
        double3 xformOp:translate = (0, {y}, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}"#
    )
}

fn hits(scene: &Scene, x: f32, y: f32) -> bool {
    let ray = Ray::new(Vec3A::new(x, y, 10.0), -Vec3A::Z);
    scene.world.intersect(&ray, 0.001, 40.0).is_some()
}

/// A native instance inside another instance's prototype is imported: its
/// prototype's parts are spliced into the outer prototype's under the
/// composed transforms, built once and shared by every outer placement.
///
/// openusd 0.5 aborted on this stage (a `debug_assert!` in
/// `pcp/instancing.rs`), so the importer used to skip the nested instance;
/// run this in a debug build too, which is where it aborted.
#[test]
fn nested_native_instance_is_imported() {
    // Instance inside a prototype: both spheres render.
    let one = nested_native_stage("nested_one", &nested_at("i", 3.0, ""), &outer_at("A", 0.0));
    assert_eq!(one.world.count(), 2, "the outer and the nested sphere");
    assert!(hits(&one, 0.0, 0.0), "the outer prototype's own sphere");
    assert!(
        hits(&one, 3.0, 0.0),
        "the nested instance's sphere, 3 along X"
    );
    assert!(!hits(&one, -3.0, 0.0));

    // Two outer placements, each carrying two nested instances: every
    // placement carries every sphere, and the inner prototype is built once.
    let nested = [nested_at("i", 3.0, ""), nested_at("j", -3.0, "")].join("\n");
    let placements = [outer_at("A", 0.0), outer_at("B", 10.0)].join("\n");
    let two = nested_native_stage("nested_two", &nested, &placements);
    for y in [0.0, 10.0] {
        for x in [0.0, 3.0, -3.0] {
            assert!(hits(&two, x, y), "a sphere at ({x}, {y})");
        }
    }
    assert_eq!(
        two.world.unique_primitive_breakdown().spheres,
        2,
        "the outer sphere and one shared inner sphere are resident, not one per placement"
    );

    // An invisible nested instance contributes nothing, in any placement.
    let hidden = nested_native_stage(
        "nested_hidden",
        &nested_at("i", 3.0, r#"token visibility = "invisible""#),
        &placements,
    );
    assert_eq!(
        hidden.world.count(),
        2,
        "the outer sphere, once per placement"
    );
    for y in [0.0, 10.0] {
        assert!(hits(&hidden, 0.0, y));
        assert!(
            !hits(&hidden, 3.0, y),
            "the hidden nested sphere at y = {y}"
        );
    }
}

/// A host that decodes nothing real, so the importer's asset plumbing can
/// be tested without an image dependency in `crust-core`: it records what
/// was asked for and hands back a synthetic two-texel map.
struct FakeAssets {
    requested: std::sync::Mutex<Vec<PathBuf>>,
    /// UV textures asked for, with the colour space the importer decided. The
    /// space is recorded because getting it wrong is silent: a normal map read
    /// through the sRGB curve is still a plausible-looking normal map.
    textures: std::sync::Mutex<Vec<(PathBuf, crust_core::ColorSpace)>>,
    /// The colour space each environment was asked for, in request order.
    environments: std::sync::Mutex<Vec<crust_core::ColorSpace>>,
}

impl Default for FakeAssets {
    fn default() -> Self {
        FakeAssets {
            requested: std::sync::Mutex::new(Vec::new()),
            textures: std::sync::Mutex::new(Vec::new()),
            environments: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl crust_core::AssetLoader for FakeAssets {
    fn load_environment(
        &self,
        path: &std::path::Path,
        space: crust_core::ColorSpace,
    ) -> Option<crust_core::EnvironmentMap> {
        self.requested.lock().unwrap().push(path.to_path_buf());
        self.environments.lock().unwrap().push(space);
        crust_core::EnvironmentMap::new(
            2,
            1,
            vec![
                crust_core::Vec3A::new(9.0, 0.0, 0.0),
                crust_core::Vec3A::new(0.0, 9.0, 0.0),
            ],
        )
    }

    fn load_texture(
        &self,
        path: &std::path::Path,
        space: crust_core::ColorSpace,
    ) -> Option<std::sync::Arc<dyn crust_core::Texture2D>> {
        self.textures
            .lock()
            .unwrap()
            .push((path.to_path_buf(), space));
        // Declined on purpose: this host records requests, it does not decode.
        // The material still builds, on its constant inputs.
        None
    }
}

/// Lights at infinity carry no scene geometry, so they must reach the light
/// list without adding hittables.
#[test]
fn loads_domelight_usda() {
    let scene = Scene::from_usd(&sample("domelight.usda")).expect("failed to open domelight.usda");

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

/// `samples/light_linking.usda`: Rim and Fill author light links, Key a
/// shadow link, so two lights carry illuminated-class sets and one is
/// sampled by NEE alone.
#[test]
fn loads_light_linking_usda() {
    let scene =
        Scene::from_usd(&sample("light_linking.usda")).expect("failed to open light_linking.usda");
    assert_eq!(scene.lights.count(), 3);
    let links = scene.lights.links().expect("the sample authors links");
    assert_eq!(links.illuminates.iter().filter(|s| s.is_some()).count(), 2);
    assert_eq!(links.nee_only.iter().filter(|&&n| n).count(), 1);
    // The hero, the floor and the rest are three receiver classes.
    let class = |z: f32, y: f32| {
        let ray = Ray::new(Vec3A::new(0.0, y, z), -Vec3A::Z).with_mask(crust_core::MASK_CAMERA);
        scene
            .world
            .light_class(scene.world.intersect(&ray, 1e-3, 1e4).expect("hit").geom_id)
    };
    let (hero, floor) = (class(5.0, 0.9), {
        let ray =
            Ray::new(Vec3A::new(-5.0, 5.0, 5.0), -Vec3A::Y).with_mask(crust_core::MASK_CAMERA);
        scene.world.light_class(
            scene
                .world
                .intersect(&ray, 1e-3, 1e4)
                .expect("floor")
                .geom_id,
        )
    });
    assert_ne!(hero, floor);
    let lit_by = |class: u16| {
        (0..3)
            .filter(|&i| scene.lights.illuminates(i, class))
            .count()
    };
    assert_eq!(lit_by(hero), 3, "Key, Fill and Rim all light the hero");
    assert_eq!(lit_by(floor), 1, "Key alone lights the floor");
}

/// `samples/dome_backdrop.usda`: the lighting dome is the one light, the
/// backdrop dome (linked to nothing) is what the camera sees in front of it.
#[test]
fn loads_dome_backdrop_usda() {
    let scene =
        Scene::from_usd(&sample("dome_backdrop.usda")).expect("failed to open dome_backdrop.usda");
    assert_eq!(scene.lights.count(), 1, "only the Hdri illuminates");
    assert_eq!(scene.lights.backdrops().len(), 1);
    assert!(scene.lights.escapes_to_backdrop(crust_core::MASK_CAMERA));
    assert!(!scene.lights.escapes_to_backdrop(crust_core::MASK_INDIRECT));
    assert_eq!(scene.world.count(), 3);
}

/// `inputs:texture:file` is resolved against the USD layer's directory and
/// handed to the host — `crust-core` never opens the file itself.
#[test]
fn dome_texture_is_resolved_and_requested_from_the_host() {
    let assets = FakeAssets::default();
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
    let scene = Scene::from_usd(&sample("domelight.usda")).expect("failed to open domelight.usda");

    let mut dome_like = 0;
    let mut cone_like = 0;
    for light in scene.lights.lights() {
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
        assert!(
            s.distance.is_infinite(),
            "a light at infinity cannot be occluded"
        );
        let (_, pdf) = light
            .escaped(crust_core::Vec3A::ZERO, s.direction)
            .expect("sample_li produced a direction escaped() does not cover");
        let pdf = pdf.expect("NEE sampled this direction").get();
        assert!(
            (pdf - s.pdf.get()).abs() <= 1e-3 * s.pdf.get().max(pdf),
            "MIS sides disagree: sample_li {} vs escaped {}",
            s.pdf.get(),
            pdf
        );
    }
    assert_eq!(dome_like, 1, "expected exactly one all-direction dome");
    assert_eq!(
        cone_like, 1,
        "expected exactly one cone-shaped distant light"
    );
}

/// A host whose Ptex decode takes a measurable, known-minimum amount of time.
struct SlowPtexAssets {
    delay: std::time::Duration,
    loaded: std::sync::Mutex<Vec<PathBuf>>,
}

/// A texture that answers every lookup with one colour — enough to be handed
/// back as `Some`, which is what makes the importer time the load.
struct ConstTexture;

impl crust_core::PtexTexture for ConstTexture {
    fn eval(&self, _face_id: u32, _u: f32, _v: f32, _width: f32) -> crust_core::Vec3A {
        crust_core::Vec3A::splat(0.25)
    }
    fn num_faces(&self) -> usize {
        1
    }
}

impl crust_core::AssetLoader for SlowPtexAssets {
    fn load_environment(
        &self,
        _path: &std::path::Path,
        _space: crust_core::ColorSpace,
    ) -> Option<crust_core::EnvironmentMap> {
        None
    }

    fn load_ptex(
        &self,
        path: &std::path::Path,
        _space: crust_core::ColorSpace,
    ) -> Option<std::sync::Arc<dyn crust_core::PtexTexture>> {
        std::thread::sleep(self.delay);
        self.loaded.lock().unwrap().push(path.to_path_buf());
        Some(std::sync::Arc::new(ConstTexture))
    }
}

/// Loop refinement builds no face table, so a Loop mesh whose material reads
/// a per-face (Ptex) texture keeps its cage — face ids that index the
/// authored triangles — rather than refining into triangles whose ordinals
/// would be read as cage face ids. The same mesh without Ptex still refines.
#[test]
fn a_loop_mesh_with_ptex_keeps_its_cage_face_ids() {
    let dir = std::env::temp_dir().join(format!("crust_loop_ptex_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let stage = |binding: &str| {
        format!(
            r#"#usda 1.0
( defaultPrim = "World" )

def Xform "World"
{{
    def Mesh "Tris" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token subdivisionScheme = "loop"
        {binding}
        int[] faceVertexCounts = [3, 3]
        int[] faceVertexIndices = [0, 1, 2, 0, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
    }}

    def Scope "Looks"
    {{
        def Material "Rock"
        {{
            asset inputs:surfaceMap = @./nonexistent.ptx@
            token outputs:ri:surface.connect = </World/Looks/Rock/Bxdf.outputs:bxdf_out>

            def Shader "Bxdf"
            {{
                uniform token info:id = "PxrDisneyBsdf"
                token outputs:bxdf_out
            }}
        }}
    }}
}}
"#
        )
    };
    let assets = SlowPtexAssets {
        delay: std::time::Duration::ZERO,
        loaded: std::sync::Mutex::new(Vec::new()),
    };
    let options = crust_core::UsdImportOptions {
        subdivision_level: Some(1),
        ..crust_core::UsdImportOptions::default()
    };
    let load = |name: &str, binding: &str| {
        let path = dir.join(name);
        std::fs::write(&path, stage(binding)).expect("write stage");
        Scene::from_usd_with_options(&path, &assets, &options).expect("stage must load")
    };

    let ptex = load(
        "loop_ptex.usda",
        "rel material:binding = </World/Looks/Rock>",
    );
    assert_eq!(
        ptex.world.primitive_breakdown().triangles,
        2,
        "a Ptex Loop mesh keeps its cage"
    );
    let hit = ptex
        .world
        .intersect(
            &crust_core::Ray::new(crust_core::Vec3A::new(0.8, 0.2, 5.0), -crust_core::Vec3A::Z),
            1e-3,
            100.0,
        )
        .expect("the cage is hit");
    assert_eq!(
        hit.rec.face.expect("a Ptex face hit").id,
        0,
        "the hit resolves to the authored triangle"
    );

    let plain = load("loop_plain.usda", "");
    assert_eq!(
        plain.world.primitive_breakdown().triangles,
        2 * 4,
        "without Ptex, Loop refines"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Time the host spends decoding assets is reported as "Load assets" and taken
/// back out of "Traverse prims" — for Ptex exactly as for environment maps.
///
/// This is worth pinning because getting it wrong is invisible: a second,
/// unfolded accumulator (there used to be a `ptex_time` beside `asset_time`)
/// silently bills every texture load to traversal instead, and on a Ptex-heavy
/// stage the host's decode can dominate the import.
#[test]
fn ptex_load_time_is_billed_to_the_asset_phase() {
    let dir = std::env::temp_dir().join(format!("crust_ptex_phase_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let stage_path = dir.join("ptex_phase.usda");

    // A mesh bound to a PxrDisneyBsdf material carrying `inputs:surfaceMap`.
    // The .ptx need not exist: resolving the asset path is the importer's job,
    // and deciding whether it can be opened is the host's.
    std::fs::write(
        &stage_path,
        r#"#usda 1.0
( defaultPrim = "World" )

def Xform "World"
{
    def Mesh "Quad" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        uniform token subdivisionScheme = "none"
        rel material:binding = </World/Looks/Rock>
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
    }

    def Scope "Looks"
    {
        def Material "Rock"
        {
            asset inputs:surfaceMap = @./nonexistent.ptx@
            float inputs:roughness = 0.5
            token outputs:ri:surface.connect = </World/Looks/Rock/Bxdf.outputs:bxdf_out>

            def Shader "Bxdf"
            {
                uniform token info:id = "PxrDisneyBsdf"
                token outputs:bxdf_out
            }
        }
    }
}
"#,
    )
    .expect("write stage");

    let delay = std::time::Duration::from_millis(120);
    let assets = SlowPtexAssets {
        delay,
        loaded: std::sync::Mutex::new(Vec::new()),
    };
    let scene =
        Scene::from_usd_with_assets(&stage_path, &assets).expect("failed to open the ptex stage");

    let loaded = assets.loaded.lock().unwrap().clone();
    assert_eq!(
        loaded.len(),
        1,
        "expected exactly one Ptex load, got {loaded:?}"
    );

    let phase = |name: &str| {
        scene
            .stats
            .phases
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no {name:?} phase in {:?}", scene.stats.phases))
            .duration
    };

    // The sleep is a lower bound on the decode, so it is a lower bound on the
    // asset phase and must *not* be inside the traversal figure.
    assert!(
        phase("Load assets") >= delay,
        "Ptex load time missing from the asset phase: {:?} < {delay:?}",
        phase("Load assets")
    );
    assert!(
        phase("Traverse prims") < delay,
        "asset time was billed to traversal: {:?} should exclude the {delay:?} decode",
        phase("Traverse prims")
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `samples/subdivision.usda`: six identical cube cages, differing only in
/// what they author — no scheme (USD's fallback, `catmullClark`), `none`,
/// `bilinear`, `catmullClark`, a fully edge-creased `catmullClark`, and a
/// UV-textured `catmullClark` — refined at the stage's
/// `crust:subdivisionLevel` (2). Probed with rays rather than counts — the
/// *shape* is what subdivision changes: the limit surface sags strictly
/// inside the cage and rounds its corners away, while `none`, bilinear
/// refinement and infinite creases all keep the cube.
#[test]
fn loads_subdivision_usda() {
    let scene =
        Scene::from_usd(&sample("subdivision.usda")).expect("failed to open subdivision.usda");

    // 6 cubes + floor + rect-light mesh.
    assert_eq!(
        scene.world.count(),
        8,
        "expected 8 geometries (6 cubes, floor, rect-light mesh), got {}",
        scene.world.count()
    );

    // Cube centers sit at x = -7.5 (no scheme: catmullClark), -4.5 (none), -1.5
    // (bilinear), 1.5 (catmullClark), 4.5 (creased), 7.5 (textured), all
    // spanning y in [0, 2]. Rays fire straight down from y = 8; the floor at
    // y = 0 answers t = 8 for anything the cube no longer covers.
    let down = -crust_core::Vec3A::Y;
    let cast = |x: f32, z: f32| {
        let ray = crust_core::Ray::new(crust_core::Vec3A::new(x, 8.0, z), down);
        scene
            .world
            .intersect(&ray, 0.001, f32::INFINITY)
            .unwrap_or_else(|| panic!("the floor backs every probe (x={x}, z={z})"))
    };
    let probe = |x: f32, z: f32| cast(x, z).rec.t;
    let (cage, none, bilinear, smooth, creased, textured) = (-7.5, -4.5, -1.5, 1.5, 4.5, 7.5);

    // Down the centers: a cube top at y = 2 answers t = 6; the Catmull-Clark
    // tops sag strictly below it, deeper than any float slop.
    for (what, x) in [("none", none), ("bilinear", bilinear), ("creased", creased)] {
        let t = probe(x, 0.0);
        assert!((t - 6.0).abs() < 1e-3, "{what}: cube top at t={t}");
    }
    for (what, x) in [
        ("no scheme", cage),
        ("catmullClark", smooth),
        ("textured", textured),
    ] {
        let t = probe(x, 0.0);
        assert!(
            t > 6.1,
            "{what}: the limit surface must sag below the cage (t={t})"
        );
    }

    // Near a top corner: every cube still stands at y = 2 there; the rounded
    // surfaces have pulled away entirely (the ray falls through to the
    // floor).
    let (dx, dz) = (0.95, 0.95);
    for (what, x) in [("none", none), ("bilinear", bilinear), ("creased", creased)] {
        let t = probe(x + dx, dz);
        assert!((t - 6.0).abs() < 1e-3, "{what}: corner at t={t}");
    }
    for (what, x) in [
        ("no scheme", cage),
        ("catmullClark", smooth),
        ("textured", textured),
    ] {
        let t = probe(x + dx, dz);
        assert!(
            (t - 8.0).abs() < 1e-3,
            "{what}: the rounded corner must miss (t={t})"
        );
    }

    // Subdivided geometry carries smooth shading normals: on the dome the
    // normal at an off-center point tilts away from straight up, which a
    // faceted cage top could never report; the `none` cage top reports
    // exactly +Y.
    let n_cage = cast(none + 0.5, 0.5).rec.normal;
    assert!(
        (n_cage.y - 1.0).abs() < 1e-5,
        "the cage top is flat, normal {n_cage:?}"
    );
    let n_smooth = cast(smooth + 0.5, 0.5).rec.normal;
    assert!(
        n_smooth.y < 0.999 && n_smooth.x > 1e-3 && n_smooth.z > 1e-3,
        "the dome's smooth normal must tilt outward, got {n_smooth:?}"
    );

    // The textured dome kept its chart through refinement: the top face is
    // charted onto the unit square, so its middle reads about (0.5, 0.5).
    let top = cast(textured, 0.0).rec;
    assert!(top.uv.is_some(), "the refined mesh dropped its UVs");
    assert!(
        (top.uv.unwrap().0 - 0.5).abs() < 0.05 && (top.uv.unwrap().1 - 0.5).abs() < 0.05,
        "top-face middle reads {:?}",
        top.uv
    );
}

/// `subdivisionScheme = none` is a polygon cage whatever level the stage
/// asks for.
#[test]
fn subdivision_scheme_none_keeps_the_cage() {
    let dir = std::env::temp_dir().join("crust_subdiv_none_probe");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("subdiv_none.usda");
    std::fs::write(
        &path,
        r#"#usda 1.0
(defaultPrim = "W")
def Xform "W" {
    def Mesh "Cube" {
        uniform token subdivisionScheme = "none"
        point3f[] points = [(-1, -1, 1), (1, -1, 1), (1, 1, 1), (-1, 1, 1),
                            (-1, -1, -1), (1, -1, -1), (1, 1, -1), (-1, 1, -1)]
        int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]
        int[] faceVertexIndices = [0, 1, 2, 3, 5, 4, 7, 6, 4, 0, 3, 7,
                                   1, 5, 6, 2, 3, 2, 6, 7, 4, 5, 1, 0]
    }
}
def Scope "Render" {
    def RenderSettings "settings" {
        int crust:subdivisionLevel = 3
    }
}
"#,
    )
    .expect("write probe stage");
    let scene = Scene::from_usd(&path).expect("stage must load");
    // The cage's corner must still be there: a down ray just inside (1,1)
    // hits the flat top at y = 1 exactly.
    let ray = crust_core::Ray::new(
        crust_core::Vec3A::new(0.95, 8.0, 0.95),
        -crust_core::Vec3A::Y,
    );
    let hit = scene
        .world
        .intersect(&ray, 0.001, f32::INFINITY)
        .expect("the unsubdivided cage corner still stands");
    assert!(
        (hit.rec.t - 7.0).abs() < 1e-3,
        "cage top expected at t=7, got {}",
        hit.rec.t
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// MaterialX
// ---------------------------------------------------------------------------
//
// The assets this was written for (the DPEL MaterialX Teapot and Lion) are two
// gigabytes and gitignored, so these run against `samples/materialx_basic.*`,
// which is authored the same way at 20 KiB of textures.

/// A `Material` prim whose only opinion is a reference into a `.mtlx` composes
/// to an *empty* prim — openusd ships no MaterialX file-format plugin — so
/// every schema query fails and the old importer fell back to grey. Getting a
/// material that is not the grey default is the whole feature.
#[test]
fn a_materialx_reference_resolves_to_a_real_material() {
    let scene =
        Scene::from_usd(&sample("materialx_basic.usda")).expect("failed to open materialx_basic");

    assert!(scene.world.count() >= 4, "expected the four quads");

    // The grey fallback is `OpenPBR::diffuse(0.5)`: no continuous specular and
    // a flat mid-grey. A resolved MaterialX material reports that it reads
    // texture coordinates, which the fallback never does — that is the cheapest
    // unambiguous signal, since colour alone could coincide.
    let textured = (0..scene.world.count() as u32)
        .filter(|&g| scene.world.material(g).uses_uv())
        .count();
    assert_eq!(
        textured, 4,
        "expected all four quads to carry MaterialX materials, got {textured}"
    );
}

/// A probe hit looking straight down at a flat, upward-facing patch.
fn probe_hit() -> (crust_core::HitRecord, crust_core::Ray) {
    use crust_core::{HitRecord, Ray, Vec3A};
    let rec = HitRecord {
        p: Vec3A::ZERO,
        normal: Vec3A::Z,
        t: 1.0,
        front_face: true,
        face: None,
        uv: Some((0.5, 0.5)),
        tangent: Vec3A::X,
        // Point-sample: this probe reports what the graph evaluates to at a
        // named (u, v), not what a filtered render would show there.
        uv_width: 0.0,
        face_width: 0.0,
    };
    (rec, Ray::new(Vec3A::new(0.0, 0.0, 1.0), -Vec3A::Z))
}

/// Two specular interfaces, each its own leaf. The lacquer is a clear varnish
/// (α 0.02) over a satin dielectric (α 0.4) over a red diffuse; the closure
/// tree keeps all three, each at the roughness the `.mtlx` authors — a GGX
/// alpha, used as authored — rather than pooling the two dielectrics onto one
/// set of parameters. Checked in numbers rather than pixels, because a wrong
/// tree still renders as a plausible glossy surface.
#[test]
fn a_materialx_lacquer_keeps_each_interface_as_its_own_leaf() {
    use crust_core::closure::{Lobe, mx::FresnelModel};
    use crust_core::materialx;

    let decline = |_: &str, _: Option<&str>| -> Option<crust_core::TextureRef> { None };
    let loaded = materialx::load(
        &sample("materialx_basic.mtlx"),
        Some("mtlx_lacquer"),
        &crust_core::mtlx::Host::new(&decline),
    )
    .expect("mtlx_lacquer compiles");
    assert!(loaded.unsupported.is_empty(), "{:?}", loaded.unsupported);
    let (rec, r) = probe_hit();
    let p = loaded.material.probe(&r, &rec);
    let leaves = p.closure.leaves();
    assert_eq!(leaves.len(), 3, "varnish, satin and diffuse");

    let alpha_ior = |l: &crust_core::closure::Prepared| match l.lobe {
        Lobe::Specular {
            ax,
            fresnel:
                crust_core::closure::mx::Fresnel {
                    model: FresnelModel::Dielectric { ior },
                    ..
                },
            ..
        } => (ax, ior),
        _ => panic!("{} is not a dielectric", l.category),
    };
    let near = |a: f32, b: f32| (a - b).abs() < 1e-5;
    let (a_clear, ior_clear) = alpha_ior(&leaves[0]);
    let (a_satin, ior_satin) = alpha_ior(&leaves[1]);
    assert!(near(a_clear, 0.02), "varnish alpha {a_clear}");
    assert!(near(a_satin, 0.4), "satin alpha {a_satin}");
    assert!(near(ior_clear, 1.5) && near(ior_satin, 1.5));
    assert_eq!(leaves[2].category, "oren_nayar_diffuse_bsdf");
    let red = leaves[2].describe();
    assert!(red.contains("0.5500 0.0800 0.0600"), "red base lost: {red}");
}

/// MaterialX's `layer` is single-scattering: the base sees exactly the
/// throughput of what lies above it, `1 − E(ωo)` per interface, and nothing
/// else — no OpenPBR-style multiple-scattering coat darkening. So the
/// lacquer's diffuse weight is the product of the two dielectrics' filters,
/// and the satin's is the varnish's alone.
#[test]
fn a_materialx_layer_attenuates_its_base_by_the_tops_throughput_only() {
    use crust_core::closure::dielectric_refl_filter;
    use crust_core::materialx;

    let decline = |_: &str, _: Option<&str>| -> Option<crust_core::TextureRef> { None };
    let loaded = materialx::load(
        &sample("materialx_basic.mtlx"),
        Some("mtlx_lacquer"),
        &crust_core::mtlx::Host::new(&decline),
    )
    .expect("mtlx_lacquer compiles");
    let (rec, r) = probe_hit();
    let p = loaded.material.probe(&r, &rec);
    let leaves = p.closure.leaves();
    let t_clear = dielectric_refl_filter(1.0, 0.02f32.sqrt(), 1.5);
    let t_satin = dielectric_refl_filter(1.0, 0.4f32.sqrt(), 1.5);
    let near = |a: f32, b: f32| (a - b).abs() < 1e-5;
    assert!(
        near(leaves[0].weight.x, 1.0),
        "varnish {}",
        leaves[0].weight
    );
    assert!(
        near(leaves[1].weight.x, t_clear),
        "satin {} vs {t_clear}",
        leaves[1].weight
    );
    assert!(
        near(leaves[2].weight.x, t_clear * t_satin),
        "diffuse {} vs {}",
        leaves[2].weight,
        t_clear * t_satin
    );
}

/// The texture files a `.mtlx` names must reach the host, resolved against the
/// **document's own** directory — MaterialX anchors asset paths on itself, not
/// on the USD layer that referenced it — and with the `<UDIM>` token intact,
/// since expanding it is the host's job.
#[test]
fn materialx_textures_reach_the_host_with_their_udim_token() {
    let assets = FakeAssets::default();
    let scene = Scene::from_usd_with_assets(&sample("materialx_basic.usda"), &assets)
        .expect("failed to open materialx_basic");
    let _ = scene;

    let requested = assets.textures.lock().unwrap();
    let names: Vec<String> = requested
        .iter()
        .map(|(p, _)| p.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names.len(),
        3,
        "expected three distinct textures (memoized), got {names:?}"
    );
    assert!(
        names.iter().any(|n| n.contains("mtlx_base.<UDIM>.png")),
        "the UDIM token was expanded or lost before the host saw it: {names:?}"
    );
    for (path, _) in requested.iter() {
        // A `<UDIM>` path names a *set*, so it never exists as a file — its
        // directory does, which is what anchoring had to get right. A plain
        // path is checked outright.
        let anchored = if path.to_string_lossy().contains("<UDIM>") {
            path.parent().is_some_and(|d| d.is_dir())
        } else {
            path.exists()
        };
        assert!(
            anchored,
            "asset path not anchored against the .mtlx's directory: {}",
            path.display()
        );
    }

    // Colour space is MaterialX's own per-input declaration, and getting it
    // wrong is silent: only `mtlx_base` is marked `srgb_texture`, and reading
    // the normal or mask maps through the sRGB curve would bend every value
    // toward zero.
    let srgb: Vec<&str> = requested
        .iter()
        .filter(|(_, s)| *s == crust_core::ColorSpace::SRGB)
        .map(|(p, _)| p.file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(
        srgb,
        vec!["mtlx_base.<UDIM>.png"],
        "wrong inputs decoded as sRGB"
    );
}

/// `primvars:st` has to survive triangulation, and faceVarying is the case
/// that matters: a vertex on a UV seam has one position and two coordinates,
/// so nothing per-point can carry it.
#[test]
fn face_varying_st_reaches_the_shading_point() {
    use crust_core::{MASK_CAMERA, Ray, Vec3A};

    let scene =
        Scene::from_usd(&sample("materialx_basic.usda")).expect("failed to open materialx_basic");

    // The two ceramic quads sit at z = 0 spanning x in [-2.1, -0.1] and
    // [0.1, 2.1], charted 0..1 and 1..2 respectively. Fire at the middle of
    // each and read back the interpolated coordinate.
    let probe = |x: f32, y: f32| -> Option<(f32, f32)> {
        let r = Ray::new(Vec3A::new(x, y, 5.0), Vec3A::new(0.0, 0.0, -1.0)).with_mask(MASK_CAMERA);
        let hit = scene.world.intersect(&r, 0.001, f32::INFINITY)?;
        hit.rec.uv
    };

    let a = probe(-1.1, 1.0).expect("left quad carries no UV");
    let b = probe(1.1, 1.0).expect("right quad carries no UV");
    assert!((a.0 - 0.5).abs() < 0.01, "left u = {}, expected ~0.5", a.0);
    assert!((a.1 - 0.5).abs() < 0.01, "left v = {}, expected ~0.5", a.1);
    // The second tile must report u ~ 1.5 — *not* wrapped into [0, 1], or the
    // UDIM addressing collapses every tile onto the first.
    assert!((b.0 - 1.5).abs() < 0.01, "right u = {}, expected ~1.5", b.0);
}

/// Two prims with the same points, topology and material but *different*
/// `primvars:st` are not the same mesh.
///
/// The `MeshSlot` a distinct mesh interns to owns the `UvMap` every placement
/// of it shades through, built from whichever prim got there first. Leaving
/// the chart out of `MeshKey` therefore made the second prim read the first
/// one's coordinates — a wrong UDIM tile, or a wrong region of one atlas,
/// rendering as a plausible texture rather than as an error. Charting one
/// panel into two tiles is the ordinary idiom (`samples/materialx_basic.usda`
/// is built on it); it only collides once the two prims agree on geometry,
/// which is exactly what an instanced kit of parts does.
#[test]
fn identical_geometry_with_different_uvs_keeps_its_own_chart() {
    use crust_core::{MASK_CAMERA, Ray, Vec3A};

    let dir = std::env::temp_dir().join("crust_uv_dedup_probe");
    std::fs::create_dir_all(&dir).expect("temp dir");
    // A material from a `.mtlx`, because `uses_uv()` is what makes the
    // importer read a chart at all — a `UsdPreviewSurface` would build no
    // table and the collision would be unobservable.
    std::fs::write(
        dir.join("flat.mtlx"),
        r#"<?xml version="1.0"?>
<materialx version="1.38">
  <oren_nayar_diffuse_bsdf name="flat_diffuse" type="BSDF">
    <input name="color" type="color3" value="0.8, 0.8, 0.8" />
  </oren_nayar_diffuse_bsdf>
  <surface name="flat_surface" type="surfaceshader">
    <input name="bsdf" type="BSDF" nodename="flat_diffuse" />
  </surface>
  <surfacematerial name="mtlx_flat" type="material">
    <input name="surfaceshader" type="surfaceshader" nodename="flat_surface" />
  </surfacematerial>
</materialx>
"#,
    )
    .expect("write probe mtlx");

    // `A` and `B` are byte-identical geometry bound to one material, set
    // apart only by their transform and their chart: A spans UDIM tile 1001,
    // B tile 1002.
    let stage = |st_b: &str| {
        format!(
            r#"#usda 1.0
(defaultPrim = "W")
def Xform "W" {{
    def Scope "Looks" {{
        def Material "Mtl" (
            prepend references = @flat.mtlx@</MaterialX/Materials/mtlx_flat>
        ) {{
        }}
    }}
    def Mesh "A" (prepend apiSchemas = ["MaterialBindingAPI"]) {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, 0, 0), (1, 0, 0), (1, 2, 0), (-1, 2, 0)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (
            interpolation = "faceVarying"
        )
        rel material:binding = </W/Looks/Mtl>
        double3 xformOp:translate = (-3, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def Mesh "B" (prepend apiSchemas = ["MaterialBindingAPI"]) {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, 0, 0), (1, 0, 0), (1, 2, 0), (-1, 2, 0)]
        texCoord2f[] primvars:st = [{st_b}] (
            interpolation = "faceVarying"
        )
        rel material:binding = </W/Looks/Mtl>
        double3 xformOp:translate = (3, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
}}
"#
        )
    };

    let probe = |scene: &crust_core::Scene, x: f32| -> (f32, f32) {
        let r =
            Ray::new(Vec3A::new(x, 1.0, 5.0), Vec3A::new(0.0, 0.0, -1.0)).with_mask(MASK_CAMERA);
        let hit = scene
            .world
            .intersect(&r, 0.001, f32::INFINITY)
            .unwrap_or_else(|| panic!("no hit at x = {x}"));
        hit.rec
            .uv
            .unwrap_or_else(|| panic!("no chart reached the shading point at x = {x}"))
    };

    // 1. Different charts: each prim must report the coordinates it authored.
    let path = dir.join("uv_dedup.usda");
    std::fs::write(&path, stage("(1, 0), (2, 0), (2, 1), (1, 1)")).expect("write probe stage");
    let scene = Scene::from_usd(&path).expect("stage must load");
    let (au, av) = probe(&scene, -3.0);
    let (bu, bv) = probe(&scene, 3.0);
    assert!(
        (au - 0.5).abs() < 0.01 && (av - 0.5).abs() < 0.01,
        "A: ({au}, {av})"
    );
    // The load-bearing one: B shading at u ~ 0.5 means it was handed A's
    // chart, so its texture reads tile 1001 instead of 1002.
    assert!(
        (bu - 1.5).abs() < 0.01 && (bv - 0.5).abs() < 0.01,
        "B shaded through A's chart: ({bu}, {bv}), expected ~(1.5, 0.5)"
    );

    // 2. Identical charts still dedupe — the fix must not disable sharing,
    // only make it correct. Two prims sharing a slot are placed as instances
    // of one prototype, so the root BVH holds instances and the *unique*
    // triangle count stays that of a single quad.
    let same = dir.join("uv_dedup_same.usda");
    std::fs::write(&same, stage("(0, 0), (1, 0), (1, 1), (0, 1)")).expect("write probe stage");
    let shared = Scene::from_usd(&same).expect("stage must load");
    assert_eq!(
        shared.stats.scene.unique.triangles, 2,
        "prims agreeing on geometry, material and chart must still share one mesh: {:?}",
        shared.stats.scene
    );
    assert_eq!(shared.stats.scene.top_level.instances, 2);
    // And the differing-chart stage is the opposite: two distinct meshes.
    assert_eq!(
        scene.stats.scene.unique.triangles, 4,
        "differing charts must intern as two meshes: {:?}",
        scene.stats.scene
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A baked (single-placement) mesh must carry a tangent frame, or every normal
/// map silently degrades to the geometric normal.
#[test]
fn baked_geometry_carries_a_tangent_frame() {
    use crust_core::{MASK_CAMERA, Ray, Vec3A};

    let scene =
        Scene::from_usd(&sample("materialx_basic.usda")).expect("failed to open materialx_basic");
    let r = Ray::new(Vec3A::new(-1.1, 1.0, 5.0), Vec3A::new(0.0, 0.0, -1.0)).with_mask(MASK_CAMERA);
    let hit = scene
        .world
        .intersect(&r, 0.001, f32::INFINITY)
        .expect("no hit on the left quad");

    let t = hit.rec.tangent;
    assert!(t.length() > 0.5, "no tangent recorded: {t:?}");
    // The quad's chart runs with +u along +x, so the tangent must too, and it
    // must be perpendicular to the normal or the frame is not a frame.
    assert!(t.x > 0.9, "tangent does not follow +u: {t:?}");
    assert!(
        t.dot(hit.rec.normal).abs() < 1e-3,
        "tangent is not orthogonal to the normal: {t:?} . {:?}",
        hit.rec.normal
    );
}

/// Geometry bound to a material that reads no texture coordinates must not pay
/// for the table — the reason [`Material::uses_uv`] exists.
#[test]
fn untextured_geometry_carries_no_uv_table() {
    use crust_core::{MASK_CAMERA, Ray, Vec3A};

    let scene = Scene::from_usd(&sample("cornellbox.usda")).expect("failed to open cornellbox");
    let r = Ray::new(Vec3A::new(0.0, 1.0, 3.0), Vec3A::new(0.0, 0.0, -1.0)).with_mask(MASK_CAMERA);
    let hit = scene
        .world
        .intersect(&r, 0.001, f32::INFINITY)
        .expect("no hit in the cornell box");
    assert!(
        !hit.rec.uv.is_some(),
        "an untextured mesh built a UV table it will never read"
    );
}

/// The emissive MaterialX sample imports, and its emitters actually emit.
///
/// Both halves matter. The document authors an `edf`, which the importer used
/// to drop on the floor — not as an unsupported node, but invisibly, because
/// the `surface` arm followed only `bsdf` and never reached the EDF's category
/// to report it. And the textured panel is the only place in the renderer
/// where an HDR texture's range has a consumer: `base_color` above 1 creates
/// energy and is clamped, correctly, while radiance above 1 is just a bright
/// light.
#[test]
fn the_emissive_materialx_sample_imports_and_emits() {
    let scene =
        Scene::from_usd(&sample("materialx_emissive.usda")).expect("failed to open the sample");

    // Two emissive panels plus the backdrop and the floor.
    assert_eq!(scene.world.count(), 4, "four quads");

    // Nothing in this document should be unsupported: `uniform_edf`,
    // `multiply` over an EDF and an `image` are all read.
    let hit = |p: Vec3A, dir: Vec3A| {
        let ray = Ray::new(p, dir.normalize());
        scene.world.intersect(&ray, 0.001, f32::INFINITY)
    };

    // Straight at the constant emitter's panel, from in front.
    let h = hit(Vec3A::new(-1.2, 0.1, 4.0), -Vec3A::Z).expect("hits the lamp panel");
    let e = h.mat.emitted_at(
        &Ray::new(Vec3A::new(-1.2, 0.1, 4.0), -Vec3A::Z),
        &h.rec,
        1.0,
    );
    assert!(
        e.max_element() > 1.0,
        "multiply(uniform_edf, 12) must exceed 1.0, got {e:?}"
    );
    // 12 x (1.0, 0.72, 0.4).
    assert!((e - Vec3A::new(12.0, 8.64, 4.8)).length() < 1e-3, "{e:?}");

    // And the textured panel emits too. Its value depends on the residency
    // path — preloaded clips to 1.0, streamed carries 16.0 — so this asserts
    // only that emission arrives at all; `mtlx_shade` is what prints the
    // number, and `world_material.rs` pins the unclipped range at the seam.
    let h = hit(Vec3A::new(1.2, 0.1, 4.0), -Vec3A::Z).expect("hits the HDR panel");
    let e = h
        .mat
        .emitted_at(&Ray::new(Vec3A::new(1.2, 0.1, 4.0), -Vec3A::Z), &h.rec, 1.0);
    assert!(e.max_element() > 0.0, "the textured panel emits nothing");
}

/// The backdrop is a plain `crust:openpbr` diffuse, so any radiance reaching
/// it came off an emitter through the integrator rather than out of a material
/// — which is what makes the sample a transport test and not a display one.
#[test]
fn the_emissive_sample_backdrop_does_not_emit() {
    let scene =
        Scene::from_usd(&sample("materialx_emissive.usda")).expect("failed to open the sample");
    let ray = Ray::new(
        Vec3A::new(4.0, 3.0, 4.0),
        Vec3A::new(-0.4, -0.3, -1.0).normalize(),
    );
    let h = scene
        .world
        .intersect(&ray, 0.001, f32::INFINITY)
        .expect("hits the backdrop");
    assert_eq!(h.mat.emitted_at(&ray, &h.rec, 1.0), Vec3A::ZERO);
}

/// `--frame`: `samples/animation.usda` time-samples a sphere's translate
/// (x = -2 at frame 1 to x = +2 at frame 10) and a card's points (y = 2 to
/// y = 1), with the sphere's *default* translate parked off-screen at
/// y = 20. Each attribute must resolve at the requested frame, linearly
/// interpolated between samples, and held past either end.
#[test]
fn animated_stage_is_evaluated_at_the_requested_frame() {
    let load = |frame: Option<f64>| {
        Scene::from_usd_at_frame(&sample("animation.usda"), &crust_core::NoAssets, frame)
            .expect("failed to open animation.usda")
    };
    // A ray straight down -z at height 0.6 through x; `None` if it misses
    // everything in front of the floor's far edge.
    let sphere_at = |scene: &Scene, x: f32| {
        let ray = Ray::new(Vec3A::new(x, 0.6, 6.0), -Vec3A::Z);
        scene.world.intersect(&ray, 0.001, 20.0).map(|h| h.rec.t)
    };
    // A ray straight down -y through the card's centre: the card's height
    // is 5 - t (the floor is at t = 5).
    let card_height = |scene: &Scene| {
        let ray = Ray::new(Vec3A::new(2.0, 5.0, -1.0), -Vec3A::Y);
        let t = scene
            .world
            .intersect(&ray, 0.001, 20.0)
            .expect("card or floor")
            .rec
            .t;
        5.0 - t
    };

    let first = load(Some(1.0));
    assert!(
        sphere_at(&first, -2.0).is_some(),
        "frame 1: sphere at x = -2"
    );
    assert!(sphere_at(&first, 2.0).is_none());
    assert!((card_height(&first) - 2.0).abs() < 1e-4);

    let last = load(Some(10.0));
    assert!(
        sphere_at(&last, 2.0).is_some(),
        "frame 10: sphere at x = +2"
    );
    assert!(sphere_at(&last, -2.0).is_none());
    assert!((card_height(&last) - 1.0).abs() < 1e-4);

    // Halfway: sphere centred at x = 0 (its front at z = 0.6, so t = 5.4),
    // card halfway down.
    let mid = load(Some(5.5));
    let t = sphere_at(&mid, 0.0).expect("frame 5.5: sphere at x = 0");
    assert!((t - 5.4).abs() < 1e-3, "sphere front at t = 5.4, got {t}");
    assert!((card_height(&mid) - 1.5).abs() < 1e-4);

    // Past the end: USD holds the last sample.
    let after = load(Some(25.0));
    assert!(sphere_at(&after, 2.0).is_some());

    // No frame: every attribute reads its default — except an `xformOp`,
    // which openusd 0.7 composes at time 0.0 (its transform API has no
    // default-time arm), so the sphere holds its first sample rather than
    // its off-screen default translate. The openusd bump in
    // `retire-openusd-workarounds` (phase 2) restores the default here.
    let default = load(None);
    assert!(
        sphere_at(&default, -2.0).is_some(),
        "no frame: the sphere's translate is read at time 0, holding frame 1"
    );
    assert!(sphere_at(&default, 2.0).is_none());
}

/// The frame drives the sampler's frame seed too, so an image sequence gets
/// independent noise per frame; without a frame the stage's `crust:frame`
/// (here unauthored, so 0) stands.
#[test]
fn frame_sets_the_sampler_seed() {
    let seed = |frame: Option<f64>| {
        Scene::from_usd_at_frame(&sample("animation.usda"), &crust_core::NoAssets, frame)
            .expect("failed to open animation.usda")
            .settings
            .frame()
    };
    assert_eq!(seed(None), 0);
    assert_eq!(seed(Some(7.0)), 7);
    assert_eq!(seed(Some(7.75)), 7, "a subframe shares its frame's seed");
    assert_eq!(seed(Some(-3.0)), -3);
}

/// Evaluating at a frame must not move anything that is not animated: the
/// cornell box authors no time samples, so any frame imports the same scene.
#[test]
fn static_stage_is_unchanged_by_a_frame() {
    let a = Scene::from_usd(&sample("cornellbox.usda")).expect("cornellbox");
    let b = Scene::from_usd_at_frame(
        &sample("cornellbox.usda"),
        &crust_core::NoAssets,
        Some(42.0),
    )
    .expect("cornellbox at frame 42");
    assert_eq!(a.world.count(), b.world.count());
    for (x, y) in [(0.0, 0.5), (-0.3, 0.2), (0.25, 0.8)] {
        let ray = Ray::new(Vec3A::new(x, y, 3.0), -Vec3A::Z);
        let ta = a.world.intersect(&ray, 0.001, 100.0).map(|h| h.rec.t);
        let tb = b.world.intersect(&ray, 0.001, 100.0).map(|h| h.rec.t);
        assert_eq!(ta, tb, "ray through ({x}, {y}) sees the same surface");
    }
}

/// A non-finite frame is not a time code. `NaN` in particular compares false
/// against the stage's time range, so without an explicit check it would
/// slip past the range warning into interpolation and the sampler seed.
#[test]
fn non_finite_frames_are_rejected() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        match Scene::from_usd_at_frame(&sample("animation.usda"), &crust_core::NoAssets, Some(bad))
        {
            Err(crust_core::Error::InvalidFrame(f)) => {
                assert!(f.is_nan() == bad.is_nan() && (f.is_nan() || f == bad));
            }
            Err(e) => panic!("frame {bad}: expected InvalidFrame, got {e}"),
            Ok(_) => panic!("frame {bad} must be rejected"),
        }
    }
}

/// UsdUVTexture requests reach the host anchored, with `<UDIM>` intact, and
/// with the colour space `sourceColorSpace` asks for — where, unlike
/// MaterialX, an unauthored attribute means `auto` (decided by the host
/// against the file) rather than raw.
#[test]
fn preview_textures_reach_the_host_with_their_source_color_space() {
    use crust_core::ColorSpace;

    let assets = FakeAssets::default();
    let scene = Scene::from_usd_with_assets(&sample("usdpreview_textured.usda"), &assets)
        .expect("failed to open usdpreview_textured");

    let mut got: Vec<(String, ColorSpace)> = assets
        .textures
        .lock()
        .unwrap()
        .iter()
        .map(|(p, s)| (p.file_name().unwrap().to_string_lossy().into_owned(), *s))
        .collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        vec![
            ("mtlx_base.<UDIM>.png".to_owned(), ColorSpace::SRGB),
            ("mtlx_mask.png".to_owned(), ColorSpace::RAW),
            ("mtlx_normal.<UDIM>.png".to_owned(), ColorSpace::RAW),
            // Unauthored and `"auto"` alike.
            ("usdpreview_albedo.exr".to_owned(), ColorSpace::AUTO),
            ("usdpreview_rough.png".to_owned(), ColorSpace::AUTO),
        ]
    );
    for (path, _) in assets.textures.lock().unwrap().iter() {
        let dir = path.parent().expect("a directory");
        assert!(dir.is_dir(), "not anchored: {}", path.display());
    }

    // Every quad's surface is texture-driven, so every one reads the chart.
    let textured = (0..scene.world.count() as u32)
        .filter(|&g| scene.world.material(g).uses_uv())
        .count();
    assert_eq!(textured, 4, "expected four textured preview surfaces");
}

/// A scope's `colorSpace:name` describes its *colour values*, not how an image
/// file is encoded: a colour texture's decode comes from its own `colorSpace`
/// metadatum or `sourceColorSpace` alone. And a texture that is not a colour
/// is never moved to other primaries, whatever the file, the scope or the
/// working space says: a change of primaries would mix a normal map's
/// channels. It decodes by `sourceColorSpace`'s curve alone, as before colour
/// management.
#[test]
fn a_scopes_color_space_does_not_reach_texture_files() {
    use crust_core::ColorSpace;
    use crust_core::color::{Space, working_space};

    let dir = std::env::temp_dir().join("crust_scope_color_space");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let texture = |name: &str, file: &str, extra: &str, out: &str| {
        format!(
            r#"
        def Shader "{name}"
        {{
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @{file}@ {extra}
            {out}
        }}"#
        )
    };
    let srgb = "\n            token inputs:sourceColorSpace = \"sRGB\"";
    let rec2020 = "(colorSpace = \"lin_rec2020\")";
    let stage = format!(
        r#"#usda 1.0
(
    defaultPrim = "W"
)
def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        uniform token renderingColorSpace = "acescg"
    }}
}}
def Xform "W" (
    prepend apiSchemas = ["ColorSpaceAPI"]
)
{{
    uniform token colorSpace:name = "acescg"
    def Material "M"
    {{
        token outputs:surface.connect = </W/M/S.outputs:surface>
        token outputs:displacement.connect = </W/M/S.outputs:displacement>
        def Shader "S"
        {{
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </W/M/Albedo.outputs:rgb>
            color3f inputs:emissiveColor.connect = </W/M/Glow.outputs:rgb>
            float inputs:roughness.connect = </W/M/Rough.outputs:r>
            normal3f inputs:normal.connect = </W/M/Normal.outputs:rgb>
            float inputs:metallic.connect = </W/M/Metal.outputs:r>
            float inputs:displacement.connect = </W/M/Height.outputs:r>
            token outputs:surface
            token outputs:displacement
        }}{}{}{}{}{}{}
    }}
    def Mesh "Quad" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (interpolation = "faceVarying")
        rel material:binding = </W/M>
    }}
}}
"#,
        texture("Albedo", "albedo.png", srgb, "vector3f outputs:rgb"),
        texture("Glow", "glow.exr", rec2020, "vector3f outputs:rgb"),
        texture("Rough", "rough.png", srgb, "float outputs:r"),
        texture("Normal", "normal.png", rec2020, "vector3f outputs:rgb"),
        texture("Metal", "metal.png", "", "float outputs:r"),
        texture("Height", "height.png", rec2020, "float outputs:r"),
    );
    let path = dir.join("scope_color_space.usda");
    std::fs::write(&path, stage).expect("write stage");

    let assets = FakeAssets::default();
    Scene::from_usd_with_assets(&path, &assets).expect("stage opens");
    let mut got: Vec<(String, ColorSpace)> = assets
        .textures
        .lock()
        .unwrap()
        .iter()
        .map(|(p, s)| (p.file_name().unwrap().to_string_lossy().into_owned(), *s))
        .collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    got.dedup();
    let acescg = working_space("acescg").unwrap();
    let rec2020 = Space::named("lin_rec2020").unwrap();
    assert_eq!(
        got,
        vec![
            // Colours: the sRGB albedo decodes into ACEScg rather than being
            // read as linear ACEScg; the file's own metadatum still wins.
            (
                "albedo.png".to_owned(),
                ColorSpace::new(Space::SRGB_TEXTURE, acescg)
            ),
            ("glow.exr".to_owned(), ColorSpace::new(rec2020, acescg)),
            // Displacement: `auto` is raw, the file's metadatum ignored.
            ("height.png".to_owned(), ColorSpace::RAW),
            // Values: `sourceColorSpace` alone, on Rec.709 primaries.
            ("metal.png".to_owned(), ColorSpace::AUTO),
            ("normal.png".to_owned(), ColorSpace::AUTO),
            ("rough.png".to_owned(), ColorSpace::SRGB),
        ]
    );
}

/// A dome texture tagged `raw` is radiance used as stored — already in the
/// working space — not data: it keeps the working space, whose luminance
/// weights its importance sampling needs.
#[test]
fn a_raw_dome_texture_keeps_the_working_space() {
    use crust_core::ColorSpace;
    use crust_core::color::working_space;

    let dir = std::env::temp_dir().join("crust_raw_dome");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let stage = r#"#usda 1.0
(
    defaultPrim = "W"
)
def Scope "Render"
{
    def RenderSettings "settings"
    {
        uniform token renderingColorSpace = "acescg"
    }
}
def Xform "W"
{
    def DomeLight "Sky"
    {
        asset inputs:texture:file = @sky.exr@ (colorSpace = "raw")
    }
}
"#;
    let path = dir.join("raw_dome.usda");
    std::fs::write(&path, stage).expect("write stage");

    let assets = FakeAssets::default();
    Scene::from_usd_with_assets(&path, &assets).expect("stage opens");
    let acescg = working_space("acescg").unwrap();
    assert_eq!(
        *assets.environments.lock().unwrap(),
        vec![ColorSpace::new(acescg, acescg)]
    );
    assert_eq!(ColorSpace::new(acescg, acescg).working(), acescg);
}

/// A texture that does not load reads the UsdUVTexture's own `fallback`, and
/// with none authored the surface input's constant — so declining every
/// texture (`CRUST_TEX=0`, or a host that decodes nothing) renders the
/// surface on its constants rather than black. And a preview surface with no
/// texture connection is still the plain OpenPBR it always was: no chart, no
/// per-hit evaluation.
#[test]
fn a_declined_preview_texture_falls_back_to_its_fallback_then_the_constant() {
    use crust_core::{MASK_CAMERA, Ray, Vec3A};

    let dir = std::env::temp_dir().join("crust_preview_fallback");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let quad = |name: &str, x: f32, mat: &str| {
        format!(
            r#"
    def Mesh "{name}" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [({x0}, 0, 0), ({x1}, 0, 0), ({x1}, 1, 0), ({x0}, 1, 0)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (interpolation = "faceVarying")
        rel material:binding = </W/Looks/{mat}>
    }}"#,
            x0 = x,
            x1 = x + 1.0
        )
    };
    let material = |name: &str, constant: &str, fallback: &str| {
        format!(
            r#"
    def Material "{name}"
    {{
        token outputs:surface.connect = </W/Looks/{name}/S.outputs:surface>
        def Shader "S"
        {{
            uniform token info:id = "UsdPreviewSurface"
            {constant}
            color3f inputs:diffuseColor.connect = </W/Looks/{name}/T.outputs:rgb>
            float inputs:roughness = 1
            token outputs:surface
        }}
        def Shader "T"
        {{
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @missing.png@
            {fallback}
            vector3f outputs:rgb
        }}
    }}"#
        )
    };
    // Two top-level children, not six: at `MIN_STREAM_CHUNKS` the importer
    // streams each subtree under its own mask, and a material that is a
    // sibling chunk of the mesh binding it is not composed while that mesh
    // is traversed.
    let plain = r#"
    def Material "Plain"
    {
        token outputs:surface.connect = </W/Looks/Plain/S.outputs:surface>
        def Shader "S"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (0.5, 0.5, 0.5)
            token outputs:surface
        }
    }"#;
    let stage = format!(
        "#usda 1.0\n(\n    defaultPrim = \"W\"\n)\ndef Xform \"W\"\n{{\ndef Scope \"Looks\"\n{{{}{}{}\n}}\ndef Xform \"Geo\"\n{{{}{}{}\n}}\n}}\n",
        material(
            "WithFallback",
            "color3f inputs:diffuseColor = (0.1, 0.8, 0.1)",
            "float4 inputs:fallback = (1, 0, 1, 1)"
        ),
        material(
            "WithConstant",
            "color3f inputs:diffuseColor = (0.1, 0.8, 0.1)",
            ""
        ),
        plain,
        quad("A", -3.0, "WithFallback"),
        quad("B", -1.0, "WithConstant"),
        quad("C", 1.0, "Plain"),
    );
    let path = dir.join("fallback.usda");
    std::fs::write(&path, stage).expect("write stage");

    let assets = FakeAssets::default();
    let scene = Scene::from_usd_with_assets(&path, &assets).expect("stage opens");
    assert_eq!(
        assets.textures.lock().unwrap().len(),
        1,
        "one file, memoized"
    );

    // Diffuse reflectance toward the normal, from straight above.
    let reflectance = |x: f32| -> (Vec3A, bool) {
        let r =
            Ray::new(Vec3A::new(x, 0.5, 5.0), Vec3A::new(0.0, 0.0, -1.0)).with_mask(MASK_CAMERA);
        let hit = scene
            .world
            .intersect(&r, 0.001, f32::INFINITY)
            .expect("hits the quad");
        let (f, _) = hit
            .mat
            .eval(&r, &hit.rec, Vec3A::Z)
            .expect("a continuous lobe");
        (f, hit.mat.uses_uv())
    };
    let (a, a_uv) = reflectance(-2.5);
    assert!(a_uv);
    assert!(
        a.x > 3.0 * a.y && a.z > 3.0 * a.y,
        "authored fallback is magenta: {a}"
    );
    let (b, b_uv) = reflectance(-0.5);
    assert!(b_uv);
    assert!(
        b.y > 3.0 * b.x && b.y > 3.0 * b.z,
        "no fallback: the constant, green: {b}"
    );
    let (c, c_uv) = reflectance(1.5);
    assert!(!c_uv, "an untextured preview surface builds no chart");
    assert!((c.x - c.y).abs() < 1e-6 && (c.y - c.z).abs() < 1e-6, "{c}");
}

/// `UsdPreviewSurface` `opacity` below 1 is translucency — a dielectric
/// refracting at `ior`, as the spec's "index of refraction to be used for
/// translucent objects" says — not an alpha cutout. Both the constant and the
/// texture-driven input take that path; `opacityThreshold > 0` is the spec's
/// cutout mode, which does not refract. ALab authors every flask, beaker and
/// screen as opacity ≈ 0 with an `ior` map, all of which rendered opaque.
#[test]
fn preview_surface_opacity_refracts_unless_it_is_a_cutout() {
    use crust_core::{MASK_CAMERA, Ray, Vec3A};

    let dir = std::env::temp_dir().join("crust_preview_opacity");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let quad = |name: &str, x: f32, mat: &str| {
        format!(
            r#"
    def Mesh "{name}" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [({x0}, 0, 0), ({x1}, 0, 0), ({x1}, 1, 0), ({x0}, 1, 0)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (interpolation = "faceVarying")
        rel material:binding = </W/Looks/{mat}>
    }}"#,
            x0 = x,
            x1 = x + 1.0
        )
    };
    let material = |name: &str, body: &str, extra: &str| {
        format!(
            r#"
    def Material "{name}"
    {{
        token outputs:surface.connect = </W/Looks/{name}/S.outputs:surface>
        def Shader "S"
        {{
            uniform token info:id = "UsdPreviewSurface"
            float inputs:ior = 1.5
            float inputs:roughness = 0.5
            {body}
            token outputs:surface
        }}{extra}
    }}"#
        )
    };
    // The texture never loads (`FakeAssets` decodes nothing), so it reads its
    // authored fallback of 0 — through the textured path, not the constant.
    let textured = material(
        "Textured",
        "float inputs:opacity.connect = </W/Looks/Textured/T.outputs:r>",
        r#"
        def Shader "T"
        {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @missing_opacity.<UDIM>.exr@
            float4 inputs:fallback = (0, 0, 0, 1)
            float outputs:r
        }"#,
    );
    // A textured mask under `opacityThreshold = 0.5`, reading its fallback.
    let masked = |name: &str, fallback: f32| {
        material(
            name,
            &format!(
                "float inputs:opacity.connect = </W/Looks/{name}/T.outputs:r>\n            \
                 float inputs:opacityThreshold = 0.5"
            ),
            &format!(
                r#"
        def Shader "T"
        {{
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @missing_mask.exr@
            float4 inputs:fallback = ({fallback}, 0, 0, 1)
            float outputs:r
        }}"#
            ),
        )
    };
    let stage = format!(
        "#usda 1.0\n(\n    defaultPrim = \"W\"\n)\ndef Xform \"W\"\n{{\ndef Scope \"Looks\"\n{{{}{}{}{}{}{}\n}}\ndef Xform \"Geo\"\n{{{}{}{}{}{}{}\n}}\n}}\n",
        material("Glass", "float inputs:opacity = 0", ""),
        textured,
        material(
            "Cutout",
            "float inputs:opacity = 0\n            float inputs:opacityThreshold = 0.5",
            ""
        ),
        material("Opaque", "", ""),
        masked("MaskedOut", 0.2),
        masked("MaskedIn", 0.8),
        quad("A", -4.0, "Glass"),
        quad("B", -2.0, "Textured"),
        quad("C", 0.0, "Cutout"),
        quad("D", 2.0, "Opaque"),
        quad("E", 4.0, "MaskedOut"),
        quad("F", 6.0, "MaskedIn"),
    );
    let path = dir.join("opacity.usda");
    std::fs::write(&path, stage).expect("write stage");
    let scene = Scene::from_usd_with_assets(&path, &FakeAssets::default()).expect("stage opens");

    // The BSDF toward straight through the quad, from straight above: zero
    // unless the surface transmits.
    let through = |x: f32| -> Vec3A {
        let r =
            Ray::new(Vec3A::new(x, 0.5, 5.0), Vec3A::new(0.0, 0.0, -1.0)).with_mask(MASK_CAMERA);
        let hit = scene
            .world
            .intersect(&r, 0.001, f32::INFINITY)
            .expect("hits the quad");
        hit.mat
            .eval(&r, &hit.rec, -Vec3A::Z)
            .map_or(Vec3A::ZERO, |(f, _)| f)
    };
    let glass = through(-3.5);
    assert!(
        glass.min_element() > 0.0,
        "constant opacity 0 refracts: {glass}"
    );
    let textured = through(-1.5);
    assert!(
        textured.min_element() > 0.0,
        "textured opacity 0 refracts: {textured}"
    );
    assert_eq!(through(0.5), Vec3A::ZERO, "a cutout does not refract");
    assert_eq!(through(2.5), Vec3A::ZERO, "the default opacity is opaque");

    // Presence: a cutout is kept whole at or above the threshold and
    // discarded below it; translucency is no cutout at all.
    let presence = |x: f32| -> Option<f32> {
        let r =
            Ray::new(Vec3A::new(x, 0.5, 5.0), Vec3A::new(0.0, 0.0, -1.0)).with_mask(MASK_CAMERA);
        let hit = scene
            .world
            .intersect(&r, 0.001, f32::INFINITY)
            .expect("hits the quad");
        hit.mat.has_cutout().then(|| hit.mat.opacity(&r, &hit.rec))
    };
    assert!(scene.world.has_cutouts());
    assert_eq!(presence(-3.5), None, "translucent glass");
    assert_eq!(presence(-1.5), None, "textured translucency");
    assert_eq!(presence(0.5), Some(0.0), "constant 0 under 0.5");
    assert_eq!(presence(2.5), None, "opaque");
    assert_eq!(presence(4.5), Some(0.0), "textured 0.2 under 0.5");
    assert_eq!(presence(6.5), Some(1.0), "textured 0.8 over 0.5");
}

/// Bindings are inherited from ancestors and resolved for the `full` purpose,
/// falling back to all-purpose — `ComputeBoundMaterial` semantics. ALab binds
/// every asset through `material:binding:full` on its `GEO` scope, never on
/// the meshes, so a direct all-purpose lookup on the mesh left all of it grey.
#[test]
fn bindings_inherit_from_ancestors_and_prefer_the_full_purpose() {
    use crust_core::{MASK_CAMERA, Ray, Vec3A};

    let dir = std::env::temp_dir().join("crust_binding_purpose");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let material = |name: &str, rgb: &str| {
        format!(
            r#"
        def Material "{name}"
        {{
            token outputs:surface.connect = </W/Looks/{name}/S.outputs:surface>
            def Shader "S"
            {{
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = ({rgb})
                float inputs:roughness = 1
                token outputs:surface
            }}
        }}"#
        )
    };
    // No API and no binding on the mesh itself: everything is inherited.
    let quad = |name: &str, x: f32| {
        format!(
            r#"
            def Mesh "{name}"
            {{
                uniform token subdivisionScheme = "none"
                int[] faceVertexCounts = [4]
                int[] faceVertexIndices = [0, 1, 2, 3]
                point3f[] points = [({x0}, 0, 0), ({x1}, 0, 0), ({x1}, 1, 0), ({x0}, 1, 0)]
            }}"#,
            x0 = x,
            x1 = x + 1.0
        )
    };
    let stage = format!(
        r#"#usda 1.0
(
    defaultPrim = "W"
)
def Xform "W"
{{
    def Scope "Looks"
    {{{full}{preview}{all}
    }}
    def Xform "Geo"
    {{
        def Xform "Purposed" (
            prepend apiSchemas = ["MaterialBindingAPI"]
        )
        {{
            rel material:binding = </W/Looks/All>
            rel material:binding:full = </W/Looks/Full>
            rel material:binding:preview = </W/Looks/Preview>
            def Scope "Nested"
            {{{a}
            }}
        }}
        def Xform "AllPurpose" (
            prepend apiSchemas = ["MaterialBindingAPI"]
        )
        {{
            rel material:binding = </W/Looks/All>
            rel material:binding:preview = </W/Looks/Preview>{b}
        }}
    }}
}}
"#,
        full = material("Full", "0.1, 0.8, 0.1"),
        preview = material("Preview", "0.8, 0.1, 0.1"),
        all = material("All", "0.1, 0.1, 0.8"),
        a = quad("A", -2.0),
        b = quad("B", 1.0),
    );
    let path = dir.join("binding.usda");
    std::fs::write(&path, stage).expect("write stage");
    let scene = Scene::from_usd(&path).expect("stage opens");

    let colour = |x: f32| -> Vec3A {
        let r =
            Ray::new(Vec3A::new(x, 0.5, 5.0), Vec3A::new(0.0, 0.0, -1.0)).with_mask(MASK_CAMERA);
        let hit = scene
            .world
            .intersect(&r, 0.001, f32::INFINITY)
            .expect("hits the quad");
        hit.mat
            .eval(&r, &hit.rec, Vec3A::Z)
            .expect("a continuous lobe")
            .0
    };
    let a = colour(-1.5);
    assert!(
        a.y > 3.0 * a.x && a.y > 3.0 * a.z,
        "two levels down, `full` beats all-purpose and preview: {a}"
    );
    let b = colour(1.5);
    assert!(
        b.z > 3.0 * b.x && b.z > 3.0 * b.y,
        "no `full` binding: all-purpose, never preview: {b}"
    );
}

/// A render draws `default` and `render` purpose only: a `proxy` or `guide`
/// subtree is pruned whole, whatever its descendants author. ALab publishes
/// every asset with a `purpose = "proxy"` `GEO_PROXY` beside its `GEO`, and
/// the proxies rendered as grey duplicates of the real geometry.
#[test]
fn proxy_and_guide_purpose_subtrees_are_not_rendered() {
    let dir = std::env::temp_dir().join("crust_purpose_prune");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let quad = |name: &str| {
        format!(
            r#"
        def Mesh "{name}"
        {{
            uniform token subdivisionScheme = "none"
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
        }}"#
        )
    };
    let stage = format!(
        r#"#usda 1.0
def Xform "W"
{{
    def Xform "GEO"
    {{
        token purpose = "render"{render}
    }}
    def Xform "GEO_PROXY"
    {{
        token purpose = "proxy"
        def Xform "Inner"
        {{
            token purpose = "render"{proxy}
        }}
    }}
    def Xform "Guides"
    {{
        token purpose = "guide"{guide}
    }}
    def Xform "Plain"
    {{{plain}
    }}
}}
"#,
        render = quad("R"),
        proxy = quad("P"),
        guide = quad("G"),
        plain = quad("D"),
    );
    let path = dir.join("purpose.usda");
    std::fs::write(&path, stage).expect("write stage");
    let scene = Scene::from_usd(&path).expect("stage opens");
    assert_eq!(
        scene.world.count(),
        2,
        "only the render- and default-purpose quads, not the proxy (even with \
         `render` authored beneath it) or the guide"
    );
}

/// `visibility = "invisible"` hides a prim and its whole subtree — geometry
/// and lights alike, whatever a descendant authors, at the evaluated time,
/// and inside a prototype as well. A camera in a hidden subtree is still
/// rendered through: its visibility only hides a viewport gizmo. ALab's rig
/// parks three interior fills/bounces and a debug dome as invisible, and all
/// four lit the shot.
#[test]
fn invisible_subtrees_draw_and_light_nothing_but_keep_their_cameras() {
    use crust_core::UsdImportOptions;

    let dir = std::env::temp_dir().join("crust_visibility_prune");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let quad = |name: &str, extra: &str| {
        format!(
            r#"
        def Mesh "{name}"
        {{
            uniform token subdivisionScheme = "none"
            {extra}
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
        }}"#
        )
    };
    let stage = format!(
        r#"#usda 1.0
(
    startTimeCode = 1
    endTimeCode = 2
)
def Xform "W"
{{
    def Xform "Hidden"
    {{
        token visibility = "invisible"{hidden}
        def Xform "Inner"
        {{
            token visibility = "inherited"{inherited}
        }}
        def Camera "Cam"
        {{
            double3 xformOp:translate = (10, 0, 5)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }}
        def SphereLight "HiddenByParent" {{ float inputs:radius = 0.1 }}
    }}
    def Xform "Shown"
    {{{shown}{invisible_mesh}
    }}
    def SphereLight "Key" {{ float inputs:radius = 0.1 }}
    def SphereLight "Parked" {{
        float inputs:radius = 0.1
        token visibility = "invisible"
    }}
    def DomeLight "Debug" {{ token visibility = "invisible" }}
    def DistantLight "Blink" {{
        token visibility.timeSamples = {{ 1: "inherited", 2: "invisible" }}
    }}
    def PointInstancer "Scatter"
    {{
        rel prototypes = [</W/Scatter/Protos/P>]
        int[] protoIndices = [0, 0]
        point3f[] positions = [(0, 0, 3), (2, 0, 3)]
        def Scope "Protos"
        {{
            def Xform "P"
            {{{proto_visible}{proto_hidden}
            }}
        }}
    }}
}}
"#,
        hidden = quad("H", ""),
        inherited = quad("I", ""),
        shown = quad("D", ""),
        invisible_mesh = quad("X", r#"token visibility = "invisible""#),
        proto_visible = quad("V", ""),
        proto_hidden = quad("Q", r#"token visibility = "invisible""#),
    );
    let path = dir.join("visibility.usda");
    std::fs::write(&path, stage).expect("write stage");
    let load = |frame: Option<f64>, camera: Option<&str>| {
        Scene::from_usd_with_options(
            &path,
            &crust_core::NoAssets,
            &UsdImportOptions {
                frame,
                camera: camera.map(str::to_owned),
                ..UsdImportOptions::default()
            },
        )
        .expect("stage opens")
    };

    let scene = load(None, None);
    // D, Key's source sphere, and the prototype's visible part once per
    // instance (2); not H, I, X, the hidden prototype part, or any hidden
    // light's geometry.
    assert_eq!(scene.world.count(), 4, "only the visible geometry");
    // Key, and Blink, whose samples are not read without a frame (its
    // default is unauthored, so `inherited`).
    assert_eq!(
        scene.lights.count(),
        2,
        "invisible lights do not illuminate"
    );

    assert_eq!(load(Some(1.0), None).lights.count(), 2, "Blink shown at 1");
    assert_eq!(load(Some(2.0), None).lights.count(), 1, "Blink hidden at 2");

    let scene = load(None, Some("/W/Hidden/Cam"));
    let x = scene.camera.get_ray(0.5, 0.5, [0.5, 0.5], 0.0).origin().x;
    assert_eq!(x, 10.0, "a camera under an invisible rig still renders");
}

/// A relative texture path that openusd cannot resolve — every `<UDIM>` path,
/// since it names a set rather than a file — is anchored on the layer that
/// authored it, not on the root layer. ALab authors its textures five
/// directories below `entry.usda`, and 1 718 sets failed to load. And a
/// network whose primvar reader names `perfuv` reads that primvar: the only
/// chart ALab's published assets carry.
#[test]
fn preview_textures_anchor_on_their_layer_and_read_the_named_primvar() {
    use crust_core::{MASK_CAMERA, Ray, Vec3A};

    let dir = std::env::temp_dir().join("crust_preview_anchor");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("look/deep")).expect("temp dir");
    std::fs::create_dir_all(dir.join("look/tex")).expect("temp dir");
    std::fs::write(
        dir.join("look/deep/look.usda"),
        r#"#usda 1.0
over "W"
{
    def Scope "Looks"
    {
        def Material "M"
        {
            token outputs:surface.connect = </W/Looks/M/S.outputs:surface>
            def Shader "S"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor.connect = </W/Looks/M/T.outputs:rgb>
                token outputs:surface
            }
            def Shader "T"
            {
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @../tex/albedo.<UDIM>.png@
                float2 inputs:st.connect = </W/Looks/M/R.outputs:result>
                vector3f outputs:rgb
            }
            def Shader "R"
            {
                uniform token info:id = "UsdPrimvarReader_float2"
                string inputs:varname = "perfuv"
                float2 outputs:result
            }
        }
    }
}
"#,
    )
    .expect("write look layer");
    std::fs::write(
        dir.join("root.usda"),
        r#"#usda 1.0
(
    subLayers = [@look/deep/look.usda@]
)
def Xform "W"
{
    def Xform "Geo"
    {
        def Mesh "Q" (
            prepend apiSchemas = ["MaterialBindingAPI"]
        )
        {
            uniform token subdivisionScheme = "none"
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
            texCoord2f[] primvars:perfuv = [(1, 0), (2, 0), (2, 1), (1, 1)] (
                interpolation = "faceVarying"
            )
            rel material:binding = </W/Looks/M>
        }
    }
}
"#,
    )
    .expect("write root layer");

    let assets = FakeAssets::default();
    let scene = Scene::from_usd_with_assets(&dir.join("root.usda"), &assets).expect("stage opens");
    let requested = assets.textures.lock().unwrap().clone();
    assert_eq!(requested.len(), 1, "{requested:?}");
    // `look/deep/../tex`, not `<root dir>/../tex`: compare real directories.
    let got = &requested[0].0;
    assert_eq!(
        got.file_name().and_then(|n| n.to_str()),
        Some("albedo.<UDIM>.png")
    );
    assert_eq!(
        got.parent().and_then(|p| p.canonicalize().ok()),
        dir.join("look/tex").canonicalize().ok(),
        "anchored against the look layer's directory: {}",
        got.display()
    );

    let r = Ray::new(Vec3A::new(0.5, 0.5, 5.0), Vec3A::new(0.0, 0.0, -1.0)).with_mask(MASK_CAMERA);
    let hit = scene
        .world
        .intersect(&r, 0.001, f32::INFINITY)
        .expect("hits the quad");
    assert!(hit.rec.uv.is_some(), "the perfuv chart was not read");
    assert!(
        (hit.rec.uv.unwrap().0 - 1.5).abs() < 0.01,
        "u = {}, expected ~1.5 from primvars:perfuv",
        hit.rec.uv.unwrap().0
    );
}

/// A texture that fails with no `fallback` and no authored constant reads the
/// input's `UsdPreviewSurface` schema default, not the node set's opaque
/// black. ALab's wrench connects `roughness` to a map the dataset does not
/// ship and authors nothing else; black made it a mirror.
#[test]
fn a_missing_texture_with_nothing_authored_reads_the_schema_default() {
    use crust_core::{MASK_CAMERA, Ray, Vec3A};

    let dir = std::env::temp_dir().join("crust_preview_schema_default");
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(
        dir.join("s.usda"),
        r#"#usda 1.0
def Xform "W"
{
    def Scope "Looks"
    {
        def Material "M"
        {
            token outputs:surface.connect = </W/Looks/M/S.outputs:surface>
            def Shader "S"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor.connect = </W/Looks/M/T.outputs:rgb>
                float inputs:roughness = 1
                token outputs:surface
            }
            def Shader "T"
            {
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @not_shipped.<UDIM>.exr@
                vector3f outputs:rgb
            }
        }
    }
    def Xform "Geo"
    {
        def Mesh "Q" (
            prepend apiSchemas = ["MaterialBindingAPI"]
        )
        {
            uniform token subdivisionScheme = "none"
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
            rel material:binding = </W/Looks/M>
        }
    }
}
"#,
    )
    .expect("write stage");
    let scene = Scene::from_usd(&dir.join("s.usda")).expect("stage opens");
    let r = Ray::new(Vec3A::new(0.5, 0.5, 5.0), Vec3A::new(0.0, 0.0, -1.0)).with_mask(MASK_CAMERA);
    let hit = scene
        .world
        .intersect(&r, 0.001, f32::INFINITY)
        .expect("hits the quad");
    let (f, _) = hit
        .mat
        .eval(&r, &hit.rec, Vec3A::Z)
        .expect("a continuous lobe");
    assert!(f.x > 0.01, "not black: {f}");
    assert!(
        (f.x - f.y).abs() < 1e-6 && (f.y - f.z).abs() < 1e-6,
        "neutral grey: {f}"
    );
}

/// Which camera renders: `UsdImportOptions::camera` (the CLI's `--camera`),
/// else `RenderSettings.camera`, else the first camera met. ALab's stage
/// carries a trailer camera per shot beside the shot camera, and the first
/// one the traversal meets is a trailer camera.
#[test]
fn camera_choice_follows_the_option_then_render_settings() {
    use crust_core::{Error, UsdImportOptions};

    let dir = std::env::temp_dir().join("crust_camera_choice");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let stage = |settings_camera: Option<&str>| {
        let rel = settings_camera
            .map(|p| format!("rel camera = <{p}>"))
            .unwrap_or_default();
        format!(
            r#"#usda 1.0
def Xform "W"
{{
    def Camera "A"
    {{
        double3 xformOp:translate = (0, 0, 5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def Camera "B"
    {{
        double3 xformOp:translate = (10, 0, 5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
}}
def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        int2 resolution = (64, 36)
        {rel}
    }}
}}
"#
        )
    };
    let load = |settings_camera: Option<&str>, camera: Option<&str>| {
        let path = dir.join(format!(
            "cam_{}.usda",
            settings_camera.unwrap_or("none").replace('/', "_")
        ));
        std::fs::write(&path, stage(settings_camera)).expect("write stage");
        Scene::from_usd_with_options(
            &path,
            &crust_core::NoAssets,
            &UsdImportOptions {
                camera: camera.map(str::to_owned),
                ..UsdImportOptions::default()
            },
        )
    };
    let x = |scene: Scene| scene.camera.get_ray(0.5, 0.5, [0.5, 0.5], 0.0).origin().x;

    assert_eq!(
        x(load(None, Some("/W/B")).expect("loads")),
        10.0,
        "the option picks B"
    );
    assert_eq!(
        x(load(None, Some("/W/A")).expect("loads")),
        0.0,
        "the option picks A"
    );
    assert_eq!(
        x(load(Some("/W/B"), None).expect("loads")),
        10.0,
        "RenderSettings.camera picks B"
    );
    assert_eq!(
        x(load(Some("/W/B"), Some("/W/A")).expect("loads")),
        0.0,
        "the option wins over RenderSettings.camera"
    );
    // A dangling RenderSettings.camera warns and falls back to a real camera.
    let fallback = x(load(Some("/W/Nope"), None).expect("falls back"));
    assert!(fallback == 0.0 || fallback == 10.0, "{fallback}");

    match load(None, Some("/W/Nope")) {
        Err(Error::CameraNotFound { path, available }) => {
            assert_eq!(path, "/W/Nope");
            let mut available = available;
            available.sort();
            assert_eq!(
                available,
                ["/W/A", "/W/B"],
                "the error lists the real cameras"
            );
        }
        other => panic!("expected CameraNotFound, got {:?}", other.map(|_| ())),
    }
    for bad in ["W/A", "/", "/W/A.xformOp:translate", ""] {
        assert!(
            matches!(load(None, Some(bad)), Err(Error::InvalidCameraPath(_))),
            "{bad:?} must be refused"
        );
    }
}

/// Keeping the stage allocated (`skip_stage_teardown`) must change nothing but
/// the teardown: the same render on a single-stage import, where it applies,
/// and on a streamed one, where it must not (every chunk is still dropped).
#[test]
fn skipping_stage_teardown_leaves_the_render_unchanged() {
    use crust_core::{RenderSettings, Renderer, UsdImportOptions};
    // Five subtrees under one root prim: enough to take the streamed path.
    let dir = std::env::temp_dir().join(format!("crust_stage_teardown_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let streamed = dir.join("five.usda");
    let mut usda =
        String::from("#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\ndef Xform \"World\"\n{\n");
    for k in 0..5 {
        usda += &format!(
            "    def Sphere \"S{k}\"\n    {{\n        double radius = 0.4\n        \
             double3 xformOp:translate = ({} 0 -3)\n        \
             uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    }}\n",
            k as f64 - 2.0
        );
    }
    usda += "    def Camera \"Cam\"\n    {\n    }\n}\n";
    std::fs::write(&streamed, usda).unwrap();

    let render = |path: &std::path::Path, keep: bool| {
        let scene = Scene::from_usd_with_options(
            path,
            &crust_core::NoAssets,
            &UsdImportOptions {
                skip_stage_teardown: keep,
                ..UsdImportOptions::default()
            },
        )
        .expect("stage opens");
        let geometries = scene.world.count();
        // 16 spp with a minimum of 16: every pixel takes exactly 16 samples.
        let settings = RenderSettings::default()
            .with_resolution(24, 16)
            .with_samples_per_pixel(16)
            .with_max_depth(3)
            .with_adaptive_sampling(16, 0.0);
        let buf = Renderer::new(scene.camera, scene.world, scene.lights, settings).render();
        let pixels: Vec<[u32; 3]> = (0..16)
            .flat_map(|y| (0..24).map(move |x| (x, y)))
            .map(|(x, y)| {
                let c = buf.get_pixel(x, y);
                [c.x.to_bits(), c.y.to_bits(), c.z.to_bits()]
            })
            .collect();
        (geometries, pixels)
    };
    for path in [sample("cornellbox.usda"), streamed] {
        let (a, b) = (render(&path, false), render(&path, true));
        assert!(a.0 > 0, "{}: nothing imported", path.display());
        assert_eq!(a, b, "{}", path.display());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `Scene::list_usd` lists what the render would use of each kind, in
/// namespace order. Cameras: one under an invisible ancestor is listed (it is
/// still rendered through), one the render never meets — inactive, abstract,
/// proxy-purpose, inside a prototype or beneath a PointInstancer — is not, and
/// every listed path is one `UsdImportOptions::camera` accepts. Lights: an
/// invisible one is pruned too, as it lights nothing. Materials: everything a
/// binding can reach, bound or not. Both import modes: two top-level subtrees
/// import as one stage, and the Cornell box's many stream.
#[test]
fn list_usd_lists_what_a_render_uses() {
    use crust_core::{ListKind, UsdImportOptions};

    let dir = std::env::temp_dir().join("crust_list_usd");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("listing.usda");
    std::fs::write(
        &path,
        r#"#usda 1.0
class Xform "Proto"
{
    def Camera "InClass" {}
    def SphereLight "ClassLight" {}
    def Material "ClassLook" {}
}
def Xform "W"
{
    def Camera "Main" {}
    def RectLight "Key" {}
    def Scope "Looks"
    {
        def Material "Bound" {}
        def Material "Unbound" {}
    }
    def Xform "Hidden"
    {
        token visibility = "invisible"
        def Camera "Witness" {}
        def DistantLight "Parked" {}
        def Material "HiddenLook" {}
        def Xform "HiddenInst" (instanceable = true references = </Proto>) {}
    }
    def Xform "Off" (active = false)
    {
        def Camera "Disabled" {}
        def DomeLight "Off" {}
        def Material "OffLook" {}
    }
    def Xform "Proxy"
    {
        uniform token purpose = "proxy"
        def Camera "ProxyCam" {}
        def DiskLight "ProxyLight" {}
    }
    def Xform "Inst" (instanceable = true references = </Proto>) {}
    def PointInstancer "Scatter"
    {
        rel prototypes = [</W/Scatter/P>]
        int[] protoIndices = [0]
        point3f[] positions = [(0, 0, 0)]
        def Xform "P"
        {
            def Camera "InInstancer" {}
            def CylinderLight "InInstancer" {}
        }
    }
    def Camera "Last" {}
    def DomeLight "Sky" {}
    def SphereLight "Degenerate" { float inputs:radius = 0 }
}
"#,
    )
    .expect("write stage");

    let list = |kind| Scene::list_usd(&path, kind).expect("lists");
    let cameras = list(ListKind::Camera);
    assert_eq!(cameras, ["/W/Main", "/W/Hidden/Witness", "/W/Last"]);
    // Every listed camera is one the import renders through.
    for camera in &cameras {
        Scene::from_usd_with_options(
            &path,
            &crust_core::NoAssets,
            &UsdImportOptions {
                camera: Some(camera.clone()),
                ..UsdImportOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("{camera}: {e}"));
    }
    let lights = list(ListKind::Light);
    assert_eq!(lights, ["/W/Key", "/W/Sky", "/W/Degenerate"]);
    // What the import puts in the light list, but for the light its values
    // make it refuse: validity is evaluated at a time code, a listing at none.
    let scene = Scene::from_usd(&path).expect("loads");
    assert_eq!(scene.lights.count(), lights.len() - 1);
    assert_eq!(
        list(ListKind::Material),
        [
            "/Proto/ClassLook",
            "/W/Looks/Bound",
            "/W/Looks/Unbound",
            "/W/Hidden/HiddenLook",
        ]
    );

    let cornell = sample("cornellbox.usda");
    assert_eq!(
        Scene::list_usd(&cornell, ListKind::Camera).expect("lists"),
        ["/scene/camera1"]
    );
    assert_eq!(
        Scene::list_usd(&cornell, ListKind::Light).expect("lists"),
        ["/scene/Sky"]
    );
    assert!(matches!(
        Scene::list_usd(&dir.join("missing.usda"), ListKind::Camera),
        Err(crust_core::Error::UsdOpen { .. })
    ));
    // Only USD is read, by extension: a USD layer under another name is
    // refused before a byte of it is parsed.
    let disguised = dir.join("listing.obj");
    std::fs::copy(&path, &disguised).expect("copy");
    assert!(matches!(
        Scene::list_usd(&disguised, ListKind::Camera),
        Err(crust_core::Error::UsdOpen { .. })
    ));

    // A sole top-level prim with four children streams them as chunks, and
    // every chunk's mask keeps it: it is walked once per chunk, and listed once.
    let solo = dir.join("solo.usda");
    std::fs::write(
        &solo,
        r#"#usda 1.0
def Camera "Rig"
{
    def Xform "A" {}
    def Xform "B" { def Material "Look" {} }
    def Xform "C" { def Camera "Inner" {} }
    def Xform "D" {}
}
"#,
    )
    .expect("write stage");
    assert_eq!(
        Scene::list_usd(&solo, ListKind::Camera).expect("lists"),
        ["/Rig", "/Rig/C/Inner"]
    );
    assert_eq!(
        Scene::list_usd(&solo, ListKind::Material).expect("lists"),
        ["/Rig/B/Look"]
    );
}
