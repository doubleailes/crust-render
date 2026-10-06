//! A light the camera does not see is an emitter and nothing else: its
//! source occludes no shadow ray, and a bounce that crosses it collects its
//! emission and goes on past it. Every scenario of the lighting spec's
//! hidden-light requirements, checked in numbers.

use crust_core::rt::Geometry;
use crust_core::{
    AreaLight, Emissive, LightList, MASK_ALL, MASK_CAMERA, MASK_INDIRECT, MASK_SHADOW, OpenPBR,
    PathSampler, Ray, SamplingStrategy, Scene, SphereShape, Vec3A, Volumes, World, WorldBuilder,
    ray_color,
};
use std::path::PathBuf;
use std::sync::Arc;

fn quad(y: f32, half: f32) -> Geometry {
    Geometry::TriangleMesh {
        vertices: vec![
            [-half, y, -half],
            [half, y, -half],
            [half, y, half],
            [-half, y, half],
        ],
        indices: vec![[0, 2, 1], [0, 3, 2]],
        normals: None,
    }
}

/// How a sphere light's source is built.
#[derive(Clone, Copy, PartialEq)]
enum Source {
    /// Hidden from the camera: a transparent emitter (the default).
    Hidden,
    /// Hidden from the camera but solid, as before transparent emitters (or
    /// as `crust:rayMask = 6` keeps it).
    Solid,
    /// Seen by every ray: a lamp bulb.
    Visible,
}

struct Lamp {
    center: Vec3A,
    radius: f32,
    radiance: f32,
    source: Source,
}

fn lamp(center: Vec3A, radius: f32, radiance: f32, source: Source) -> Lamp {
    Lamp {
        center,
        radius,
        radiance,
        source,
    }
}

/// `floor` (a material over the plane y = 0), an optional diffuse ceiling at
/// y = 4, and `lamps`.
fn scene(floor: OpenPBR, ceiling: bool, lamps: &[&Lamp]) -> (World, LightList) {
    let mut world = WorldBuilder::new();
    let mut lights = LightList::new();
    world.attach(quad(0.0, 50.0), Arc::new(floor));
    if ceiling {
        world.attach(
            quad(4.0, 50.0),
            Arc::new(OpenPBR::diffuse(Vec3A::splat(0.7))),
        );
    }
    for l in lamps {
        // One-sided, as an imported light's source is: a bounce through the
        // sphere meets its inside wall too, which emits nothing.
        let emitter = Arc::new(Emissive::light(Vec3A::splat(l.radiance), None));
        let mask = match l.source {
            Source::Hidden => MASK_INDIRECT,
            Source::Solid => MASK_SHADOW | MASK_INDIRECT,
            Source::Visible => MASK_ALL,
        };
        let id = world.attach_masked(
            Geometry::Sphere {
                center: l.center,
                radius: l.radius,
            },
            emitter.clone(),
            mask,
        );
        world.set_transparent_emitter(id, l.source == Source::Hidden);
        lights.add(AreaLight::new(
            SphereShape {
                center: l.center,
                radius: l.radius,
            },
            emitter,
            id,
        ));
    }
    (world.commit(), lights)
}

/// The mean of `n` camera paths from `from` to the surface point `to`.
fn radiance(
    (world, lights): &(World, LightList),
    from: Vec3A,
    to: Vec3A,
    depth: i32,
    s: SamplingStrategy,
    n: i32,
) -> Vec3A {
    let ray = Ray::new(from, to - from).with_mask(MASK_CAMERA);
    let mut sum = [0.0f64; 3];
    for i in 0..n {
        let c = ray_color(
            &ray,
            world,
            lights,
            &Volumes::default(),
            depth,
            s,
            PathSampler::new(3, 7, 0, i),
        );
        for (acc, x) in sum.iter_mut().zip(c.to_array()) {
            *acc += x as f64;
        }
    }
    Vec3A::from_array(sum.map(|x| (x / n as f64) as f32))
}

fn rel(a: Vec3A, b: Vec3A) -> f32 {
    ((a - b).abs() / b.abs().max(Vec3A::splat(1e-6))).max_element()
}

// ---------------------------------------------------------------------------
// The bounce side: a crossing collects and continues
// ---------------------------------------------------------------------------

/// A mirror sends one deterministic bounce up through two hidden sphere
/// lights in a row. The path collects both emissions, whole (nothing
/// competes after a delta bounce), at the depth limit (the last segment is
/// traced on its own) and below it — where a solid near light used to hide
/// the far one.
#[test]
fn a_bounce_collects_every_hidden_light_it_crosses() {
    let mirror = OpenPBR::metal(Vec3A::ONE, 0.0);
    let near = |s| lamp(Vec3A::new(0.0, 2.0, -2.0), 0.3, 5.0, s);
    let far = |s| lamp(Vec3A::new(0.0, 4.0, -4.0), 0.3, 3.0, s);
    let (from, to) = (Vec3A::new(0.0, 1.0, 1.0), Vec3A::ZERO);
    let s = SamplingStrategy::PowerMis;
    for depth in [1, 3] {
        let one =
            |lamps: &[&Lamp]| radiance(&scene(mirror.clone(), false, lamps), from, to, depth, s, 1);
        let both = one(&[&near(Source::Hidden), &far(Source::Hidden)]);
        let n = one(&[&near(Source::Hidden)]);
        let f = one(&[&far(Source::Hidden)]);
        assert!(n.min_element() > 0.0 && f.min_element() > 0.0, "{n} {f}");
        assert!(
            rel(both, n + f) < 1e-4,
            "depth {depth}: {both} vs {n} + {f}"
        );
        // A solid near source ends the bounce: the far light is lost.
        let solid = one(&[&near(Source::Solid), &far(Source::Hidden)]);
        assert!(rel(solid, n) < 1e-4, "depth {depth}: {solid} vs {n}");
    }
}

/// A small hidden light far away, crossed by a deterministic bounce: a
/// mirror floor under one hidden sphere light at a distance-to-radius ratio
/// of 40, 160 and 200, with the eye placed so the reflected ray passes
/// through a random point of the sphere's disc. The bounce collects the
/// light's emission exactly once — the same as the bounce that stops at the
/// light made solid — for every ray.
///
/// The analytic sphere's textbook discriminant lost 1e-4 of distance from 8
/// units away, more than the pass-through restart steps past, so the
/// restarted segment met the entry again and a third of these rays
/// collected the emission twice: a diffuse plane came out 4 %, 34 % and 64 %
/// brighter under BSDF sampling than under light sampling at these ratios.
#[test]
fn every_strategy_agrees_on_a_small_far_hidden_light() {
    let mirror = OpenPBR::metal(Vec3A::ONE, 0.0);
    let (radius, radiance_out) = (0.05, 5.0);
    let p = Vec3A::new(0.3, 0.0, 0.2);
    let mut rng = openqmc::pcg::Rng::new(17);
    for ratio in [40.0, 160.0, 200.0] {
        let center = Vec3A::new(0.0, ratio * radius, 0.0);
        let one = |source| {
            let l = lamp(center, radius, radiance_out, source);
            scene(mirror.clone(), false, &[&l])
        };
        let (hidden, solid) = (one(Source::Hidden), one(Source::Solid));
        let mut doubled = 0;
        for _ in 0..200 {
            // The reflected ray from `p` goes through `q`, a random point of
            // the disc the sphere presents to it: the eye is `q` mirrored
            // about the floor, seen through `p`.
            let r = 0.9 * radius * rng.next_f32().sqrt();
            let phi = std::f32::consts::TAU * rng.next_f32();
            let q = center + Vec3A::new(r * phi.cos(), 0.0, r * phi.sin());
            let eye = Vec3A::new(2.0 * p.x - q.x, q.y, 2.0 * p.z - q.z);
            let s = SamplingStrategy::BsdfOnly;
            let h = radiance(&hidden, eye, p, 1, s, 1);
            let o = radiance(&solid, eye, p, 1, s, 1);
            assert!(
                o.min_element() > 0.0,
                "ratio {ratio}: the bounce misses ({o})"
            );
            if rel(h, 2.0 * o) < 1e-3 {
                doubled += 1;
            }
            assert!(
                rel(h, o) < 1e-4,
                "ratio {ratio}: hidden {h} vs solid {o} through {q}"
            );
        }
        assert_eq!(
            doubled, 0,
            "ratio {ratio}: {doubled} of 200 bounces collected twice"
        );
    }
}

// ---------------------------------------------------------------------------
// Hidden lights neither shadow nor block one another
// ---------------------------------------------------------------------------

/// Two hidden sphere lights side by side over a diffuse floor: from the
/// floor point below, the near one covers most of the far one.
fn pair(source: Source) -> [Lamp; 2] {
    [
        lamp(Vec3A::new(0.0, 2.0, 0.0), 0.5, 10.0, source),
        lamp(Vec3A::new(0.6, 3.5, 0.0), 0.8, 6.0, source),
    ]
}

const EYE: Vec3A = Vec3A::new(0.4, 1.0, 0.3);

/// The floor under two overlapping hidden lights equals the sum of the two
/// single-light renders, under power MIS; light sampling alone and BSDF
/// sampling alone agree with it — at the depth limit and below it. A solid
/// pair breaks the sum, so the test can tell.
#[test]
fn hidden_lights_do_not_shadow_one_another() {
    let floor = OpenPBR::diffuse(Vec3A::splat(0.5));
    for depth in [1, 3] {
        let [a, b] = pair(Source::Hidden);
        let at = |lamps: &[&Lamp], s, n| {
            radiance(
                &scene(floor.clone(), false, lamps),
                EYE,
                Vec3A::ZERO,
                depth,
                s,
                n,
            )
        };
        let power = SamplingStrategy::PowerMis;
        let both = at(&[&a, &b], power, 8192);
        let sum = at(&[&a], power, 8192) + at(&[&b], power, 8192);
        assert!(both.min_element() > 0.05, "{both}");
        assert!(rel(both, sum) < 0.02, "depth {depth}: {both} vs {sum}");
        for (s, n, tol) in [
            (SamplingStrategy::LightOnly, 8192, 0.03),
            (SamplingStrategy::BsdfOnly, 32_768, 0.05),
        ] {
            let m = at(&[&a, &b], s, n);
            assert!(rel(m, both) < tol, "depth {depth}, {s:?}: {m} vs {both}");
        }

        let [a, b] = pair(Source::Solid);
        let solid = at(&[&a, &b], power, 8192);
        assert!(
            rel(solid, sum) > 0.1,
            "depth {depth}: a solid pair shadows ({solid} vs {sum})"
        );
    }
}

/// A hidden sphere just below a diffuse ceiling lit by a second light. The
/// ceiling patch above it is lit as if it were absent, and the floor below
/// receives that patch's bounce light through it: both are the sum of the
/// two single-light renders.
#[test]
fn what_lies_behind_a_hidden_light_is_lit() {
    let floor = OpenPBR::diffuse(Vec3A::splat(0.5));
    let key = lamp(Vec3A::new(3.0, 1.0, 0.0), 0.3, 30.0, Source::Hidden);
    let hidden = lamp(Vec3A::new(0.0, 3.5, 0.0), 0.4, 4.0, Source::Hidden);
    let power = SamplingStrategy::PowerMis;
    for (what, from, to) in [
        (
            "ceiling",
            Vec3A::new(0.3, 1.0, 0.2),
            Vec3A::new(0.0, 4.0, 0.0),
        ),
        ("floor", EYE, Vec3A::ZERO),
    ] {
        let at = |lamps: &[&Lamp]| {
            radiance(&scene(floor.clone(), true, lamps), from, to, 4, power, 8192)
        };
        let both = at(&[&key, &hidden]);
        let sum = at(&[&key]) + at(&[&hidden]);
        assert!(rel(both, sum) < 0.02, "{what}: {both} vs {sum}");
        let solid = lamp(hidden.center, hidden.radius, hidden.radiance, Source::Solid);
        let shadowed = at(&[&key, &solid]);
        assert!(
            rel(shadowed, sum) > 0.05,
            "{what}: a solid source shadows ({shadowed} vs {sum})"
        );
    }
}

/// A camera-visible lamp between the floor and a second light still
/// occludes it, on both sides of MIS: the floor is darker than the two
/// single-light renders sum to, and light-only and BSDF-only agree.
#[test]
fn a_visible_lamp_still_casts_a_shadow() {
    let floor = OpenPBR::diffuse(Vec3A::splat(0.5));
    let [a, b] = pair(Source::Visible);
    let at = |lamps: &[&Lamp], s, n| {
        radiance(
            &scene(floor.clone(), false, lamps),
            EYE,
            Vec3A::ZERO,
            1,
            s,
            n,
        )
    };
    let power = SamplingStrategy::PowerMis;
    let both = at(&[&a, &b], power, 8192);
    let sum = at(&[&a], power, 8192) + at(&[&b], power, 8192);
    assert!(rel(both, sum) > 0.1, "{both} vs {sum}");
    let bsdf = at(&[&a, &b], SamplingStrategy::BsdfOnly, 32_768);
    assert!(rel(bsdf, both) < 0.05, "{bsdf} vs {both}");
}

// ---------------------------------------------------------------------------
// Import: which sources are hidden
// ---------------------------------------------------------------------------

fn load(name: &str, body: &str) -> Scene {
    let dir = std::env::temp_dir().join("crust_hidden_lights_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path: PathBuf = dir.join(format!("{name}.usda"));
    let text = format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
{body}
}}
"#
    );
    std::fs::write(&path, text).expect("write stage");
    Scene::from_usd(&path).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn sphere_light(name: &str, x: f32, extra: &str) -> String {
    format!(
        r#"    def SphereLight "{name}"
    {{
        float inputs:radius = 0.5
        {extra}
        double3 xformOp:translate = ({x}, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
"#
    )
}

/// The `geom_id` a ray of `mask` from `+Z` meets at `x`, if any.
fn hit(scene: &Scene, x: f32, mask: crust_core::RayMask) -> Option<u32> {
    let ray = Ray::new(Vec3A::new(x, 0.0, 10.0), -Vec3A::Z).with_mask(mask);
    scene.world.intersect(&ray, 1e-3, 100.0).map(|h| h.geom_id)
}

/// A hidden source is seen by bounce rays alone and is a transparent
/// emitter; a camera-visible one (crust's attribute or RenderMan's) is seen
/// by every ray and stays solid; an authored `crust:rayMask` decides the
/// mask and keeps the source solid, whatever bits it clears.
#[test]
fn import_marks_hidden_sources_alone() {
    let body = [
        sphere_light("Hidden", 0.0, ""),
        sphere_light("Visible", 3.0, "bool crust:light:cameraVisible = 1"),
        sphere_light(
            "Prman",
            6.0,
            "int primvars:ri:attributes:visibility:camera = 1",
        ),
        sphere_light("Masked", 9.0, "int crust:rayMask = 4"),
        sphere_light("Blocker", 12.0, "int crust:rayMask = 6"),
    ]
    .concat();
    let scene = load("marks", &body);
    assert!(scene.world.has_transparent_emitters());
    let sees = |x, mask| hit(&scene, x, mask).is_some();
    let transparent = |x| {
        scene
            .world
            .is_transparent_emitter(hit(&scene, x, MASK_ALL).unwrap())
    };

    assert!(sees(0.0, MASK_INDIRECT));
    assert!(!sees(0.0, MASK_SHADOW), "a hidden source occludes");
    assert!(!sees(0.0, MASK_CAMERA));
    assert!(transparent(0.0));
    for x in [3.0, 6.0] {
        for mask in [MASK_CAMERA, MASK_SHADOW, MASK_INDIRECT] {
            assert!(sees(x, mask), "a visible source at {x} is missing a mask");
        }
        assert!(!transparent(x), "a visible source at {x} is transparent");
    }
    assert!(sees(9.0, MASK_INDIRECT) && !sees(9.0, MASK_SHADOW) && !sees(9.0, MASK_CAMERA));
    assert!(!transparent(9.0), "an authored mask keeps the source solid");
    assert!(sees(12.0, MASK_SHADOW) && !sees(12.0, MASK_CAMERA));
    assert!(!transparent(12.0));
}

/// A world whose only lights are camera-visible sources and a dome has no
/// transparent emitter, so its bounces walk exactly as before.
#[test]
fn no_hidden_source_no_transparent_emitter() {
    let body = [
        sphere_light("Visible", 0.0, "bool crust:light:cameraVisible = 1"),
        r#"    def DomeLight "Sky"
    {
    }
"#
        .to_string(),
    ]
    .concat();
    let scene = load("none", &body);
    assert_eq!(scene.lights.count(), 2);
    assert!(!scene.world.has_transparent_emitters());
    assert!(!scene.world.has_bounce_pass_throughs());
}

/// The two overlapping hidden lights inside fog, from the importer: a bounce
/// that crosses them pays the fog's transmittance up to each, and a scatter
/// before one means it was never reached. Power MIS, light sampling alone
/// and BSDF sampling alone agree on the floor, and the floor is the sum of
/// the two single-light renders.
#[test]
fn hidden_lights_in_fog_agree_across_strategies() {
    let stage = |name: &str, lights: &[(&str, f32, f32, f32)], strategy: &str, spp: u32| {
        let mut body = String::from(
            r#"    def Camera "Cam"
    {
        float focalLength = 50
        float horizontalAperture = 20
        float verticalAperture = 20
        double3 xformOp:translate = (0, 1, 0)
        float xformOp:rotateX = -90
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateX"]
    }
    def Mesh "Floor"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-10, 0, -10), (-10, 0, 10), (10, 0, 10), (10, 0, -10)]
    }
    def Cube "Fog"
    {
        double size = 8
        token crust:volume:type = "homogeneous"
        color3f crust:volume:sigmaS = (0.15, 0.15, 0.15)
        color3f crust:volume:sigmaA = (0.1, 0.1, 0.1)
        double3 xformOp:translate = (0, 4, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
"#,
        );
        for (light, x, y, r) in lights {
            body += &format!(
                r#"    def SphereLight "{light}"
    {{
        float inputs:radius = {r}
        float inputs:intensity = 20
        double3 xformOp:translate = ({x}, {y}, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
"#
            );
        }
        let dir = std::env::temp_dir().join("crust_hidden_lights_tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("{name}.usda"));
        std::fs::write(
            &path,
            format!(
                r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
{body}
}}

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        int2 resolution = (4, 4)
        int crust:samplesPerPixel = {spp}
        int crust:minSamplesPerPixel = {spp}
        int crust:maxDepth = 3
        float crust:varianceThreshold = 0
        float crust:indirectClamp = 0
        token crust:samplingStrategy = "{strategy}"
    }}
}}
"#
            ),
        )
        .expect("write stage");
        let scene = Scene::from_usd(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(scene.world.has_transparent_emitters());
        let b = crust_core::Renderer::new(scene.camera, scene.world, scene.lights, scene.settings)
            .with_volumes(scene.volumes)
            .render();
        let mut sum = Vec3A::ZERO;
        for y in 0..4 {
            for x in 0..4 {
                sum += b.get_pixel(x, y);
            }
        }
        sum / 16.0
    };
    let a = ("A", 0.0, 2.0, 0.5);
    let b = ("B", 0.6, 3.5, 0.8);
    let both = stage("fog_power", &[a, b], "power", 4096);
    let sum = stage("fog_a", &[a], "power", 4096) + stage("fog_b", &[b], "power", 4096);
    assert!(both.min_element() > 0.0, "{both}");
    assert!(rel(both, sum) < 0.03, "{both} vs {sum}");
    for (strategy, spp, tol) in [("light", 4096, 0.03), ("bsdf", 16_384, 0.06)] {
        let m = stage(&format!("fog_{strategy}"), &[a, b], strategy, spp);
        assert!(rel(m, both) < tol, "{strategy}: {m} vs {both}");
    }
}
