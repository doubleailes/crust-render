//! Motion over the shutter, from the stage to the film: `disableMotionBlur`
//! on the render settings, the per-geometry motion record `WorldBuilder`
//! derives from the kernel's end transforms, and the `motionvector` AOV
//! built on both.

use crust_core::rt::{Geometry, InstanceHitId, SceneBuilder};
use crust_core::{
    AovFilm, AovProduct, AovRequest, AovSource, AovVar, Buffer, MASK_ALL, OpenPBR, Precision, Ray,
    Renderer, Scene, Vec3A, WorldBuilder,
};
use glam::{Affine3A, Quat, Vec3};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const W: usize = 64;
const H: usize = 36;

fn write_stage(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_motion_vector_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, text).expect("write stage");
    path
}

fn load(name: &str, text: &str) -> Scene {
    let path = write_stage(name, text);
    Scene::from_usd(&path).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// An emissive sphere of radius 0.4 at x = −0.6, streaking 1.2 to the right
/// over the shutter, seen by a default (50 mm) camera at z = 8 looking down
/// −Z, with no light and no dome: everything but the sphere is black. At
/// this distance one world unit is about 19 pixels, so the authored
/// silhouette spans columns 13–28 and the shutter-close one 36–51.
fn streak_stage(settings: &str) -> String {
    format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
    def Camera "cam"
    {{
        double3 xformOp:translate = (0, 0, 8)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def Material "Glow"
    {{
        token outputs:surface.connect = </World/Glow/S.outputs:surface>
        def Shader "S"
        {{
            uniform token info:id = "crust:openpbr"
            float inputs:emissionLuminance = 3
            color3f inputs:emissionColor = (1, 1, 1)
            token outputs:surface
        }}
    }}
    def Sphere "Mover" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        double radius = 0.4
        float3 crust:motion:translate = (1.2, 0, 0)
        double3 xformOp:translate = (-0.6, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        rel material:binding = </World/Glow>
    }}
}}

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/cam>
        int2 resolution = ({W}, {H})
        int crust:samplesPerPixel = 16
        int crust:minSamplesPerPixel = 16
        int crust:maxDepth = 2
{settings}
    }}
}}
"#
    )
}

fn render(scene: Scene) -> Buffer {
    assert!(scene.world.has_motion());
    Renderer::new(scene.camera, scene.world, scene.lights, scene.settings).render_with_tiles()
}

fn lit(b: &Buffer, x: usize, y: usize) -> bool {
    b.get_pixel(x, y).max_element() > 0.0
}

fn bits(b: &Buffer) -> Vec<[u32; 3]> {
    (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .map(|(x, y)| {
            let c = b.get_pixel(x, y);
            [c.x.to_bits(), c.y.to_bits(), c.z.to_bits()]
        })
        .collect()
}

/// The columns a blurred sphere lights and a sharp one must not: well right
/// of the authored silhouette (column 28), inside the shutter-close one.
fn streak() -> std::ops::Range<usize> {
    34..52
}

#[test]
fn disabled_motion_blur_renders_the_sphere_sharp_at_shutter_open() {
    let scene = load(
        "sharp",
        &streak_stage("        uniform bool disableMotionBlur = 1"),
    );
    assert!(!scene.settings.motion_blur());
    let image = render(scene);
    // The authored silhouette is lit...
    assert!(lit(&image, 20, H / 2));
    // ...and nothing right of it is.
    for y in 0..H {
        for x in streak() {
            assert!(!lit(&image, x, y), "pixel ({x}, {y}) lit with blur off");
        }
    }
}

#[test]
fn motion_blur_stays_on_by_default() {
    let scene = load("blurred", &streak_stage(""));
    assert!(scene.settings.motion_blur());
    let image = render(scene);
    assert!(lit(&image, 20, H / 2));
    assert!(
        streak().any(|x| lit(&image, x, H / 2)),
        "no pixel of the streak is lit with blur on"
    );
}

#[test]
fn instantaneous_shutter_is_bit_identical_to_disable_motion_blur() {
    let a = render(load(
        "synonym_a",
        &streak_stage("        uniform bool disableMotionBlur = 1"),
    ));
    let b = render(load(
        "synonym_b",
        &streak_stage("        uniform bool instantaneousShutter = 1"),
    ));
    assert_eq!(bits(&a), bits(&b));
}

// ---------------------------------------------------------------------------
// The per-geometry motion record
// ---------------------------------------------------------------------------

/// Counts the WARN events emitted on this thread.
struct CountWarnings(Arc<AtomicUsize>);

impl tracing::Subscriber for CountWarnings {
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
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn grey() -> Arc<OpenPBR> {
    Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5)))
}

/// A unit sphere at the origin, as a scene to instance.
fn ball() -> Arc<crust_core::rt::Scene> {
    let mut inner = SceneBuilder::new();
    inner.attach(Geometry::Sphere {
        center: Vec3A::ZERO,
        radius: 1.0,
    });
    Arc::new(inner.commit())
}

fn quad() -> Geometry {
    Geometry::TriangleMesh {
        vertices: vec![
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
        ],
        indices: vec![[0, 1, 2], [0, 2, 3]],
        normals: None,
    }
}

fn instance(scene: Arc<crust_core::rt::Scene>, l: Affine3A, end: Option<Affine3A>) -> Geometry {
    Geometry::Instance {
        scene,
        transform: l,
        transform_end: end.map(Box::new),
    }
}

/// The record is read off the end transform the kernel interpolates: the
/// importer's two shapes — a sphere over an identity placement, a mesh's
/// `T(v)·L` — give the authored `v`, and whatever has no end transform gives
/// zero.
#[test]
fn motion_records_follow_the_kernels_end_transforms() {
    let v_sphere = Vec3A::new(1.0, 0.0, 0.0);
    let v_mesh = Vec3A::new(0.0, 2.0, -0.5);
    // A placement with a rotation and a non-uniform scale, moved by `v`.
    let l = Affine3A::from_scale_rotation_translation(
        Vec3::new(2.0, 1.0, 0.5),
        Quat::from_rotation_z(0.7),
        Vec3::new(3.0, 0.0, 0.0),
    );
    let mut b = WorldBuilder::new();
    let sphere = b.attach(
        instance(
            ball(),
            Affine3A::IDENTITY,
            Some(Affine3A::from_translation(v_sphere.into())),
        ),
        grey(),
    );
    let mesh = b.attach(
        instance(
            ball(),
            l,
            Some(Affine3A::from_translation(v_mesh.into()) * l),
        ),
        grey(),
    );
    let placed = b.attach(instance(ball(), l, None), grey());
    let baked = b.attach(quad(), grey());
    assert_eq!(b.unresolved_motion(), 0);
    let world = b.commit();
    assert!(world.has_motion());
    assert_eq!(world.motion(sphere), v_sphere);
    assert_eq!(world.motion(mesh), v_mesh);
    assert_eq!(world.motion(placed), Vec3A::ZERO);
    assert_eq!(world.motion(baked), Vec3A::ZERO);
    // An id no geometry has is zero too, never a panic.
    assert_eq!(world.motion(99), Vec3A::ZERO);
}

/// Motion the record cannot express blurs in the kernel and reads zero: an
/// end transform whose linear part differs from the start's, a forwarded
/// label (two placements share the id), and an instance over a scene that
/// itself moves. Each is counted, and the commit warns once for all of them.
#[test]
fn motion_the_record_cannot_express_is_counted_and_reads_zero() {
    let mut nested = SceneBuilder::new();
    nested.attach(instance(
        ball(),
        Affine3A::IDENTITY,
        Some(Affine3A::from_translation(Vec3::X)),
    ));
    let nested = Arc::new(nested.commit());
    assert!(nested.has_motion());

    let mut b = WorldBuilder::new();
    let rotating = b.attach(
        instance(
            ball(),
            Affine3A::IDENTITY,
            Some(Affine3A::from_rotation_y(0.5)),
        ),
        grey(),
    );
    let scaling = b.attach(
        instance(
            ball(),
            Affine3A::IDENTITY,
            Some(Affine3A::from_scale(Vec3::splat(2.0))),
        ),
        grey(),
    );
    let forwarded = b.attach_labelled(
        instance(
            ball(),
            Affine3A::IDENTITY,
            Some(Affine3A::from_translation(Vec3::X)),
        ),
        grey(),
        MASK_ALL,
        InstanceHitId::As(0),
    );
    let over_moving = b.attach(instance(nested.clone(), Affine3A::IDENTITY, None), grey());
    let over_moving_and_moving = b.attach(
        instance(
            nested,
            Affine3A::IDENTITY,
            Some(Affine3A::from_translation(Vec3::X)),
        ),
        grey(),
    );
    // And one the record does express, beside them.
    let moving = b.attach(
        instance(
            ball(),
            Affine3A::IDENTITY,
            Some(Affine3A::from_translation(Vec3::Y)),
        ),
        grey(),
    );
    assert_eq!(b.unresolved_motion(), 5);

    let warnings = Arc::new(AtomicUsize::new(0));
    let world = tracing::subscriber::with_default(CountWarnings(warnings.clone()), || b.commit());
    assert_eq!(
        warnings.load(Ordering::Relaxed),
        1,
        "one summarised warning"
    );
    for id in [
        rotating,
        scaling,
        forwarded,
        over_moving,
        over_moving_and_moving,
    ] {
        assert_eq!(world.motion(id), Vec3A::ZERO, "geometry {id}");
    }
    assert_eq!(world.motion(moving), Vec3A::Y);

    // A world without any of them commits silently.
    let mut quiet = WorldBuilder::new();
    quiet.attach(quad(), grey());
    let warnings = Arc::new(AtomicUsize::new(0));
    let _ = tracing::subscriber::with_default(CountWarnings(warnings.clone()), || quiet.commit());
    assert_eq!(warnings.load(Ordering::Relaxed), 0);
}

/// The importer decides a mesh's representation late, through a reserved
/// slot: the record follows the geometry the slot is finally given.
#[test]
fn a_reserved_slot_records_the_motion_of_the_geometry_it_is_given() {
    let mut b = WorldBuilder::new();
    let a = b.reserve_slot(grey(), MASK_ALL);
    let c = b.reserve_slot(grey(), MASK_ALL);
    let d = b.reserve_slot(grey(), MASK_ALL);
    // Out of id order, so the table has to stay sorted on its own.
    b.set_geometry(
        d,
        instance(
            ball(),
            Affine3A::IDENTITY,
            Some(Affine3A::from_translation(Vec3::Z)),
        ),
    );
    b.set_geometry(
        a,
        instance(
            ball(),
            Affine3A::IDENTITY,
            Some(Affine3A::from_translation(Vec3::X)),
        ),
    );
    b.set_geometry(c, quad());
    // Given again, the earlier record goes.
    b.set_geometry(d, instance(ball(), Affine3A::IDENTITY, None));
    let world = b.commit();
    assert_eq!(world.motion(a), Vec3A::X);
    assert_eq!(world.motion(c), Vec3A::ZERO);
    assert_eq!(world.motion(d), Vec3A::ZERO);
}

/// Through the importer: a moving sphere and a moving mesh carry their
/// authored `crust:motion:translate`, a static prim and a baked
/// (non-invertible) mesh whose motion the importer dropped carry zero.
#[test]
fn imported_geometry_carries_its_authored_translation() {
    let scene = load(
        "imported_motion",
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{
    def Sphere "Mover"
    {
        double radius = 0.5
        float3 crust:motion:translate = (0.7, 0, 0)
        double3 xformOp:translate = (-3, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Mesh "Riser"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-0.5, -0.5, 0), (0.5, -0.5, 0), (0.5, 0.5, 0), (-0.5, 0.5, 0)]
        float3 crust:motion:translate = (0, 0.5, 0.25)
        double3 xformOp:translate = (0, 0, 0)
        float3 xformOp:scale = (2, 1, 1)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:scale"]
    }
    def Sphere "Still"
    {
        double radius = 0.5
        double3 xformOp:translate = (3, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Mesh "Flat"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-0.5, -0.5, 0), (0.5, -0.5, 0), (0.5, 0.5, 0), (-0.5, 0.5, 0)]
        float3 crust:motion:translate = (1, 0, 0)
        double3 xformOp:translate = (6, 0, 0)
        float3 xformOp:scale = (1, 1, 0)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:scale"]
    }
}
"#,
    );
    assert_eq!(scene.world.count(), 4);
    let hit = |x: f32| {
        scene
            .world
            .intersect(&Ray::new(Vec3A::new(x, 0.0, 5.0), -Vec3A::Z), 1e-3, 100.0)
            .unwrap_or_else(|| panic!("nothing at x = {x}"))
            .geom_id
    };
    assert_eq!(scene.world.motion(hit(-3.0)), Vec3A::new(0.7, 0.0, 0.0));
    assert_eq!(scene.world.motion(hit(0.0)), Vec3A::new(0.0, 0.5, 0.25));
    assert_eq!(scene.world.motion(hit(3.0)), Vec3A::ZERO);
    assert_eq!(scene.world.motion(hit(6.0)), Vec3A::ZERO);
}

// ---------------------------------------------------------------------------
// The sample scene and the AOV invariants with a motionvector var present
// ---------------------------------------------------------------------------

fn sample_scene() -> Scene {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("samples/motionvector.usda");
    Scene::from_usd(&path).expect("samples/motionvector.usda")
}

fn renderer(scene: Scene) -> Renderer {
    Renderer::new(scene.camera, scene.world, scene.lights, scene.settings)
}

fn film_bits(film: &AovFilm, beauty: &Buffer, vars: &[AovVar]) -> Vec<Vec<u32>> {
    vars.iter()
        .flat_map(|v| film.var_channels(beauty, v))
        .map(|plane| plane.into_iter().map(f32::to_bits).collect())
        .collect()
}

fn beauty_bits(b: &Buffer, w: usize, h: usize) -> Vec<[u32; 3]> {
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let c = b.get_pixel(x, y);
            [c.x.to_bits(), c.y.to_bits(), c.z.to_bits()]
        })
        .collect()
}

/// The sample's product resolves as authored: the beauty with alpha, `P`
/// and a two-channel closest `motionvector` named `forward`, blur off.
#[test]
fn the_sample_scene_asks_for_a_forward_var_with_blur_off() {
    let scene = sample_scene();
    assert!(!scene.settings.motion_blur());
    assert!(scene.world.has_motion());
    let vars: Vec<_> = scene.aovs.products[0]
        .vars
        .iter()
        .map(|v| (v.name.as_str(), v.source, v.components))
        .collect();
    assert_eq!(
        vars,
        [
            ("C", AovSource::Color, 4),
            ("P", AovSource::P, 3),
            ("forward", AovSource::MotionVector, 2),
        ]
    );
    let forward = &scene.aovs.products[0].vars[2];
    assert_eq!(forward.accumulation, crust_core::Accumulation::Closest);
    assert_eq!(forward.precision, Precision::Float);
    assert_eq!(forward.clear, 0.0);
}

/// With a `motionvector` var present the film's guarantees hold: every
/// channel is bit-identical between tiles and scanlines, and the beauty is
/// bit-identical to the render without any AOV.
#[test]
fn tiles_scanlines_and_the_beauty_are_unchanged_by_a_motion_vector_var() {
    let scene = sample_scene();
    let (w, h) = scene.settings.get_dimensions();
    let request = scene.aovs.clone();
    let vars: Vec<AovVar> = request.products[0].vars.clone();
    let r = renderer(scene);
    let (tiled, tiled_film, _) = r.render_with_aovs(true, &|_, _| {}, &request);
    let (rows, rows_film, _) = r.render_with_aovs(false, &|_, _| {}, &request);
    assert_eq!(
        film_bits(&tiled_film, &tiled, &vars),
        film_bits(&rows_film, &rows, &vars)
    );
    assert_eq!(beauty_bits(&tiled, w, h), beauty_bits(&rows, w, h));
    let plain = r.render_with_tiles();
    assert_eq!(beauty_bits(&plain, w, h), beauty_bits(&tiled, w, h));
    // And the vector plane is not empty: the moving sphere's pixels carry
    // one, the sky's do not.
    let forward = tiled_film.var_channels(&tiled, &vars[2]);
    assert!(forward[0].iter().any(|&u| u > 1.0), "no rightward motion");
    assert!(forward[1].iter().any(|&v| v > 1.0), "no upward motion");
    assert!(
        forward[0][..w].iter().all(|&u| u == 0.0),
        "the top row is sky"
    );
}

/// A `motionvector` var alone takes the AOV path and needs the film.
#[test]
fn a_motion_vector_var_alone_needs_the_film() {
    let request = AovRequest {
        products: vec![AovProduct {
            prim_path: "/p".into(),
            name: "p.exr".into(),
            vars: vec![AovVar {
                prim_path: "/v".into(),
                name: "forward".into(),
                channel_prefix: None,
                source: AovSource::MotionVector,
                components: 2,
                precision: Precision::Half,
                accumulation: AovSource::MotionVector.default_accumulation(),
                clear: 0.0,
                expression: None,
                raw: false,
            }],
            attributes: Vec::new(),
        }],
    };
    assert!(request.needs_film());
}

/// A forwarding instance reports its inner hits under another geometry's
/// id (`InstanceHitId::As` / `Offset`), so a hit carrying that id may lie
/// in the forwarding placement rather than on the geometry that owns it. No
/// translation is the answer for such an id: the owner's record goes,
/// whichever is attached first, and the id is counted unresolved.
#[test]
fn an_id_a_forwarding_instance_shares_gets_no_motion_record() {
    let moving = || {
        instance(
            ball(),
            Affine3A::IDENTITY,
            Some(Affine3A::from_translation(Vec3::X)),
        )
    };
    let placed = |x: f32| {
        instance(
            ball(),
            Affine3A::from_translation(Vec3::new(x, 0.0, 0.0)),
            None,
        )
    };

    // The moving geometry first, then an instance forwarding to its id.
    let mut b = WorldBuilder::new();
    let owner = b.attach(moving(), grey());
    let group = b.attach_labelled(placed(5.0), grey(), MASK_ALL, InstanceHitId::As(owner));
    assert_eq!(b.unresolved_motion(), 1);
    let world = b.commit();
    assert_eq!(world.motion(owner), Vec3A::ZERO);
    assert_eq!(world.motion(group), Vec3A::ZERO);
    // A hit through the forwarding placement carries the owner's id...
    let hit = world
        .intersect(&Ray::new(Vec3A::new(5.0, 0.0, 5.0), -Vec3A::Z), 1e-3, 100.0)
        .expect("the placed ball");
    assert_eq!(hit.geom_id, owner);
    // ...and reads zero, not the owner's translation.
    assert_eq!(world.motion(hit.geom_id), Vec3A::ZERO);

    // The forwarding instance first: the range is remembered, and a moving
    // geometry attached inside it later gets no record either.
    let mut b = WorldBuilder::new();
    let _group = b.attach_labelled(placed(5.0), grey(), MASK_ALL, InstanceHitId::As(1));
    let owner = b.attach(moving(), grey());
    assert_eq!(owner, 1);
    assert_eq!(b.unresolved_motion(), 1);
    assert_eq!(b.commit().motion(owner), Vec3A::ZERO);

    // `Offset(base)` covers `base ..= base + inner.max_hit_id()`: a ball
    // scene reports id 0, so an offset of 3 forwards id 3 and nothing else.
    let mut b = WorldBuilder::new();
    let _group = b.attach_labelled(placed(5.0), grey(), MASK_ALL, InstanceHitId::Offset(3));
    let outside = b.attach(moving(), grey()); // id 1
    let _static = b.attach(quad(), grey()); // id 2
    let inside = b.attach(moving(), grey()); // id 3
    assert_eq!((outside, inside), (1, 3));
    let world = b.commit();
    assert_eq!(world.motion(outside), Vec3A::X);
    assert_eq!(world.motion(inside), Vec3A::ZERO);
}

/// The unresolved set is the slots' current state: a reserved slot given a
/// rotating instance and then a static or translating geometry is not
/// unresolved any more, and the commit stays silent.
#[test]
fn replacing_an_unresolved_geometry_clears_its_warning() {
    let mut b = WorldBuilder::new();
    let a = b.reserve_slot(grey(), MASK_ALL);
    let c = b.reserve_slot(grey(), MASK_ALL);
    let rotating = || {
        instance(
            ball(),
            Affine3A::IDENTITY,
            Some(Affine3A::from_rotation_y(0.5)),
        )
    };
    b.set_geometry(a, rotating());
    b.set_geometry(c, rotating());
    assert_eq!(b.unresolved_motion(), 2);
    // Given again unresolved: still one id, not two assignments.
    b.set_geometry(a, rotating());
    assert_eq!(b.unresolved_motion(), 2);
    b.set_geometry(a, quad());
    b.set_geometry(
        c,
        instance(
            ball(),
            Affine3A::IDENTITY,
            Some(Affine3A::from_translation(Vec3::Z)),
        ),
    );
    assert_eq!(b.unresolved_motion(), 0);
    let warnings = Arc::new(AtomicUsize::new(0));
    let world = tracing::subscriber::with_default(CountWarnings(warnings.clone()), || b.commit());
    assert_eq!(warnings.load(Ordering::Relaxed), 0);
    assert_eq!(world.motion(a), Vec3A::ZERO);
    assert_eq!(world.motion(c), Vec3A::Z);
}
