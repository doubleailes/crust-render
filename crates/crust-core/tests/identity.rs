//! The identity, primvar and ID-matte AOVs, end to end from a stage: what
//! each names at the camera's first hit, checked against the hit's own
//! position (the `P` AOV of the same, closest, sample); the beauty they never
//! change; ids that do not move between frames; and a matte whose coverage
//! sums to one and whose colour sums to the beauty.

use crust_core::identity::murmur3_32;
use crust_core::{
    AovFilm, AovRequest, AovVar, Buffer, NoAssets, PixelFilter, Renderer, Scene, UsdImportOptions,
    Vec3A,
};
use std::path::PathBuf;
use std::sync::OnceLock;

const W: usize = 48;
const H: usize = 36;

/// Left and Right meshes side by side at z = 0, a backdrop at z = -5 behind
/// everything (so no pixel sees past the scene), a ball inside a component
/// in front of them, and a PointInstancer of boxes whose middle instance is
/// hidden. Left moves between frames 1 and 2.
fn stage() -> String {
    r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
    startTimeCode = 1
    endTimeCode = 2
)

def Xform "World"
{
    def Camera "cam"
    {
        float focalLength = 35
        float horizontalAperture = 36
        double3 xformOp:translate = (0, 0, 10)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    def Mesh "Backdrop"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-40, -40, -5), (40, -40, -5), (40, 40, -5), (-40, 40, -5)]
    }

    def Mesh "Left" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4, 4]
        int[] faceVertexIndices = [0, 1, 2, 3, 3, 2, 4, 5]
        point3f[] points = [(-4, -3, 0), (-0.5, -3, 0), (-0.5, 0, 0), (-4, 0, 0), (-0.5, 3, 0), (-4, 3, 0)]
        color3f[] primvars:displayColor = [(1, 0, 0), (0, 1, 0)] (
            interpolation = "uniform"
        )
        rel material:binding = </World/Looks/Red>
        double3 xformOp:translate.timeSamples = {
            1: (0, 0, 0),
            2: (0.25, 0, 0),
        }
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    def Mesh "Right"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0.5, -3, 0), (4, -3, 0), (4, 3, 0), (0.5, 3, 0)]
        color3f[] primvars:displayColor = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)] (
            interpolation = "vertex"
        )
    }

    def Xform "Group" (
        kind = "component"
    )
    {
        color3f[] primvars:displayColor = [(0.2, 0.4, 0.6)]

        def Sphere "Ball"
        {
            double radius = 0.6
            double3 xformOp:translate = (0, 0, 2)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
    }

    def PointInstancer "Inst"
    {
        rel prototypes = [</World/Inst/Protos/Box>]
        int[] protoIndices = [0, 0, 0]
        point3f[] positions = [(-2, -1.5, 1), (0, -1.5, 1.5), (2, -1.5, 1)]
        int64[] invisibleIds = [1]

        def Scope "Protos"
        {
            def Mesh "Box"
            {
                uniform token subdivisionScheme = "none"
                int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]
                int[] faceVertexIndices = [0, 1, 3, 2, 2, 3, 7, 6, 6, 7, 5, 4, 4, 5, 1, 0, 1, 5, 7, 3, 4, 0, 2, 6]
                point3f[] points = [(-0.4, -0.4, 0.4), (0.4, -0.4, 0.4), (-0.4, 0.4, 0.4), (0.4, 0.4, 0.4), (-0.4, -0.4, -0.4), (0.4, -0.4, -0.4), (-0.4, 0.4, -0.4), (0.4, 0.4, -0.4)]
            }
        }
    }

    # A displayColor no color3f var can hold: refused, with a warning.
    def Mesh "Odd"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 20), (1, 0, 20), (0, 1, 20)]
        float[] primvars:displayColor = [0.5]
    }

    def Scope "Looks"
    {
        def Material "Red"
        {
            token outputs:surface.connect = </World/Looks/Red/S.outputs:surface>

            def Shader "S"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.8, 0.1, 0.1)
                token outputs:surface
            }
        }
    }
}

def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/cam>
        int2 resolution = (48, 36)
        rel products = [</Render/flat>, </Render/objects>]
        int crust:samplesPerPixel = 4
        int crust:maxDepth = 2
    }

    def RenderProduct "flat"
    {
        token productName = "flat.exr"
        rel orderedVars = [</Render/P>, </Render/prim>, </Render/mat>, </Render/asset>,
                           </Render/inst>, </Render/face>, </Render/dc>, </Render/dc_filtered>]
    }

    def RenderProduct "objects"
    {
        uniform token productType = "deepRaster"
        token productName = "objects.exr"
        rel orderedVars = [</Render/prim>, </Render/beauty>]
    }

    def RenderVar "P"
    {
        uniform token dataType = "point3f"
        uniform string sourceName = "P"
    }
    def RenderVar "prim"
    {
        uniform string sourceName = "primId"
    }
    def RenderVar "mat"
    {
        uniform string sourceName = "materialId"
    }
    def RenderVar "asset"
    {
        uniform string sourceName = "assetId"
    }
    def RenderVar "inst"
    {
        uniform string sourceName = "instanceId"
    }
    def RenderVar "face"
    {
        uniform string sourceName = "elementId"
    }
    def RenderVar "dc"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "displayColor"
        uniform token sourceType = "primvar"
        bool driver:parameters:aov:multiSampled = 0
    }
    def RenderVar "dc_filtered"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "displayColor"
        uniform token sourceType = "primvar"
    }
    def RenderVar "beauty"
    {
        uniform token dataType = "color4f"
        uniform string sourceName = "color"
    }
}
"#
    .to_owned()
}

/// The stage, written once per test process: the import may map the file,
/// so rewriting it under a test still reading it would fault.
fn path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("crust_identity_tests_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("identity.usda");
        std::fs::write(&path, stage()).expect("write stage");
        path
    })
    .clone()
}

fn load(frame: f64) -> Scene {
    let options = UsdImportOptions {
        frame: Some(frame),
        ..UsdImportOptions::default()
    };
    Scene::from_usd_with_options(&path(), &NoAssets, &options).expect("imports")
}

/// The scene's renderer, with a one-pixel box filter so a pixel's samples
/// all lie inside it: a filtered value away from an edge is then one
/// surface's.
fn renderer(scene: Scene) -> (Renderer, AovRequest) {
    let settings = scene
        .settings
        .with_pixel_filter(PixelFilter::BoxFilter { radius: 0.5 });
    let request = scene.aovs.clone();
    (
        Renderer::new(scene.camera, scene.world, scene.lights, settings),
        request,
    )
}

fn render(r: &Renderer, tiled: bool, request: &AovRequest) -> (Buffer, AovFilm) {
    let (b, f, _) = r.render_with_aovs(tiled, &|_, _| {}, request);
    (b, f)
}

fn var<'a>(request: &'a AovRequest, name: &str) -> &'a AovVar {
    request.products[0]
        .vars
        .iter()
        .find(|v| v.name == name)
        .unwrap_or_else(|| panic!("var {name}"))
}

const NONE: u32 = u32::MAX;

fn hash(path: &str) -> u32 {
    murmur3_32(path.as_bytes())
}

/// What a pixel's closest sample hit, read off its position.
#[derive(Debug, PartialEq)]
enum Hit {
    Backdrop,
    Left,
    Right,
    Ball,
    Box {
        x: f32,
    },
    /// On or near an edge between two of the above: not checked.
    Edge,
}

fn classify(p: Vec3A, left_shift: f32) -> Hit {
    let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
    if near(p.z, -5.0) {
        return Hit::Backdrop;
    }
    if (p - Vec3A::new(0.0, 0.0, 2.0)).length() < 0.61 {
        return Hit::Ball;
    }
    if (0.55..=1.95).contains(&p.z) && (-1.95..=-1.05).contains(&p.y) {
        return Hit::Box { x: p.x };
    }
    if near(p.z, 0.0) {
        // Away from the faces' shared edge (y = 0) and the meshes' borders.
        if p.y.abs() < 0.05 || p.y.abs() > 2.95 {
            return Hit::Edge;
        }
        if p.x < 0.0 && (-3.95 + left_shift..=-0.55 + left_shift).contains(&p.x) {
            return Hit::Left;
        }
        if (0.55..=3.95).contains(&p.x) {
            return Hit::Right;
        }
    }
    Hit::Edge
}

/// One value of a var at every pixel, top-down.
fn plane(film: &AovFilm, beauty: &Buffer, v: &AovVar) -> Vec<Vec<f32>> {
    film.var_channels(beauty, v)
}

#[test]
fn ids_and_primvars_name_what_the_first_hit_lies_on() {
    let scene = load(1.0);
    let codes: Vec<&str> = scene.warnings.iter().map(|w| w.code.as_str()).collect();
    assert!(codes.contains(&"aov.primvar_type_mismatch"), "{codes:?}");
    let (r, request) = renderer(scene);
    let (beauty, film) = render(&r, true, &request);
    let p = plane(&film, &beauty, var(&request, "P"));
    let bits = |name: &str| -> Vec<u32> {
        plane(&film, &beauty, var(&request, name))[0]
            .iter()
            .map(|x| x.to_bits())
            .collect()
    };
    let (prim, mat, asset, inst, face) = (
        bits("prim"),
        bits("mat"),
        bits("asset"),
        bits("inst"),
        bits("face"),
    );
    let dc = plane(&film, &beauty, var(&request, "dc"));
    let dcf = plane(&film, &beauty, var(&request, "dc_filtered"));
    let unbound = hash("crust:unbound");
    let mut seen = std::collections::BTreeMap::new();
    for i in 0..W * H {
        let at = Vec3A::new(p[0][i], p[1][i], p[2][i]);
        let colour = [dc[0][i], dc[1][i], dc[2][i]];
        let hit = classify(at, 0.0);
        let key = match &hit {
            Hit::Box { .. } => "box",
            other => match other {
                Hit::Backdrop => "backdrop",
                Hit::Left => "left",
                Hit::Right => "right",
                Hit::Ball => "ball",
                _ => "edge",
            },
        };
        *seen.entry(key).or_insert(0) += 1;
        let ids = (prim[i], mat[i], asset[i], inst[i], face[i]);
        match hit {
            Hit::Backdrop => {
                let b = hash("/World/Backdrop");
                assert_eq!(ids, (b, unbound, b, NONE, 0), "pixel {i}");
                assert_eq!(colour, [0.0; 3], "clear where not authored, pixel {i}");
            }
            Hit::Left => {
                let face_id = if at.y < 0.0 { 0 } else { 1 };
                let l = hash("/World/Left");
                assert_eq!(
                    ids,
                    (l, hash("/World/Looks/Red"), l, NONE, face_id),
                    "pixel {i}"
                );
                let want = if face_id == 0 {
                    [1.0, 0.0, 0.0]
                } else {
                    [0.0, 1.0, 0.0]
                };
                assert_eq!(colour, want, "uniform, pixel {i}");
            }
            Hit::Right => {
                let r = hash("/World/Right");
                assert_eq!(ids, (r, unbound, r, NONE, 0), "pixel {i}");
                // Vertex-interpolated over the quad: (s, t, 0).
                let (s, t) = ((at.x - 0.5) / 3.5, (at.y + 3.0) / 6.0);
                assert!(
                    (colour[0] - s).abs() < 1e-4
                        && (colour[1] - t).abs() < 1e-4
                        && colour[2] == 0.0,
                    "vertex, pixel {i}: {colour:?} at {at:?}"
                );
            }
            Hit::Ball => {
                assert_eq!(
                    ids,
                    (
                        hash("/World/Group/Ball"),
                        unbound,
                        hash("/World/Group"),
                        NONE,
                        NONE
                    ),
                    "pixel {i}"
                );
                // Inherited from the component, constant.
                let want = [0.2f32, 0.4, 0.6];
                assert!(
                    colour.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-6),
                    "inherited, pixel {i}: {colour:?}"
                );
            }
            Hit::Box { x } => {
                let instancer = hash("/World/Inst");
                assert_eq!((ids.0, ids.1, ids.2), (instancer, unbound, instancer));
                // The middle instance is hidden: the visible ones keep their
                // index in the arrays.
                assert_eq!(ids.3, if x < 0.0 { 0 } else { 2 }, "pixel {i}");
                assert!(ids.4 < 6, "a box face, pixel {i}: {}", ids.4);
            }
            Hit::Edge => {}
        }
    }
    // Inside one surface every sample sees the same value, so the filtered
    // var agrees with the closest one where the value is constant around the
    // pixel: its four neighbours' closest samples saw that value too (the
    // Right gradient aside).
    let closest = |i: usize| [dc[0][i], dc[1][i], dc[2][i]];
    let mut interior = 0;
    for y in 1..H - 1 {
        for x in 1..W - 1 {
            let i = y * W + x;
            let at = Vec3A::new(p[0][i], p[1][i], p[2][i]);
            if matches!(classify(at, 0.0), Hit::Right | Hit::Edge) {
                continue;
            }
            if [i - 1, i + 1, i - W, i + W]
                .iter()
                .any(|&j| closest(j) != closest(i) || prim[j] != prim[i])
            {
                continue;
            }
            interior += 1;
            let filtered = [dcf[0][i], dcf[1][i], dcf[2][i]];
            assert!(
                filtered
                    .iter()
                    .zip(closest(i))
                    .all(|(a, b)| (a - b).abs() < 1e-5),
                "pixel {i}: {filtered:?} vs {:?}",
                closest(i)
            );
        }
    }
    assert!(interior > 100, "{interior} interior pixels");
    for class in ["backdrop", "left", "right", "ball", "box"] {
        assert!(
            seen.get(class).copied().unwrap_or(0) >= 4,
            "too few {class} pixels: {seen:?}"
        );
    }
}

#[test]
fn identity_aovs_change_no_bit_of_the_beauty() {
    let (r, request) = renderer(load(1.0));
    let (beauty_only, _) = r.render_with_stats(true, &|_, _| {});
    let (beauty, _) = render(&r, true, &request);
    for y in 0..H {
        for x in 0..W {
            let (a, b) = (beauty_only.get_pixel(x, y), beauty.get_pixel(x, y));
            assert_eq!(
                a.to_array().map(f32::to_bits),
                b.to_array().map(f32::to_bits),
                "({x}, {y})"
            );
        }
    }
}

#[test]
fn tiles_and_scanlines_write_the_same_ids_primvars_and_matte() {
    let (r, request) = renderer(load(1.0));
    let (tb, tiled) = render(&r, true, &request);
    let (sb, scan) = render(&r, false, &request);
    for v in &request.products[0].vars {
        let a: Vec<Vec<u32>> = tiled
            .var_channels(&tb, v)
            .iter()
            .map(|c| c.iter().map(|x| x.to_bits()).collect())
            .collect();
        let b: Vec<Vec<u32>> = scan
            .var_channels(&sb, v)
            .iter()
            .map(|c| c.iter().map(|x| x.to_bits()).collect())
            .collect();
        assert_eq!(a, b, "{}", v.name);
    }
    let deep = &request.products[1];
    let (ta, sa) = (
        tiled.matte(deep).expect("a matte"),
        scan.matte(deep).expect("a matte"),
    );
    for y in 0..H {
        for x in 0..W {
            assert_eq!(ta.pixel(x, y), sa.pixel(x, y), "({x}, {y})");
        }
    }
}

/// The spec's "stable IDs across frames": Left moves, its id does not.
#[test]
fn a_prim_has_the_same_id_in_every_frame() {
    let ids_of_left = |frame: f64| -> std::collections::BTreeSet<u32> {
        let (r, request) = renderer(load(frame));
        let (beauty, film) = render(&r, true, &request);
        let p = plane(&film, &beauty, var(&request, "P"));
        let prim = &plane(&film, &beauty, var(&request, "prim"))[0];
        let shift = if frame == 2.0 { 0.25 } else { 0.0 };
        (0..W * H)
            .filter(|&i| classify(Vec3A::new(p[0][i], p[1][i], p[2][i]), shift) == Hit::Left)
            .map(|i| prim[i].to_bits())
            .collect()
    };
    let (one, two) = (ids_of_left(1.0), ids_of_left(2.0));
    assert_eq!(one, two);
    assert_eq!(one.into_iter().collect::<Vec<_>>(), [hash("/World/Left")]);
}

/// The spec's "coverage sums to one": the backdrop leaves no background, so
/// every pixel's names cover it whole — and their colours add up to the
/// beauty.
#[test]
fn a_matte_covers_every_pixel_and_splits_the_beauty() {
    let (r, request) = renderer(load(1.0));
    let (beauty, film) = render(&r, true, &request);
    let matte = film.matte(&request.products[1]).expect("a matte");
    assert_eq!(matte.dimensions(), (W, H));
    let names = [
        "/World/Backdrop",
        "/World/Left",
        "/World/Right",
        "/World/Group/Ball",
        "/World/Inst",
    ];
    let mut named = std::collections::BTreeSet::new();
    for y in 0..H {
        for x in 0..W {
            let samples = matte.pixel(x, y);
            assert!(!samples.is_empty(), "({x}, {y})");
            let coverage: f32 = samples.iter().map(|s| s.value[3]).sum();
            assert!((coverage - 1.0).abs() < 1e-5, "({x}, {y}): {coverage}");
            let mut colour = [0.0f32; 3];
            for s in samples {
                for (sum, v) in colour.iter_mut().zip(s.value) {
                    *sum += v;
                }
                let name = matte.name(s.name);
                assert!(names.contains(&name), "({x}, {y}): {name}");
                named.insert(name.to_owned());
            }
            let (r_, g, b) = beauty.get_rgb(x, y);
            for (got, want) in colour.iter().zip([r_, g, b]) {
                assert!(
                    (got - want).abs() <= 1e-5 * want.abs().max(1.0),
                    "({x}, {y}): {colour:?} vs {:?}",
                    (r_, g, b)
                );
            }
            // Front to back.
            assert!(samples.windows(2).all(|w| w[0].z <= w[1].z), "({x}, {y})");
        }
    }
    assert_eq!(named.len(), names.len(), "{named:?}");
}

/// A subdivided cube and a native instance of a prototype that binds its own
/// material; `products` decides what the stage asks for.
fn refined_stage(products: &str) -> String {
    format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

class Xform "Proto"
{{
    def Mesh "Card" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-0.5, -0.5, 0), (0.5, -0.5, 0), (0.5, 0.5, 0), (-0.5, 0.5, 0)]
        rel material:binding = </Proto/Looks/M>
    }}
    def Scope "Looks"
    {{
        def Material "M"
        {{
            token outputs:surface.connect = </Proto/Looks/M/S.outputs:surface>
            def Shader "S"
            {{
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.1, 0.6, 0.2)
                token outputs:surface
            }}
        }}
    }}
}}

def Xform "World"
{{
    def Camera "cam"
    {{
        float focalLength = 35
        float horizontalAperture = 36
        double3 xformOp:translate = (0, 0, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def Mesh "Cube"
    {{
        int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]
        int[] faceVertexIndices = [0, 1, 3, 2, 2, 3, 7, 6, 6, 7, 5, 4, 4, 5, 1, 0, 1, 5, 7, 3, 4, 0, 2, 6]
        point3f[] points = [(-1, -1, 1), (1, -1, 1), (-1, 1, 1), (1, 1, 1), (-1, -1, -1), (1, -1, -1), (-1, 1, -1), (1, 1, -1)]
        double3 xformOp:translate = (-1.2, 0, 0)
        double3 xformOp:rotateXYZ = (30, 40, 0)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateXYZ"]
    }}
    def "InstA" (
        instanceable = true
        references = </Proto>
    )
    {{
        double3 xformOp:translate = (1.2, 0.5, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def "InstB" (
        instanceable = true
        references = </Proto>
    )
    {{
        double3 xformOp:translate = (1.2, -0.7, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
}}

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/cam>
        int2 resolution = (40, 30)
        int crust:samplesPerPixel = 4
        int crust:maxDepth = 2
        int crust:subdivisionLevel = 2
        {products}
    }}
    def RenderProduct "flat"
    {{
        token productName = "flat.exr"
        rel orderedVars = [</Render/P>, </Render/face>]
    }}
    def RenderProduct "materials"
    {{
        uniform token productType = "deepRaster"
        token productName = "materials.exr"
        rel orderedVars = [</Render/mat>]
    }}
    def RenderVar "P"
    {{
        uniform token dataType = "point3f"
        uniform string sourceName = "P"
    }}
    def RenderVar "face"
    {{
        uniform string sourceName = "elementId"
    }}
    def RenderVar "mat"
    {{
        uniform string sourceName = "materialId"
    }}
}}
"#
    )
}

fn load_text(name: &str, text: &str) -> Scene {
    let dir = std::env::temp_dir().join(format!("crust_identity_tests_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, text).expect("write stage");
    Scene::from_usd(&path).expect("imports")
}

/// Asking for element maps has the refiner report the faces it tracks, and
/// nothing else: a subdivided surface renders the same bits with or without
/// `elementId`. Its refined triangles name their cage face.
#[test]
fn element_maps_change_no_geometry_and_name_cage_faces() {
    let asked = load_text(
        "refined_asked",
        &refined_stage("rel products = [</Render/flat>, </Render/materials>]"),
    );
    let plain = load_text("refined_plain", &refined_stage(""));
    // The cube is refined to level 2 (6 × 16 quads), not its cage's 12
    // triangles; the card is two.
    assert_eq!(asked.stats.scene.unique.triangles, 192 + 2);
    let (r, request) = renderer(asked);
    let (rp, _) = renderer(plain);
    let (with, film) = render(&r, true, &request);
    let (without, _) = rp.render_with_stats(true, &|_, _| {});
    for y in 0..30 {
        for x in 0..40 {
            assert_eq!(
                with.get_pixel(x, y).to_array().map(f32::to_bits),
                without.get_pixel(x, y).to_array().map(f32::to_bits),
                "({x}, {y})"
            );
        }
    }
    let p = film.var_channels(&with, var(&request, "P"));
    let face = &film.var_channels(&with, var(&request, "face"))[0];
    let mut faces = std::collections::BTreeSet::new();
    for i in 0..40 * 30 {
        let id = face[i].to_bits();
        if p[0][i] < -0.1 {
            // The refined cube: a cage face, never a refined one.
            assert!(id < 6, "pixel {i}: {id}");
            faces.insert(id);
        }
    }
    assert!(faces.len() >= 3, "{faces:?}");
    // A material inside a native prototype is named after each instance:
    // the same in every stage, whatever the prototype's number.
    let matte = film.matte(&request.products[1]).expect("a matte");
    let mut names = std::collections::BTreeSet::new();
    for y in 0..30 {
        for x in 0..40 {
            for s in matte.pixel(x, y) {
                names.insert(matte.name(s.name).to_owned());
            }
        }
    }
    assert_eq!(
        names.into_iter().collect::<Vec<_>>(),
        [
            "/World/InstA/Looks/M",
            "/World/InstB/Looks/M",
            "crust:unbound"
        ]
    );
}
