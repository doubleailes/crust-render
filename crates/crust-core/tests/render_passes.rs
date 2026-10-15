//! Render passes at import: the pass `UsdImportOptions::render_pass` names,
//! its layers in `apiSchemas` order with `default` last, the layer each prim's
//! geometry is owned by (`World::layer`), and the coded warnings for what a
//! pass authors and crust does not read.

use crust_core::{Error, NoAssets, Scene, UsdImportOptions, WarningCode};
use std::path::{Path, PathBuf};

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

fn write_stage(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_render_pass_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, text).expect("write stage");
    path
}

fn import(path: &Path, pass: Option<&str>) -> Result<Scene, Error> {
    let options = UsdImportOptions {
        render_pass: pass.map(str::to_owned),
        ..UsdImportOptions::default()
    };
    Scene::from_usd_with_options(path, &NoAssets, &options)
}

/// `world` under the root, and a `/Render` scope with a settings prim and
/// the pass `/Render/Passes/p` authoring `pass` (its metadata, then its
/// body), imported for that pass.
fn load(name: &str, world: &str, pass_meta: &str, pass_body: &str) -> Scene {
    let text = format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
)

def Xform "World"
{{
    def Camera "cam"
    {{
    }}
{world}
}}

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        int2 resolution = (16, 16)
    }}

    def Scope "Passes"
    {{
        def RenderPass "p" (
            {pass_meta}
        )
        {{
{pass_body}
        }}
    }}
}}
"#
    );
    let path = write_stage(name, &text);
    import(&path, Some("/Render/Passes/p")).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// The layer names of every geometry the prim at `path` emitted, one per
/// `geom_id`.
fn layers_of(scene: &Scene, path: &str) -> Vec<String> {
    let pass = scene.render_pass.as_ref().expect("a render pass");
    let ids = scene
        .provenance
        .as_ref()
        .expect("an imported scene")
        .geometry_of(path);
    assert!(!ids.is_empty(), "{path} emitted no geometry");
    ids.into_iter()
        .flatten()
        .map(|id| pass.layers[scene.world.layer(id) as usize].clone())
        .collect()
}

fn the_layer_of(scene: &Scene, path: &str) -> String {
    let layers = layers_of(scene, path);
    assert!(
        layers.windows(2).all(|w| w[0] == w[1]),
        "{path}: {layers:?}"
    );
    layers[0].clone()
}

fn codes(scene: &Scene) -> Vec<WarningCode> {
    scene.warnings.iter().map(|w| w.code).collect()
}

const MESH: &str = r#"
            uniform token subdivisionScheme = "none"
            int[] faceVertexCounts = [3]
            int[] faceVertexIndices = [0, 1, 2]
            point3f[] points = [(0, 0, -2), (1, 0, -2), (0, 1, -2)]
"#;

fn mesh(name: &str) -> String {
    format!("def Mesh \"{name}\"\n{{{MESH}}}\n")
}

#[test]
fn the_sample_pass_has_its_layers_in_order() {
    let scene = import(&sample("render_passes.usda"), Some("/Render/Passes/render")).unwrap();
    let pass = scene.render_pass.as_ref().unwrap();
    assert_eq!(pass.path, "/Render/Passes/render");
    assert_eq!(pass.name, "render");
    assert_eq!(pass.render_source.as_deref(), Some("/Render/settings"));
    assert_eq!(pass.layers, ["set", "hero", "default"]);
    assert_eq!(pass.file_pattern, "renders/$l_$n_$o.$04f.$x");
    assert_eq!(pass.layer_pattern, "$n_$o");
    for (prim, layer) in [
        ("/World/Set/Floor", "set"),
        ("/World/Set/Wall", "set"),
        ("/World/Set/Leaf", "set"),
        ("/World/Set/Glass", "set"),
        // Membership is inherited: the collection names /World/Hero.
        ("/World/Hero/Body/Mesh", "hero"),
        // No layer claims the key light's own geometry.
        ("/World/Key", "default"),
    ] {
        assert_eq!(the_layer_of(&scene, prim), layer, "{prim}");
    }
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
}

#[test]
fn a_pass_with_no_layers_has_only_default() {
    let scene = import(&sample("render_passes.usda"), Some("/Render/Passes/fx")).unwrap();
    let pass = scene.render_pass.as_ref().unwrap();
    assert_eq!(pass.layers, ["default"]);
    assert_eq!(pass.render_source.as_deref(), Some("/Render/fx_settings"));
    assert_eq!(pass.file_pattern, "$l_$n_$o.$04f.$x");
    assert_eq!(scene.settings.get_dimensions(), (64, 36));
    for id in 0..scene.world.count() as u32 {
        assert_eq!(scene.world.layer(id), 0);
    }
}

#[test]
fn without_a_pass_nothing_is_layered() {
    let scene = import(&sample("render_passes.usda"), None).unwrap();
    assert!(scene.render_pass.is_none());
    assert_eq!(scene.settings.get_dimensions(), (160, 90));
    for id in 0..scene.world.count() as u32 {
        assert_eq!(scene.world.layer(id), 0);
    }
}

#[test]
fn a_pass_that_is_not_one_is_refused() {
    let stage = sample("render_passes.usda");
    assert!(matches!(
        import(&stage, Some("/World/Cam")),
        Err(Error::NotARenderPass(p)) if p == "/World/Cam"
    ));
    assert!(matches!(
        import(&stage, Some("/Render/Passes/missing")),
        Err(Error::NotARenderPass(_))
    ));
    assert!(matches!(
        import(&stage, Some("Render/Passes/render")),
        Err(Error::InvalidPassPath(_))
    ));
}

#[test]
fn overlapping_layers() {
    let scene = load(
        "overlap",
        &format!(
            "def Xform \"Set\"\n{{\n{}}}\ndef Xform \"Hero\"\n{{\n{}}}\n",
            mesh("Floor"),
            mesh("Body")
        ),
        r#"prepend apiSchemas = ["CollectionAPI:layer:set", "CollectionAPI:layer:hero"]"#,
        r#"
            rel collection:layer:set:includes = </World>
            rel collection:layer:hero:includes = </World/Hero>
"#,
    );
    // The earlier layer wins.
    assert_eq!(the_layer_of(&scene, "/World/Hero/Body"), "set");
    assert_eq!(the_layer_of(&scene, "/World/Set/Floor"), "set");
    let overlap = scene
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::PassLayerOverlap)
        .unwrap_or_else(|| panic!("{:#?}", scene.warnings));
    assert_eq!(overlap.prims, ["/World/Hero/Body"]);
    for word in ["/World/Hero", "set", "hero"] {
        assert!(overlap.message.contains(word), "{}", overlap.message);
    }
}

#[test]
fn layers_follow_api_schemas_order_and_default_is_last() {
    let scene = load(
        "order",
        &format!("{}{}{}", mesh("A"), mesh("B"), mesh("C")),
        r#"prepend apiSchemas = ["CollectionAPI:layer:zeta", "CollectionAPI:layer:alpha"]"#,
        r#"
            rel collection:layer:zeta:includes = </World/B>
            rel collection:layer:alpha:includes = </World/A>
"#,
    );
    let pass = scene.render_pass.as_ref().unwrap();
    assert_eq!(pass.layers, ["zeta", "alpha", "default"]);
    assert_eq!(the_layer_of(&scene, "/World/A"), "alpha");
    assert_eq!(the_layer_of(&scene, "/World/B"), "zeta");
    assert_eq!(the_layer_of(&scene, "/World/C"), "default");
}

#[test]
fn a_layer_that_authors_nothing_claims_nothing() {
    let scene = load(
        "empty_layer",
        &mesh("A"),
        r#"prepend apiSchemas = ["CollectionAPI:layer:empty"]"#,
        "",
    );
    assert_eq!(
        scene.render_pass.as_ref().unwrap().layers,
        ["empty", "default"]
    );
    assert_eq!(the_layer_of(&scene, "/World/A"), "default");
}

#[test]
fn two_instances_of_one_prototype_in_two_layers() {
    let scene = load(
        "instances",
        &format!(
            r#"
    class Xform "Proto"
    {{
{}
    }}
    def Xform "Left" (
        instanceable = true
        prepend references = </World/Proto>
    )
    {{
    }}
    def Xform "Right" (
        instanceable = true
        prepend references = </World/Proto>
    )
    {{
    }}
"#,
            mesh("Leaf")
        ),
        r#"prepend apiSchemas = ["CollectionAPI:layer:left", "CollectionAPI:layer:right"]"#,
        r#"
            rel collection:layer:left:includes = </World/Left>
            rel collection:layer:right:includes = </World/Right>
"#,
    );
    // A native instance emits ids of its own.
    assert_eq!(the_layer_of(&scene, "/World/Left"), "left");
    assert_eq!(the_layer_of(&scene, "/World/Right"), "right");
    assert!(
        !codes(&scene).contains(&WarningCode::PassLayerTargetInInstance),
        "{:#?}",
        scene.warnings
    );
}

#[test]
fn a_target_inside_an_instance_is_judged_on_the_instance() {
    let scene = load(
        "inside_instance",
        &format!(
            r#"
    class Xform "Proto"
    {{
{}
    }}
    def Xform "Left" (
        instanceable = true
        prepend references = </World/Proto>
    )
    {{
    }}
"#,
            mesh("Leaf")
        ),
        r#"prepend apiSchemas = ["CollectionAPI:layer:leaf"]"#,
        r#"
            rel collection:layer:leaf:includes = </World/Left/Leaf>
"#,
    );
    assert!(
        codes(&scene).contains(&WarningCode::PassLayerTargetInInstance),
        "{:#?}",
        scene.warnings
    );
}

#[test]
fn a_camera_visibility_collection() {
    let scene = load(
        "camera_visibility",
        &mesh("A"),
        r#"prepend apiSchemas = ["CollectionAPI:cameraVisibility"]"#,
        r#"
            uniform bool collection:cameraVisibility:includeRoot = 0
"#,
    );
    let w = scene
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::PassCollectionNotRead)
        .unwrap_or_else(|| panic!("{:#?}", scene.warnings));
    assert!(w.message.contains("/Render/Passes/p"), "{}", w.message);
    assert!(w.message.contains("cameraVisibility"), "{}", w.message);
    // The render sees every object: the mesh is still there.
    assert_eq!(the_layer_of(&scene, "/World/A"), "default");
}

#[test]
fn a_visibility_collection_that_restricts_nothing_is_not_warned() {
    let scene = load("render_visibility_default", &mesh("A"), "", "");
    assert!(
        !codes(&scene).contains(&WarningCode::PassCollectionNotRead),
        "{:#?}",
        scene.warnings
    );
}

// --- One traversal, one film per layer (`render-passes` "Each sample belongs
// to one layer", "A layer receives only the samples it owns", "Layers do not
// change the light"). Rendered on the sample stage at 80×45 — its aspect —
// with a box filter of radius 0.5, so each pixel's samples land in it alone.

use crust_core::{
    Accumulation, AovFilm, AovProduct, AovRequest, AovSource, AovVar, Buffer, PixelFilter,
    Precision, RenderSettings, Renderer,
};

const W: usize = 80;
const H: usize = 45;

/// The sample stage imported for `pass`, with its renderer at 80×45.
fn renderer(pass: Option<&str>, settings: impl Fn(RenderSettings) -> RenderSettings) -> Renderer {
    let scene = import(&sample("render_passes.usda"), pass).unwrap();
    let base = scene
        .settings
        .with_resolution(W, H)
        .with_pixel_filter("box".parse::<PixelFilter>().unwrap());
    Renderer::new(scene.camera, scene.world, scene.lights, settings(base))
        .with_volumes(scene.volumes)
}

fn var(name: &str, source: AovSource, components: usize, layer: Option<u16>) -> AovVar {
    AovVar {
        prim_path: format!("/Render/Vars/{name}"),
        name: name.to_owned(),
        channel_prefix: None,
        source,
        components,
        precision: Precision::Float,
        accumulation: Accumulation::Filtered,
        clear: source.default_clear(),
        expression: None,
        raw: false,
        variance: false,
        layer,
    }
}

/// A `color4f` beauty and a depth per layer `0..layers`.
fn layered(layers: u16) -> AovRequest {
    let vars = (0..layers)
        .flat_map(|l| {
            [
                var("Beauty", AovSource::Color, 4, Some(l)),
                var("Depth", AovSource::Depth, 1, Some(l)),
            ]
        })
        .collect();
    AovRequest {
        products: vec![AovProduct {
            prim_path: "/Render/p".into(),
            name: "p.exr".into(),
            vars,
            attributes: Vec::new(),
        }],
    }
}

fn render(r: &Renderer, req: &AovRequest) -> (Buffer, AovFilm) {
    let (buffer, film, _) = r.render_with_aovs(true, &|_, _| {}, req);
    (buffer, film)
}

/// Layer `l`'s beauty channels (R, G, B, A), top-down.
fn beauty_of(film: &AovFilm, beauty: &Buffer, l: u16) -> Vec<Vec<f32>> {
    film.var_channels(beauty, &var("Beauty", AovSource::Color, 4, Some(l)))
}

fn depth_of(film: &AovFilm, beauty: &Buffer, l: u16) -> Vec<f32> {
    film.var_channels(beauty, &var("Depth", AovSource::Depth, 1, Some(l)))
        .remove(0)
}

fn px(x: usize, y: usize) -> usize {
    y * W + x
}

/// The beauty buffer's R, G, B planes, top-down.
fn planes(beauty: &Buffer) -> Vec<Vec<f32>> {
    (0..3)
        .map(|c| {
            (0..W * H)
                .map(|i| {
                    let (r, g, b) = beauty.get_rgb(i % W, i / W);
                    [r, g, b][c]
                })
                .collect()
        })
        .collect()
}

// Pixels of the 80×45 frame, top-down: the hero seen through the cut-out
// leaf (its left half, x 32–40), the hero behind the glass pane (its right
// half, x 41–48), the sky above the wall (rows 0–12), and the floor.
const THROUGH_LEAF: (usize, usize) = (36, 30);
const THROUGH_GLASS: (usize, usize) = (45, 30);
const SKY: (usize, usize) = (40, 2);
const FLOOR: (usize, usize) = (5, 42);
const SET: u16 = 0;
const HERO: u16 = 1;
const DEFAULT: u16 = 2;

#[test]
fn each_sample_belongs_to_the_layer_it_first_meets() {
    let r = renderer(Some("/Render/Passes/render"), |s| {
        s.with_samples_per_pixel(4)
    });
    let (beauty, film) = render(&r, &layered(3));
    let alpha = |l: u16, (x, y): (usize, usize)| beauty_of(&film, &beauty, l)[3][px(x, y)];
    // A cutout leaf in front of the hero: the samples pass the leaf, so the
    // hero owns them.
    assert_eq!(alpha(HERO, THROUGH_LEAF), 1.0);
    assert_eq!(alpha(SET, THROUGH_LEAF), 0.0);
    // Glass owns what is seen through it.
    assert_eq!(alpha(SET, THROUGH_GLASS), 1.0);
    assert_eq!(alpha(HERO, THROUGH_GLASS), 0.0);
    // Seeing the sky: `default` owns it, with no alpha (nothing was hit),
    // and the sky's colour.
    let sky = |l: u16| beauty_of(&film, &beauty, l)[0][px(SKY.0, SKY.1)];
    assert!(sky(DEFAULT) > 0.0);
    assert_eq!(sky(SET), 0.0);
    assert_eq!(sky(HERO), 0.0);
    assert_eq!(alpha(DEFAULT, SKY), 0.0);
}

#[test]
fn a_hero_behind_a_wall_is_held_out() {
    let r = renderer(Some("/Render/Passes/render"), |s| {
        s.with_samples_per_pixel(4)
    });
    let (beauty, film) = render(&r, &layered(3));
    let hero = beauty_of(&film, &beauty, HERO);
    let set = beauty_of(&film, &beauty, SET);
    let q = px(THROUGH_GLASS.0, THROUGH_GLASS.1);
    // The hero, behind the set's glass pane: zero colour, zero alpha.
    for (c, plane) in hero.iter().enumerate() {
        assert_eq!(plane[q], 0.0, "channel {c}");
    }
    // The set holds the pane, and what is seen through it.
    let whole = planes(&beauty);
    for c in 0..3 {
        assert_eq!(set[c][q].to_bits(), whole[c][q].to_bits(), "channel {c}");
    }
}

#[test]
fn depth_where_a_layer_owns_nothing() {
    let r = renderer(Some("/Render/Passes/render"), |s| {
        s.with_samples_per_pixel(4)
    });
    let (beauty, film) = render(&r, &layered(3));
    let q = px(FLOOR.0, FLOOR.1);
    assert!(depth_of(&film, &beauty, SET)[q].is_finite());
    assert_eq!(depth_of(&film, &beauty, HERO)[q], f32::INFINITY);
    assert_eq!(depth_of(&film, &beauty, DEFAULT)[q], f32::INFINITY);
    let q = px(THROUGH_LEAF.0, THROUGH_LEAF.1);
    assert!(depth_of(&film, &beauty, HERO)[q].is_finite());
    assert_eq!(depth_of(&film, &beauty, SET)[q], f32::INFINITY);
}

fn assert_sum(film: &AovFilm, beauty: &Buffer, layers: u16, what: &str) {
    let whole = planes(beauty);
    let parts: Vec<Vec<Vec<f32>>> = (0..layers).map(|l| beauty_of(film, beauty, l)).collect();
    for c in 0..3 {
        for q in 0..W * H {
            let sum: f64 = parts.iter().map(|p| p[c][q] as f64).sum();
            let b = whole[c][q] as f64;
            assert!(
                (sum - b).abs() <= 1e-4 * b.abs().max(1.0),
                "{what}: channel {c} pixel {q}: layers sum to {sum}, beauty {b}"
            );
        }
    }
    // Every sample is in exactly one layer: the alphas sum to the coverage.
    for q in 0..W * H {
        let a: f32 = parts.iter().map(|p| p[3][q]).sum();
        assert!((0.0..=1.0 + 1e-5).contains(&a), "{what}: alpha {a} at {q}");
    }
}

#[test]
fn the_layers_add_up_to_the_beauty() {
    let r = renderer(Some("/Render/Passes/render"), |s| {
        s.with_samples_per_pixel(16).with_indirect_clamp(0.0)
    });
    let (beauty, film) = render(&r, &layered(3));
    assert_sum(&film, &beauty, 3, "unguided");
}

#[test]
fn the_layers_add_up_to_the_beauty_when_guided() {
    let r = renderer(Some("/Render/Passes/render"), |s| {
        s.with_samples_per_pixel(16)
            .with_indirect_clamp(0.0)
            .with_guiding(true, 2, 0.5)
    });
    let (beauty, film) = render(&r, &layered(3));
    assert_sum(&film, &beauty, 3, "guided");
}

#[test]
fn layering_changes_no_beauty() {
    let settings = |s: RenderSettings| s.with_samples_per_pixel(4);
    let plain = renderer(None, settings).render_with_tiles();
    let r = renderer(Some("/Render/Passes/render"), settings);
    let (beauty, _) = render(&r, &layered(3));
    assert!(
        planes(&plain)
            .iter()
            .flatten()
            .zip(planes(&beauty).iter().flatten())
            .all(|(a, b)| a.to_bits() == b.to_bits())
    );
}

#[test]
fn one_layer_is_the_beauty() {
    for guided in [false, true] {
        let settings = |s: RenderSettings| {
            s.with_samples_per_pixel(4)
                .with_guiding(guided, 2, 0.5)
                .with_resolution(W, H)
        };
        // The fx pass declares no layer: `default` owns every sample.
        let r = renderer(Some("/Render/Passes/fx"), settings);
        let (beauty, film) = render(&r, &layered(1));
        let layer = beauty_of(&film, &beauty, 0);
        let whole = planes(&beauty);
        for c in 0..3 {
            assert!(
                layer[c]
                    .iter()
                    .map(|v| v.to_bits())
                    .eq(whole[c].iter().map(|v| v.to_bits())),
                "guided {guided}: channel {c}"
            );
        }
        // And the beauty is the render's without the pass, at its settings.
        // Unguided only: a guided render's training is not reproducible
        // from one render to the next, with or without layers.
        if guided {
            continue;
        }
        let plain = {
            let scene = import(&sample("render_passes.usda"), None).unwrap();
            let s = settings(
                scene
                    .settings
                    .with_pixel_filter("box".parse::<PixelFilter>().unwrap()),
            )
            .with_samples_per_pixel(4)
            .with_max_depth(r.settings.max_depth());
            Renderer::new(scene.camera, scene.world, scene.lights, s).render_with_tiles()
        };
        assert!(
            planes(&plain)
                .iter()
                .flatten()
                .zip(whole.iter().flatten())
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "guided {guided}"
        );
    }
}

#[test]
fn the_heros_shadow_on_the_set() {
    let r = renderer(Some("/Render/Passes/render"), |s| {
        s.with_samples_per_pixel(4)
    });
    let (beauty, film) = render(&r, &layered(3));
    let whole = planes(&beauty);
    let hero = beauty_of(&film, &beauty, HERO);
    let set = beauty_of(&film, &beauty, SET);
    // Every floor pixel the hero does not cover: the set holds all of it,
    // shadow included, and the hero none.
    for y in 36..H {
        for x in 0..W {
            let q = px(x, y);
            if hero[3][q] > 0.0 || set[3][q] < 1.0 {
                continue;
            }
            for c in 0..3 {
                assert_eq!(hero[c][q], 0.0);
                assert_eq!(set[c][q].to_bits(), whole[c][q].to_bits(), "({x}, {y})");
            }
        }
    }
}
