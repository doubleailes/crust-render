//! `chiang_hair_bsdf` on curves, end to end: a strand lit through itself, a
//! tuft still casting a shadow, and a glass curve still refracting into its
//! own tube. Small stages written on the fly, their materials referenced
//! from `samples/hair.mtlx`.

use crust_core::rt::{CurveSegment, Geometry};
use crust_core::{
    Emissive, HitRecord, LightList, Material, PathSampler, Ray, SamplingStrategy, Scene, Vec3A,
    Volumes, WorldBuilder, ray_color,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A stage of `body` under `/World`, with `Looks/<name>` defined for each
/// `samples/hair.mtlx` material in `materials`, written and loaded.
fn load(name: &str, materials: &[&str], body: &str) -> Scene {
    let mtlx = repo()
        .join("samples/hair.mtlx")
        .canonicalize()
        .expect("hair.mtlx");
    let looks: String = materials
        .iter()
        .map(|m| {
            format!(
                "        def Material \"{m}\" (\n            prepend references = @{}@</MaterialX/Materials/{m}>\n        )\n        {{\n        }}\n",
                mtlx.display()
            )
        })
        .collect();
    let text = format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
    def Scope "Looks"
    {{
{looks}    }}
{body}
}}
"#
    );
    let dir = std::env::temp_dir().join("crust_hair_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, text).expect("write stage");
    Scene::from_usd(&path).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Mean radiance along `ray`, over `n` paths of at most `depth` bounces.
fn radiance(scene: &mut Scene, ray: &Ray, depth: i32, n: i32) -> Vec3A {
    radiance_by(scene, ray, depth, n, SamplingStrategy::PowerMis)
}

/// [`radiance`] under one sampling strategy.
fn radiance_by(
    scene: &mut Scene,
    ray: &Ray,
    depth: i32,
    n: i32,
    strategy: SamplingStrategy,
) -> Vec3A {
    let volumes = Volumes::new(std::mem::take(&mut scene.volumes));
    let sum = (0..n).fold(Vec3A::ZERO, |sum, i| {
        sum + ray_color(
            ray,
            &scene.world,
            &scene.lights,
            &volumes,
            depth,
            strategy,
            PathSampler::new(0, 0, 0, i),
        )
    });
    sum / n as f32
}

/// One horizontal strand at the origin, bound to `material`, with a small
/// light straight behind it as the camera sees it, and nothing else.
fn backlit_strand(name: &str, material: &str) -> Scene {
    lit_strand(name, material, "(0, 0, -3)")
}

/// [`backlit_strand`] with the light at `light`.
fn lit_strand(name: &str, material: &str, light: &str) -> Scene {
    load(
        name,
        &[material],
        &format!(
            r#"
    def BasisCurves "Strand" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token type = "linear"
        int[] curveVertexCounts = [2]
        point3f[] points = [(-2, 0, 0), (2, 0, 0)]
        float[] widths = [0.1] (interpolation = "constant")
        rel material:binding = </World/Looks/{material}>
    }}
    def SphereLight "Back"
    {{
        float inputs:intensity = 50
        float inputs:radius = 0.2
        double3 xformOp:translate = {light}
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}"#
        ),
    )
}

/// The camera sees the strand's front with the light directly behind it.
/// A fibre forward-scatters that light toward the camera through its own
/// strand; the diffuse twin sees the light below its horizon and is dark.
/// Both ways of reaching the light pass out of the strand's own tube: the
/// shadow ray (light sampling alone) and the continuation ray (BSDF sampling
/// alone). Before either did, the far wall shadowed the fibre.
#[test]
fn a_backlit_strand_glows_where_a_diffuse_one_is_dark() {
    let ray = Ray::new(Vec3A::new(0.0, 0.0, 5.0), -Vec3A::Z);
    let mut hair = backlit_strand("backlit_hair", "mtlx_hair_clear");
    let mut diffuse = backlit_strand("backlit_diffuse", "mtlx_diffuse_twin");
    for strategy in [
        SamplingStrategy::LightOnly,
        SamplingStrategy::BsdfOnly,
        SamplingStrategy::PowerMis,
    ] {
        // Direct light only: the TT lobe toward the light, through the strand.
        let glow = radiance_by(&mut hair, &ray, 1, 1024, strategy);
        let dark = radiance_by(&mut diffuse, &ray, 1, 1024, strategy);
        assert!(glow.min_element() > 0.05, "{strategy:?}: hair {glow:?}");
        assert!(dark.max_element() < 1e-6, "{strategy:?}: diffuse {dark:?}");
    }
}

/// A tuft between a light and a floor still shadows it: only rays leaving a
/// fibre pass out of tubes, and the floor's shadow rays enter the tuft.
#[test]
fn a_tuft_still_shadows_the_floor() {
    let mut scene = load(
        "tuft_shadow",
        &["mtlx_hair_clear"],
        r#"
    def BasisCurves "Tuft" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        uniform token type = "linear"
        int[] curveVertexCounts = [2, 2, 2]
        point3f[] points = [(-2, 1, -0.3), (2, 1, -0.3), (-2, 1, 0), (2, 1, 0), (-2, 1, 0.3), (2, 1, 0.3)]
        float[] widths = [0.4] (interpolation = "constant")
        rel material:binding = </World/Looks/mtlx_hair_clear>
    }
    def Mesh "Floor"
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-10, 0, -10), (-10, 0, 10), (10, 0, 10), (10, 0, -10)]
    }
    def SphereLight "Top"
    {
        float inputs:intensity = 2000
        float inputs:radius = 0.05
        double3 xformOp:translate = (0, 3, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#,
    );
    // Straight down from between the floor and the tuft: under the tuft, and
    // far enough beside it that the light's segment clears its end.
    let under = Ray::new(Vec3A::new(0.0, 0.5, 0.0), -Vec3A::Y);
    let beside = Ray::new(Vec3A::new(6.0, 0.5, 0.0), -Vec3A::Y);
    let shadowed = radiance(&mut scene, &under, 1, 64);
    let lit = radiance(&mut scene, &beside, 1, 64);
    assert!(lit.min_element() > 0.01, "beside {lit:?}");
    assert!(shadowed.max_element() < 1e-6, "under {shadowed:?}");
}

fn fixture(name: &str) -> crust_core::materialx::Loaded {
    crust_core::materialx::load(
        &repo().join("samples/hair.mtlx"),
        Some(name),
        &crust_core::mtlx::Host::new(&|_, _| None),
    )
    .unwrap_or_else(|e| panic!("{name}: {e:?}"))
}

/// One strand along x through the origin, radius 0.1, inside a white
/// emitting sphere: the mean radiance of camera rays crossing it at `h` (in
/// radii) from its axis.
fn strand_furnace(material: Arc<dyn Material>, h: f32) -> Vec3A {
    let mut world = WorldBuilder::new();
    world.attach(
        Geometry::RoundCurves {
            segments: vec![CurveSegment {
                p0: Vec3A::new(-3.0, 0.0, 0.0),
                p1: Vec3A::new(3.0, 0.0, 0.0),
                r0: 0.1,
                r1: 0.1,
            }],
        },
        material,
    );
    world.attach(
        Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 50.0,
        },
        Arc::new(Emissive::new(Vec3A::ONE)),
    );
    let world = world.commit();
    let (lights, volumes) = (LightList::new(), Volumes::default());
    let ray = Ray::new(Vec3A::new(0.0, 0.1 * h, -5.0), Vec3A::Z);
    let n = 2048;
    let sum = (0..n).fold(Vec3A::ZERO, |sum, i| {
        sum + ray_color(
            &ray,
            &world,
            &lights,
            &volumes,
            24,
            SamplingStrategy::PowerMis,
            PathSampler::new(5, 11, 0, i),
        )
    });
    sum / n as f32
}

/// The white furnace on a strand: a clear fibre returns all it receives, at
/// every offset across the strand, and every other fixture fibre returns
/// less, never more.
#[test]
fn every_hair_fixture_is_bounded_in_a_white_furnace() {
    for h in [0.0, 0.5, -0.85] {
        let clear = strand_furnace(fixture("mtlx_hair_clear").material, h);
        assert!(
            (clear - Vec3A::ONE).abs().max_element() < 0.03,
            "clear fibre at h {h}: {clear}"
        );
        for name in [
            "mtlx_hair_bare",
            "mtlx_hair_roughness",
            "mtlx_hair_color",
            "mtlx_hair_melanin",
            "mtlx_hair_mix",
            "mtlx_hair_translucent_mix",
            "mtlx_hair_tilted_mix",
        ] {
            let mean = strand_furnace(fixture(name).material, h);
            assert!(
                mean.max_element() <= 1.02 && mean.min_element() > 0.0,
                "{name} at h {h}: {mean}"
            );
        }
        // A white mass leaf returns `color` — all of it — whichever way its
        // normal faces, its light passing out of the strand as a fibre's.
        for name in ["mtlx_mass_back", "mtlx_mass_front"] {
            let mean = strand_furnace(fixture(name).material, h);
            assert!(
                (mean - Vec3A::ONE).abs().max_element() < 0.03,
                "{name} at h {h}: {mean}"
            );
        }
    }
}

/// Absorption colours the strand in the furnace: warm for the bare fixture's
/// rising coefficient, and close to the authored colour's hue for the one
/// absorption came from.
#[test]
fn absorbing_fibres_are_coloured_in_the_furnace() {
    for name in ["mtlx_hair_bare", "mtlx_hair_color", "mtlx_hair_melanin"] {
        let c = strand_furnace(fixture(name).material, 0.3);
        assert!(c.x > c.y && c.y > c.z, "{name}: {c}");
    }
}

/// A fibre beside a diffuse in a `mix` keeps both leaves, at the mix's
/// weights.
#[test]
fn a_mixed_fibre_keeps_both_leaves() {
    let l = fixture("mtlx_hair_mix");
    let rec = HitRecord {
        p: Vec3A::ZERO,
        normal: Vec3A::Z,
        t: 1.0,
        front_face: true,
        part: crust_core::HitPart::None,
        uv: None,
        tangent: Vec3A::X,
        uv_width: 0.0,
    };
    let probe = l.material.probe(&Ray::new(Vec3A::Z, -Vec3A::Z), &rec);
    let leaves = probe.closure.leaves();
    let categories: Vec<_> = leaves.iter().map(|p| p.category).collect();
    assert_eq!(categories, ["chiang_hair_bsdf", "oren_nayar_diffuse_bsdf"]);
    assert_eq!(leaves[0].weight, Vec3A::splat(0.25));
    assert_eq!(leaves[1].weight, Vec3A::splat(0.75));
    assert!(probe.closure.passes_out_of_curves());
}

/// A fibre beside a transmitting leaf: only the fibre's light passes out of
/// the strand. Lit from straight behind, the translucent half's light meets
/// the tube's far wall — on its own it would be dark at one bounce — so the
/// mix shows the fibre's half of the clear fibre's glow, under either
/// sampling strategy, and not the full glow passing everything would give.
#[test]
fn only_the_fibres_light_passes_out_of_a_mixed_strand() {
    let ray = Ray::new(Vec3A::new(0.0, 0.0, 5.0), -Vec3A::Z);
    let mut hair = backlit_strand("mixed_hair_ref", "mtlx_hair_clear");
    let mut mixed = backlit_strand("mixed_hair", "mtlx_hair_translucent_mix");
    // Light sampling sees the same light samples through the same fibre:
    // the half is exact. Passing the translucent half's light too would add
    // its share of the backlight, 0.11 here (1.5% of the glow).
    let full = radiance_by(&mut hair, &ray, 1, 4096, SamplingStrategy::LightOnly);
    let half = radiance_by(&mut mixed, &ray, 1, 4096, SamplingStrategy::LightOnly);
    assert!(
        (half - full * 0.5).abs().max_element() < 1e-3 * full.x,
        "light sampling: mixed {half:?} vs clear fibre {full:?}"
    );
    // BSDF sampling picks a share per sample: the half within noise (0.01
    // here), well short of the translucent's 0.09 had it passed.
    let full = radiance_by(&mut hair, &ray, 1, 16384, SamplingStrategy::BsdfOnly);
    let half = radiance_by(&mut mixed, &ray, 1, 16384, SamplingStrategy::BsdfOnly);
    assert!(
        (half - full * 0.5).abs().max_element() < 0.04,
        "BSDF sampling: mixed {half:?} vs clear fibre {full:?}"
    );
}

/// A reflecting leaf about a normal of its own, tilted into the tube,
/// beside a fibre: the same rule as the translucent's. The light is below
/// the tube's horizon but within the tilted diffuse's hemisphere, so the
/// diffuse has light to send into the tube — alone, its shadow ray meets the
/// far wall and it stays dark; mixed with the fibre it must stay so, the
/// mix showing only the fibre's share, rather than its light passing out of
/// the strand with the fibre's.
#[test]
fn a_tilted_reflecting_leaf_beside_a_fibre_meets_its_tube() {
    let ray = Ray::new(Vec3A::new(0.0, 0.0, 5.0), -Vec3A::Z);
    // 60° above the strand's far side: below the tube's horizon at the hit,
    // 30° inside the tilted diffuse's hemisphere.
    let light = "(0, 2.6, -1.5)";
    let mut hair = lit_strand("tilted_hair_ref", "mtlx_hair_clear", light);
    let mut alone = lit_strand("tilted_alone", "mtlx_tilted_alone", light);
    let mut mixed = lit_strand("tilted_hair", "mtlx_hair_tilted_mix", light);
    // The diffuse has a response toward the light: what would show were its
    // light to pass out of the strand.
    let hit = alone.world.intersect(&ray, 1e-4, 1e4).expect("the strand");
    let toward = (Vec3A::new(0.0, 2.6, -1.5) - hit.rec.p).normalize();
    assert!(
        hit.rec.normal.dot(toward) < 0.0,
        "the light is below the tube's horizon"
    );
    let (f, _) = hit
        .mat
        .eval(&ray, &hit.rec, toward)
        .expect("a diffuse evaluates");
    assert!(
        f.max_element() > 0.1,
        "the tilted diffuse faces the light: {f:?}"
    );
    for (strategy, n) in [
        (SamplingStrategy::LightOnly, 4096),
        (SamplingStrategy::BsdfOnly, 16384),
    ] {
        let fibre = radiance_by(&mut hair, &ray, 1, n, strategy);
        let dark = radiance_by(&mut alone, &ray, 1, n, strategy);
        let mix = radiance_by(&mut mixed, &ray, 1, n, strategy);
        assert!(dark.max_element() < 1e-6, "{strategy:?}: alone {dark:?}");
        assert!(
            (mix - fibre * 0.5).abs().max_element() < 2e-3 + 0.05 * fibre.max_element(),
            "{strategy:?}: mixed {mix:?} vs the fibre's half of {fibre:?}"
        );
    }
}

/// A strand whose only leaf is a mass diffuse facing away from the camera,
/// toward the light behind the strand: lit as the mass's surface would be,
/// not shadowed by its own tube — under light sampling and BSDF sampling
/// alike. The same leaf facing the camera has the light below its horizon.
#[test]
fn a_mass_lit_strand_lit_from_behind_its_own_tube() {
    let ray = Ray::new(Vec3A::new(0.0, 0.0, 5.0), -Vec3A::Z);
    let mut back = backlit_strand("mass_back", "mtlx_mass_back");
    let mut front = backlit_strand("mass_front", "mtlx_mass_front");
    for strategy in [
        SamplingStrategy::LightOnly,
        SamplingStrategy::BsdfOnly,
        SamplingStrategy::PowerMis,
    ] {
        let lit = radiance_by(&mut back, &ray, 1, 1024, strategy);
        let dark = radiance_by(&mut front, &ray, 1, 1024, strategy);
        assert!(lit.min_element() > 0.05, "{strategy:?}: mass {lit:?}");
        assert!(dark.max_element() < 1e-6, "{strategy:?}: front {dark:?}");
    }
}
