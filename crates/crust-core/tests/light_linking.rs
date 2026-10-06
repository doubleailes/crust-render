//! Light and shadow linking (`collection:lightLink` / `collection:shadowLink`),
//! driven by small stages written on the fly: membership through openusd's
//! collection query, the two MIS sides of light linking, and the shadow-class
//! encoding with its NEE-only rule.

use crust_core::{
    Buffer, EVERY_CLASS, MASK_CAMERA, MASK_INDIRECT, MASK_SHADOW, Ray, RayMask, Renderer, Scene,
    Vec3A,
};
use std::path::PathBuf;

fn write_stage(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_light_linking_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, text).expect("write stage");
    path
}

/// `body` under `/World`, plus a `/Render/settings` prim when given.
fn load(name: &str, body: &str, settings: &str) -> Scene {
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
{settings}
"#
    );
    let path = write_stage(name, &text);
    Scene::from_usd(&path).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn render(scene: Scene) -> Buffer {
    Renderer::new(scene.camera, scene.world, scene.lights, scene.settings)
        .with_volumes(scene.volumes)
        .render()
}

fn settings(strategy: &str, spp: u32, size: u32) -> String {
    settings_at_depth(strategy, spp, size, 1)
}

fn settings_at_depth(strategy: &str, spp: u32, size: u32, depth: u32) -> String {
    format!(
        r#"
def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        int2 resolution = ({size}, {size})
        int crust:samplesPerPixel = {spp}
        int crust:minSamplesPerPixel = {spp}
        int crust:maxDepth = {depth}
        float crust:varianceThreshold = 0
        float crust:indirectClamp = 0
        token crust:samplingStrategy = "{strategy}"
    }}
}}"#
    )
}

/// The `geom_id` a camera ray from `+Z` hits at `(x, y)`.
fn geom_at(scene: &Scene, x: f32, y: f32) -> u32 {
    let ray = Ray::new(Vec3A::new(x, y, 50.0), -Vec3A::Z).with_mask(MASK_CAMERA);
    scene
        .world
        .intersect(&ray, 1e-3, 1e4)
        .unwrap_or_else(|| panic!("nothing at ({x}, {y})"))
        .geom_id
}

/// The light-list index of the sphere light above `x` (at `y = 20`).
fn light_above(scene: &Scene, x: f32) -> usize {
    let ray = Ray::new(Vec3A::new(x, 40.0, 0.0), -Vec3A::Y).with_mask(MASK_INDIRECT);
    let hit = scene.world.intersect(&ray, 1e-3, 1e4).expect("a light");
    scene
        .lights
        .find_index_by_geom_at(hit.geom_id, Vec3A::ZERO)
        .expect("a light's geometry")
        .0
}

fn sphere(name: &str, x: f32, extra: &str) -> String {
    format!(
        r#"    def Sphere "{name}"
    {{
        double radius = 0.5
        {extra}
        double3 xformOp:translate = ({x}, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
"#
    )
}

fn sphere_light(name: &str, x: f32, links: &str) -> String {
    format!(
        r#"    def SphereLight "{name}"
    {{
        float inputs:radius = 0.5
        {links}
        double3 xformOp:translate = ({x}, 20, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
"#
    )
}

// ---------------------------------------------------------------------------
// Membership (openusd's collection query)
// ---------------------------------------------------------------------------

#[test]
fn membership_follows_usd_collection_rules() {
    let body = [
        r#"    def Xform "Set"
    {
        def Sphere "Chair"
        {
            double radius = 0.5
        }
    }
"#
        .to_string(),
        sphere("Hero", 3.0, ""),
        r#"    def Xform "Group"
    {
        def Sphere "Body"
        {
            double radius = 0.5
            double3 xformOp:translate = (6, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
    }
"#
        .to_string(),
        sphere("Other", 9.0, ""),
        r#"    def Scope "Coll" (prepend apiSchemas = ["CollectionAPI:friends"])
    {
        uniform bool collection:friends:includeRoot = 0
        prepend rel collection:friends:includes = </World/Hero>
    }
    def Scope "A" (prepend apiSchemas = ["CollectionAPI:a"])
    {
        uniform bool collection:a:includeRoot = 0
        prepend rel collection:a:includes = </World/B.collection:b>
    }
    def Scope "B" (prepend apiSchemas = ["CollectionAPI:b"])
    {
        uniform bool collection:b:includeRoot = 0
        prepend rel collection:b:includes = </World/A.collection:a>
    }
"#
        .to_string(),
        sphere_light(
            "Nearest",
            0.0,
            "uniform bool collection:lightLink:includeRoot = 0\n        \
             prepend rel collection:lightLink:includes = </World>\n        \
             prepend rel collection:lightLink:excludes = </World/Set>",
        ),
        sphere_light(
            "Explicit",
            10.0,
            "uniform bool collection:lightLink:includeRoot = 0\n        \
             uniform token collection:lightLink:expansionRule = \"explicitOnly\"\n        \
             prepend rel collection:lightLink:includes = [</World/Group>, </World/Hero>]",
        ),
        sphere_light(
            "Nested",
            20.0,
            "uniform bool collection:lightLink:includeRoot = 0\n        \
             prepend rel collection:lightLink:includes = </World/Coll.collection:friends>",
        ),
        sphere_light(
            "Rooted",
            30.0,
            "prepend rel collection:lightLink:excludes = </World/Hero>",
        ),
        sphere_light("Plain", 40.0, ""),
        sphere_light(
            "Cycle",
            50.0,
            "uniform bool collection:lightLink:includeRoot = 0\n        \
             prepend rel collection:lightLink:includes = </World/A.collection:a>",
        ),
    ]
    .concat();
    let scene = load("membership", &body, "");

    // The cycle resolves (openusd breaks it) to no member: that light
    // illuminates nothing and has left the list.
    assert_eq!(scene.lights.count(), 5);

    let class = |x: f32| scene.world.light_class(geom_at(&scene, x, 0.0));
    let (chair, hero, body, other) = (class(0.0), class(3.0), class(6.0), class(9.0));
    let lit = |light_x: f32, receiver: u16| {
        scene
            .lights
            .illuminates(light_above(&scene, light_x), receiver)
    };
    // The nearest opinion wins: /World/Set is excluded under an included /World.
    assert_eq!(
        [
            lit(0.0, chair),
            lit(0.0, hero),
            lit(0.0, body),
            lit(0.0, other)
        ],
        [false, true, true, true]
    );
    // explicitOnly: the named paths only, not /World/Group's child.
    assert_eq!(
        [
            lit(10.0, chair),
            lit(10.0, hero),
            lit(10.0, body),
            lit(10.0, other)
        ],
        [false, true, false, false]
    );
    // A nested collection's members.
    assert_eq!(
        [
            lit(20.0, chair),
            lit(20.0, hero),
            lit(20.0, body),
            lit(20.0, other)
        ],
        [false, true, false, false]
    );
    // includeRoot unauthored is UsdLux's fallback, true: all but the exclude.
    assert_eq!(
        [
            lit(30.0, chair),
            lit(30.0, hero),
            lit(30.0, body),
            lit(30.0, other)
        ],
        [true, false, true, true]
    );
    // The default collection illuminates everything.
    assert!([chair, hero, body, other].iter().all(|&c| lit(40.0, c)));
    // A volume-region scatter (no prim) is lit by every light.
    assert!(lit(20.0, EVERY_CLASS));
}

#[test]
fn a_default_collection_builds_nothing() {
    let body = [sphere("Ball", 0.0, ""), sphere_light("L", 0.0, "")].concat();
    let scene = load("default_links", &body, "");
    assert!(scene.lights.links().is_none());
    assert_eq!(
        scene.world.light_class(geom_at(&scene, 0.0, 0.0)),
        EVERY_CLASS
    );
}

// ---------------------------------------------------------------------------
// Light linking through the integrator
// ---------------------------------------------------------------------------

/// A floor and a hero ball under a sphere light linked to the hero alone,
/// seen from above. Depth 1: direct light only.
fn linked_scene(strategy: &str, spp: u32) -> Scene {
    let body = r#"    def Camera "Cam"
    {
        float focalLength = 35
        float horizontalAperture = 36
        float verticalAperture = 36
        double3 xformOp:translate = (0, 6, 0)
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
    def Sphere "Hero"
    {
        double radius = 1
        double3 xformOp:translate = (0, 1, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def SphereLight "Key"
    {
        float inputs:radius = 1
        float inputs:intensity = 20
        uniform bool collection:lightLink:includeRoot = 0
        prepend rel collection:lightLink:includes = </World/Hero>
        double3 xformOp:translate = (1, 4, 1)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
"#
    .to_string();
    load(
        &format!("linked_{strategy}_{spp}"),
        &body,
        &settings(strategy, spp, 16),
    )
}

/// Sum over the centre (hero) pixels, and the largest corner (floor) pixel.
fn hero_and_floor(b: &Buffer) -> (f64, f32) {
    let mut hero = 0.0;
    for y in 6..10 {
        for x in 6..10 {
            let c = b.get_pixel(x, y);
            hero += (c.x + c.y + c.z) as f64;
        }
    }
    let floor = [(0, 0), (15, 0), (0, 15), (15, 15)]
        .iter()
        .map(|&(x, y)| b.get_pixel(x, y).max_element())
        .fold(0.0f32, f32::max);
    (hero, floor)
}

#[test]
fn an_unlinked_receiver_is_black_under_every_strategy() {
    for strategy in ["power", "light", "bsdf"] {
        let (hero, floor) = hero_and_floor(&render(linked_scene(strategy, 16)));
        assert_eq!(floor, 0.0, "{strategy}: the floor is not linked");
        assert!(hero > 0.0, "{strategy}: the hero is");
    }
}

/// NEE alone and BSDF sampling alone must reach the same answer on the
/// linked receiver: both sides apply the same filter.
///
/// 1024 spp on purpose. The 16 spp rule for image comparisons (`CLAUDE.md`)
/// exists because above `min_samples_per_pixel` the adaptive early stop
/// makes a one-ulp change cascade; `settings` pins `minSamplesPerPixel` to the
/// sample count and the variance threshold to 0, so nothing adapts here. And
/// this is not a regression diff but a test that two *different* estimators
/// have the same mean, which needs enough samples for their noise (BSDF-only
/// finding a small light is the noisy one) to fall well inside 5%.
#[test]
fn nee_only_and_bsdf_only_agree_on_a_linked_receiver() {
    let (nee, _) = hero_and_floor(&render(linked_scene("light", 1024)));
    let (bsdf, _) = hero_and_floor(&render(linked_scene("bsdf", 1024)));
    let rel = (nee - bsdf).abs() / nee;
    assert!(rel < 0.05, "NEE {nee} vs BSDF {bsdf}: {rel:.3}");
}

#[test]
fn a_dome_excluded_from_a_prim_does_not_reach_it() {
    let body = r#"    def Camera "Cam"
    {
        float focalLength = 35
        float horizontalAperture = 36
        float verticalAperture = 36
        double3 xformOp:translate = (0, 0, 8)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def DomeLight "Sky"
    {
        prepend rel collection:lightLink:excludes = </World/Set>
    }
    def Sphere "Set"
    {
        double radius = 1
        double3 xformOp:translate = (-1.5, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Sphere "Prop"
    {
        double radius = 1
        double3 xformOp:translate = (1.5, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
"#
    .to_string();
    // Depth 1 for NEE; depth 2 for BSDF sampling, which finds a dome only by
    // a scored escape (a depth-exhausted bounce scores emitters it hits, not
    // the background). At depth 2 the only way Set could be lit is that
    // escape, since BSDF-only runs no NEE at the Prop vertex behind it.
    for (strategy, depth) in [("power", 1), ("bsdf", 2)] {
        let b = render(load(
            &format!("dome_exclude_{strategy}"),
            &body,
            &settings_at_depth(strategy, 16, 32, depth),
        ));
        // Row 16 crosses both balls' centres (x ≈ 8 and ≈ 24).
        assert_eq!(b.get_pixel(8, 16).max_element(), 0.0, "{strategy}: Set");
        assert!(b.get_pixel(24, 16).max_element() > 0.0, "{strategy}: Prop");
    }
}

// ---------------------------------------------------------------------------
// Shadow linking
// ---------------------------------------------------------------------------

/// A floor seen from above, lit by a sphere light off to the side, with
/// `blocker` (authored under `/World/Blocker`) between the light and the
/// floor's centre, outside the camera's view.
fn shadow_scene(name: &str, blocker: &str, light_links: &str, strategy: &str) -> Scene {
    let body = format!(
        r#"    def Camera "Cam"
    {{
        float focalLength = 100
        float horizontalAperture = 20
        float verticalAperture = 20
        double3 xformOp:translate = (0, 6, 0)
        float xformOp:rotateX = -90
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateX"]
    }}
    def Mesh "Floor"
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-10, 0, -10), (-10, 0, 10), (10, 0, 10), (10, 0, -10)]
    }}
    def SphereLight "Key"
    {{
        float inputs:radius = 0.25
        float inputs:intensity = 50
        {light_links}
        double3 xformOp:translate = (6, 5, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
{blocker}
"#
    );
    load(name, &body, &settings(strategy, 16, 8))
}

const BALL: &str = r#"    def Sphere "Blocker"
    {
        double radius = 0.8
        int crust:rayMask = 6
        double3 xformOp:translate = (3, 2.5, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#;

const FOG: &str = r#"    def Cube "Blocker"
    {
        double size = 1.6
        token crust:volume:type = "homogeneous"
        color3f crust:volume:sigmaS = (0, 0, 0)
        color3f crust:volume:sigmaA = (3, 3, 3)
        double3 xformOp:translate = (3, 2.5, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#;

const EXCLUDE: &str = "prepend rel collection:shadowLink:excludes = </World/Blocker>";

fn centre(b: &Buffer) -> Vec3A {
    let mut s = Vec3A::ZERO;
    for y in 3..5 {
        for x in 3..5 {
            s += b.get_pixel(x, y);
        }
    }
    s
}

#[test]
fn an_excluded_occluder_or_volume_casts_no_nee_shadow() {
    let open = centre(&render(shadow_scene("shadow_open", "", "", "light")));
    assert!(open.max_element() > 0.0);
    for (kind, blocker) in [("ball", BALL), ("fog", FOG)] {
        let blocked = centre(&render(shadow_scene(
            &format!("shadow_{kind}_blocked"),
            blocker,
            "",
            "light",
        )));
        assert!(
            blocked.max_element() < 0.5 * open.max_element(),
            "{kind}: it shadows the floor without a link ({blocked} vs {open})"
        );
        let excluded = centre(&render(shadow_scene(
            &format!("shadow_{kind}_excluded"),
            blocker,
            EXCLUDE,
            "light",
        )));
        assert!(
            (excluded - open).abs().max_element() <= 1e-4 * open.max_element(),
            "{kind}: excluded, the floor is lit as if it were absent ({excluded} vs {open})"
        );
    }
}

/// A shadow-linked light is sampled by NEE alone at a continuous vertex, so
/// power MIS and NEE-only are the same estimator for it.
#[test]
fn power_mis_matches_nee_only_for_a_shadow_linked_light() {
    let mis = centre(&render(shadow_scene(
        "restricted_power",
        BALL,
        EXCLUDE,
        "power",
    )));
    let nee = centre(&render(shadow_scene(
        "restricted_light",
        BALL,
        EXCLUDE,
        "light",
    )));
    assert!(
        (mis - nee).abs().max_element() <= 1e-4 * nee.max_element(),
        "{mis} vs {nee}"
    );
}

/// 30 lights, each excluding its own occluder, make 30 occluder classes:
/// 28 get a bit, two share the overflow bit. The two lights whose excluded
/// occluder overflows cannot be encoded and fall back to full shadowing; the
/// overflow occluders still block every other light.
#[test]
fn overflow_occluders_block_unrestricted_and_refused_lights() {
    let mut body = String::new();
    for i in 0..30 {
        let x = 3.0 * i as f32;
        body += &format!(
            r#"    def Sphere "O{i}"
    {{
        double radius = 0.5
        double3 xformOp:translate = ({x}, 10, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
"#
        );
        body += &sphere_light(
            &format!("L{i}"),
            x,
            &format!("prepend rel collection:shadowLink:excludes = </World/O{i}>"),
        );
    }
    body += &sphere_light("Free", 200.0, "");
    let scene = load("overflow", &body, "");
    assert_eq!(scene.lights.count(), 31);

    // Whether a shadow ray from below occluder `i` toward light `light`
    // (straight up to y = 19) is blocked.
    let blocked = |i: usize, light: usize| {
        let ray = Ray::new(Vec3A::new(3.0 * i as f32, 0.0, 0.0), Vec3A::Y)
            .with_mask(scene.lights.shadow_mask(light));
        scene.world.occluded(&ray, 1e-3, 19.0)
    };
    let free = light_above(&scene, 200.0);
    let per_light: Vec<usize> = (0..30)
        .map(|i| light_above(&scene, 3.0 * i as f32))
        .collect();
    let refused: Vec<usize> = (0..30)
        .filter(|&i| !scene.lights.nee_only(per_light[i]))
        .collect();
    assert_eq!(
        refused.len(),
        2,
        "two occluder classes share the overflow bit"
    );
    for (i, &own) in per_light.iter().enumerate() {
        assert!(blocked(i, free), "O{i} blocks an unrestricted light");
        if refused.contains(&i) {
            assert!(blocked(i, own), "a refused light is shadowed by everything");
        } else {
            assert!(!blocked(i, own), "L{i} ignores its excluded O{i}");
        }
        for &r in &refused {
            if r != i {
                assert!(blocked(r, own), "overflow O{r} still blocks L{i}");
            }
        }
    }
}

/// With no link anywhere, an authored `crust:rayMask` keeps its bits 3–31:
/// a ray carrying only bit 7 still hits geometry authored with it.
#[test]
fn authored_high_mask_bits_are_kept_without_links() {
    let body = sphere("Ball", 0.0, "int crust:rayMask = 134");
    let scene = load("high_bits", &body, "");
    let ray = Ray::new(Vec3A::new(0.0, 0.0, 10.0), -Vec3A::Z).with_mask(RayMask(1 << 7));
    assert!(scene.world.occluded(&ray, 1e-3, 100.0));
    let shadow = Ray::new(Vec3A::new(0.0, 0.0, 10.0), -Vec3A::Z).with_mask(MASK_SHADOW);
    assert!(scene.world.occluded(&shadow, 1e-3, 100.0));
}

/// A link authored only in a payload on an existing light prim: the index
/// stage (payloads unloaded) has the prim but not the link, so it must not be
/// the one the link is read on. (The payload authors `includeRoot = 0`
/// alone: a relationship target outside the arc's namespace, such as
/// `</World/Hero>`, is not mapped through a payload.)
#[test]
fn a_link_authored_in_a_payload_is_honoured() {
    write_stage(
        "payload_links_layer",
        r#"#usda 1.0
def Xform "World"
{
    def SphereLight "Key"
    {
        uniform bool collection:lightLink:includeRoot = 0
    }
}
"#,
    );
    let body = r#"    def Camera "Cam"
    {
        float focalLength = 35
        float horizontalAperture = 36
        float verticalAperture = 36
        double3 xformOp:translate = (0, 6, 0)
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
    def Sphere "Hero"
    {
        double radius = 1
        double3 xformOp:translate = (0, 1, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def SphereLight "Key" (
        prepend payload = @payload_links_layer.usda@</World/Key>
    )
    {
        float inputs:radius = 1
        float inputs:intensity = 20
        double3 xformOp:translate = (1, 4, 1)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
"#;
    let scene = load("payload_links", body, &settings("power", 16, 16));
    // Read on the index stage the link would be the default and Key would
    // light everything; the payload's link includes nothing.
    assert_eq!(scene.lights.count(), 0, "the payload's link is read");
    let (hero, floor) = hero_and_floor(&render(scene));
    assert_eq!((hero, floor), (0.0, 0.0));
}

/// A volume region a light's `lightLink` excludes gets nothing from it, on
/// the NEE side and on the phase-sampled bounce side alike.
#[test]
fn a_volume_excluded_by_a_light_link_is_not_lit() {
    let body = |links: &str| {
        format!(
            r#"    def Camera "Cam"
    {{
        float focalLength = 35
        float horizontalAperture = 36
        float verticalAperture = 36
        double3 xformOp:translate = (0, 0, 8)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def Cube "Fog"
    {{
        double size = 3
        token crust:volume:type = "homogeneous"
        color3f crust:volume:sigmaS = (1, 1, 1)
        color3f crust:volume:sigmaA = (0, 0, 0)
    }}
    def SphereLight "Key"
    {{
        float inputs:radius = 1
        float inputs:intensity = 20
        {links}
        double3 xformOp:translate = (0, 4, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
"#
        )
    };
    let exclude = "prepend rel collection:lightLink:excludes = </World/Fog>";
    for (strategy, depth) in [("power", 1), ("bsdf", 2)] {
        let lit = render(load(
            &format!("fog_lit_{strategy}"),
            &body(""),
            &settings_at_depth(strategy, 16, 8, depth),
        ));
        let dark = render(load(
            &format!("fog_excluded_{strategy}"),
            &body(exclude),
            &settings_at_depth(strategy, 16, 8, depth),
        ));
        assert!(
            centre(&lit).max_element() > 0.0,
            "{strategy}: the fog is lit"
        );
        assert_eq!(
            centre(&dark),
            Vec3A::ZERO,
            "{strategy}: excluded, it is dark"
        );
    }
}

/// A hidden light's source is in no shadow class: shadow rays toward a
/// restricted light (which carry class bits) and toward an unrestricted one
/// both pass it, while the occluder the restricted light does not exclude
/// still blocks it, and a camera-visible source still blocks both.
#[test]
fn a_hidden_light_source_is_no_caster_under_shadow_linking() {
    let mut body = String::new();
    // Two occluders, so the restricted light has a class to ignore and one
    // to be blocked by.
    body += &sphere("Kept", 50.0, "");
    body += &sphere("Dropped", 30.0, "");
    body += &sphere_light(
        "Restricted",
        100.0,
        "prepend rel collection:shadowLink:excludes = </World/Dropped>",
    );
    body += &sphere_light("Free", 200.0, "");
    // The sources under test, at y = 10 (the occluders are at y = 0).
    for (name, x, extra) in [
        ("Hidden", 3.0, ""),
        ("Visible", 6.0, "int crust:light:cameraVisible = 1"),
    ] {
        body += &format!(
            r#"    def SphereLight "{name}"
    {{
        float inputs:radius = 0.5
        {extra}
        double3 xformOp:translate = ({x}, 10, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
"#
        );
    }
    let scene = load("hidden_source_classes", &body, "");
    let restricted = light_above(&scene, 100.0);
    let free = light_above(&scene, 200.0);
    assert!(scene.lights.nee_only(restricted), "the link was encoded");
    let blocked = |x: f32, light: usize| {
        let ray =
            Ray::new(Vec3A::new(x, -5.0, 0.0), Vec3A::Y).with_mask(scene.lights.shadow_mask(light));
        scene.world.occluded(&ray, 1e-3, 24.0)
    };
    for light in [restricted, free] {
        assert!(!blocked(3.0, light), "a hidden source blocks light {light}");
        assert!(
            blocked(6.0, light),
            "a visible source lets light {light} through"
        );
        assert!(
            blocked(50.0, light),
            "the kept occluder lets light {light} through"
        );
    }
    assert!(!blocked(30.0, restricted), "the excluded occluder blocks");
    assert!(
        blocked(30.0, free),
        "the excluded occluder still blocks others"
    );
}
