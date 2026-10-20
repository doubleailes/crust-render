//! Primvar AOVs (`sourceType = "primvar"`), end to end: small stages written
//! on the fly, imported, rendered at 16 samples per pixel, and each channel
//! checked in numbers against the authored primvar — interpolation by
//! interpolation, refusal by refusal — with the beauty bit-identical to the
//! render without the primvar vars.

use crust_core::{
    AovFilm, AovRequest, AovVar, Buffer, Renderer, Scene, UsdImportOptions, WarningCode,
};
use std::path::PathBuf;

const W: usize = 48;
const H: usize = 36;

fn write_stage(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_primvar_aov_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, text).expect("write stage");
    path
}

fn load_at(name: &str, text: &str, frame: Option<f64>) -> Scene {
    let path = write_stage(name, text);
    let options = UsdImportOptions {
        frame,
        ..UsdImportOptions::default()
    };
    Scene::from_usd_with_options(&path, &crust_core::NoAssets, &options)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn load(name: &str, text: &str) -> Scene {
    load_at(name, text, None)
}

/// A stage: a camera at z = 10 looking down −Z, a dome light, `world` under
/// `/World`, and a product with `vars` (RenderVar bodies keyed by name) plus
/// a `P` var.
fn stage(world: &str, vars: &[(&str, &str)]) -> String {
    stage_with(world, vars, "")
}

/// [`stage`] with `settings` (attribute lines) on the RenderSettings prim.
fn stage_with(world: &str, vars: &[(&str, &str)], settings: &str) -> String {
    let targets: Vec<String> = std::iter::once("</Render/P>".to_owned())
        .chain(vars.iter().map(|(n, _)| format!("</Render/{n}>")))
        .collect();
    let defs: String = vars
        .iter()
        .map(|(n, body)| format!("    def RenderVar \"{n}\"\n    {{\n{body}\n    }}\n"))
        .collect();
    format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
    startTimeCode = 1
    endTimeCode = 5
)

def Xform "World"
{{
    def Camera "cam"
    {{
        double3 xformOp:translate = (0, 0, 10)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def DomeLight "dome"
    {{
        float inputs:intensity = 1
    }}
{world}
}}

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/cam>
        int2 resolution = ({W}, {H})
        rel products = [</Render/p>]
{settings}
    }}
    def RenderProduct "p"
    {{
        token productName = "p.exr"
        rel orderedVars = [{}]
    }}
    def RenderVar "P"
    {{
        uniform token dataType = "point3f"
        uniform string sourceName = "P"
    }}
{defs}
}}
"#,
        targets.join(", ")
    )
}

/// A primvar var's body.
fn primvar_var(name: &str, data_type: &str, extra: &str) -> String {
    format!(
        "        uniform token dataType = \"{data_type}\"\n        uniform string sourceName = \"{name}\"\n        uniform token sourceType = \"primvar\"\n{extra}"
    )
}

/// A unit-ish quad mesh spanning `x0..x1` × `-1..1` at depth `z`, with
/// `extra` (primvar lines) inside it.
fn quad(name: &str, x0: f32, x1: f32, z: f32, extra: &str) -> String {
    format!(
        r#"    def Mesh "{name}"
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [({x0}, -1, {z}), ({x1}, -1, {z}), ({x1}, 1, {z}), ({x0}, 1, {z})]
{extra}
    }}
"#
    )
}

struct Rendered {
    beauty: Buffer,
    film: AovFilm,
    vars: Vec<AovVar>,
}

impl Rendered {
    /// Var `name`'s channels, top-down.
    fn channels(&self, name: &str) -> Vec<Vec<f32>> {
        let var = self
            .vars
            .iter()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("no var {name}"));
        self.film.var_channels(&self.beauty, var)
    }

    /// Var `name` at every pixel, as one vector per pixel.
    fn pixels(&self, name: &str) -> Vec<Vec<f32>> {
        let ch = self.channels(name);
        (0..W * H)
            .map(|i| ch.iter().map(|c| c[i]).collect())
            .collect()
    }
}

/// A scene ready to render: its renderer at 16 spp and its request.
struct Setup {
    r: Renderer,
    request: AovRequest,
}

fn setup(scene: Scene) -> Setup {
    let request = scene.aovs.clone();
    let settings = scene
        .settings
        .with_samples_per_pixel(16)
        .with_resolution(W, H);
    Setup {
        r: Renderer::new(scene.camera, scene.world, scene.lights, settings),
        request,
    }
}

fn render(s: &Setup) -> Rendered {
    let vars = s.request.products[0].vars.clone();
    let (beauty, film, _) = s.r.render_with_aovs(true, &|_, _| {}, &s.request);
    Rendered { beauty, film, vars }
}

fn beauty_bits(b: &Buffer) -> Vec<[u32; 3]> {
    (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .map(|(x, y)| {
            let c = b.get_pixel(x, y);
            [c.x.to_bits(), c.y.to_bits(), c.z.to_bits()]
        })
        .collect()
}

/// The beauty with only the `P` var, for the bit-identity check.
fn beauty_without_primvars(s: &Setup) -> Vec<[u32; 3]> {
    let mut request: AovRequest = s.request.clone();
    request.products[0].vars.retain(|v| v.name == "P");
    let (beauty, _, _) = s.r.render_with_aovs(true, &|_, _| {}, &request);
    beauty_bits(&beauty)
}

fn assert_beauty_unchanged(s: &Setup, r: &Rendered) {
    assert_eq!(
        beauty_bits(&r.beauty),
        beauty_without_primvars(s),
        "a primvar var changed the beauty"
    );
}

fn close(a: &[f32], b: &[f32], eps: f32) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= eps)
}

/// Whether pixel `p`'s closest `P` is a hit (the clear value is 0 and the
/// quads sit off the origin plane only at z = 0, so test the y range).
fn hit(p: &[f32]) -> bool {
    p != [0.0, 0.0, 0.0]
}

const REST_POINTS: &str = "        point3f[] primvars:rest = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)] (\n            interpolation = \"vertex\"\n        )";

/// "Rest position": the mesh, translated by (0.5, 0.25, 0) at frame 5,
/// reads its frame-1 position — `P` minus the translation, on every hit
/// pixel. And "Prefix is optional": `rest` and `primvars:rest` are one
/// channel, bit for bit.
#[test]
fn the_rest_position_is_the_authored_frame_one_position() {
    let world = r#"    def Xform "move"
    {
        double3 xformOp:translate.timeSamples = {
            1: (0, 0, 0),
            5: (0.5, 0.25, 0),
        }
        uniform token[] xformOpOrder = ["xformOp:translate"]
"#
    .to_owned()
        + &quad("mesh", -1.0, 1.0, 0.0, REST_POINTS)
        + "    }\n";
    let text = stage(
        &world,
        &[
            ("Pref", &primvar_var("rest", "point3f", "")),
            ("rest2", &primvar_var("primvars:rest", "point3f", "")),
        ],
    );
    let scene = load_at("rest", &text, Some(5.0));
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    let s = setup(scene);
    let r = render(&s);
    let (p, pref) = (r.pixels("P"), r.pixels("Pref"));
    let mut hits = 0;
    for (p, pref) in p.iter().zip(&pref) {
        if !hit(p) {
            assert_eq!(pref, &[0.0, 0.0, 0.0], "background reads the clear value");
            continue;
        }
        hits += 1;
        let want = [p[0] - 0.5, p[1] - 0.25, p[2]];
        assert!(close(pref, &want, 1e-4), "Pref {pref:?} vs P {p:?}");
    }
    assert!(hits > W * H / 10, "the quad covers part of the frame");
    let bits = |c: Vec<Vec<f32>>| -> Vec<Vec<u32>> {
        c.into_iter()
            .map(|c| c.into_iter().map(f32::to_bits).collect())
            .collect()
    };
    assert_eq!(bits(r.channels("Pref")), bits(r.channels("rest2")));
    assert_beauty_unchanged(&s, &r);
}

/// "Clear value where not authored": a mesh without the primvar and the
/// background read the authored clear value.
#[test]
fn a_prim_without_the_primvar_reads_the_clear_value() {
    let world = quad(
        "with",
        -2.0,
        -0.1,
        0.0,
        &REST_POINTS.replace("-1, -1", "-2, -1"),
    ) + &quad("without", 0.1, 2.0, 0.0, "");
    let text = stage(
        &world,
        &[(
            "Pref",
            &primvar_var(
                "rest",
                "point3f",
                "        float driver:parameters:aov:clearValue = -7",
            ),
        )],
    );
    let scene = load("clear", &text);
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    let s = setup(scene);
    let r = render(&s);
    let (p, pref) = (r.pixels("P"), r.pixels("Pref"));
    let (mut with, mut without, mut background) = (0, 0, 0);
    for (p, pref) in p.iter().zip(&pref) {
        if !hit(p) {
            background += 1;
            assert_eq!(pref, &[-7.0, -7.0, -7.0]);
        } else if p[0] > 0.0 {
            without += 1;
            assert_eq!(pref, &[-7.0, -7.0, -7.0], "at {p:?}");
        } else {
            with += 1;
            assert!(pref[0] > -7.0, "at {p:?}");
        }
    }
    assert!(with > 0 && without > 0 && background > 0);
}

/// "Face-varying UV set": an indexed `faceVarying` `uv2` with a seam
/// between two faces — each side reads its own chart, interpolated across
/// its face.
#[test]
fn a_face_varying_uv_set_keeps_its_seam() {
    // Two faces sharing the edge x = 0. The left face's chart is
    // `(x + 1, y + 1) / 2`; the right face's is shifted by 10 in u, so the
    // shared edge is a seam.
    let world = r#"    def Mesh "mesh"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4, 4]
        int[] faceVertexIndices = [0, 1, 4, 3, 1, 2, 5, 4]
        point3f[] points = [(-1, -1, 0), (0, -1, 0), (1, -1, 0), (-1, 1, 0), (0, 1, 0), (1, 1, 0)]
        texCoord2f[] primvars:uv2 = [(0, 0), (0.5, 0), (0.5, 1), (0, 1), (10.5, 0), (11, 0), (11, 1), (10.5, 1)] (
            interpolation = "faceVarying"
        )
        int[] primvars:uv2:indices = [0, 1, 2, 3, 4, 5, 6, 7]
    }
"#;
    let text = stage(
        world,
        &[(
            "uv2",
            &primvar_var(
                "uv2",
                "texCoord2f",
                "        bool driver:parameters:aov:multiSampled = 0",
            ),
        )],
    );
    let scene = load("uv2", &text);
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    let s = setup(scene);
    let r = render(&s);
    let (p, uv) = (r.pixels("P"), r.pixels("uv2"));
    let (mut left, mut right) = (0, 0);
    for (p, uv) in p.iter().zip(&uv) {
        if !hit(p) {
            continue;
        }
        let want = if p[0] < 0.0 {
            left += 1;
            [(p[0] + 1.0) / 2.0, (p[1] + 1.0) / 2.0]
        } else {
            right += 1;
            [10.0 + (p[0] + 1.0) / 2.0, (p[1] + 1.0) / 2.0]
        };
        assert!(close(uv, &want, 1e-4), "uv2 {uv:?} at {p:?}, want {want:?}");
    }
    assert!(left > 0 && right > 0);
}

/// "Per-face colour": a `uniform` `displayColor` gives each face's pixels
/// that face's colour — filtered, so checked away from the faces' edges.
#[test]
fn a_uniform_colour_is_per_face() {
    let world = r#"    def Mesh "mesh"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4, 4]
        int[] faceVertexIndices = [0, 1, 4, 3, 1, 2, 5, 4]
        point3f[] points = [(-1, -1, 0), (0, -1, 0), (1, -1, 0), (-1, 1, 0), (0, 1, 0), (1, 1, 0)]
        color3f[] primvars:displayColor = [(1, 0, 0), (0, 0, 1)] (
            interpolation = "uniform"
        )
    }
"#;
    let text = stage(
        world,
        &[("Cd", &primvar_var("displayColor", "color3f", ""))],
    );
    let scene = load("uniform_colour", &text);
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    assert_eq!(
        scene.aovs.products[0].vars[1].accumulation,
        crust_core::Accumulation::Filtered
    );
    let s = setup(scene);
    let r = render(&s);
    let (p, cd) = (r.pixels("P"), r.pixels("Cd"));
    let (mut red, mut blue) = (0, 0);
    // Inside a face by more than a pixel (one pixel is about 0.09 units).
    let inside = |p: &[f32]| p[0].abs() > 0.2 && p[0].abs() < 0.8 && p[1].abs() < 0.8;
    for (p, cd) in p.iter().zip(&cd) {
        if !hit(p) || !inside(p) {
            continue;
        }
        if p[0] < 0.0 {
            red += 1;
            assert!(close(cd, &[1.0, 0.0, 0.0], 1e-5), "{cd:?} at {p:?}");
        } else {
            blue += 1;
            assert!(close(cd, &[0.0, 0.0, 1.0], 1e-5), "{cd:?} at {p:?}");
        }
    }
    assert!(red > 0 && blue > 0);
    assert_beauty_unchanged(&s, &r);
}

/// Two quads at different depths whose silhouettes cross pixels: a
/// constant `rest` and `displayColor` each, front and back.
fn overlapping() -> String {
    let front = "        point3f primvars:rest = (1, 1, 1)\n        color3f primvars:displayColor = (1, 0, 0)";
    let back = "        point3f primvars:rest = (5, 5, 5)\n        color3f primvars:displayColor = (0, 0, 1)";
    quad("front", -1.03, 0.37, 1.0, front) + &quad("back", -2.5, 2.5, -1.0, back)
}

/// "A rest position is never blended across an edge" and "A colour primvar
/// is filtered": over the same edge pixels, the `point3f` var holds one
/// mesh's value and the `color3f` var a blend.
#[test]
fn a_rest_position_is_closest_and_a_colour_filtered() {
    let text = stage(
        &overlapping(),
        &[
            ("Pref", &primvar_var("rest", "point3f", "")),
            ("Cd", &primvar_var("displayColor", "color3f", "")),
        ],
    );
    let scene = load("edges", &text);
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    let s = setup(scene);
    let r = render(&s);
    let (pref, cd) = (r.pixels("Pref"), r.pixels("Cd"));
    for v in &pref {
        assert!(
            [[0.0; 3], [1.0; 3], [5.0; 3]]
                .iter()
                .any(|w| v.as_slice() == w),
            "a blended rest position {v:?}"
        );
    }
    let blended = cd
        .iter()
        .filter(|c| c[0] > 0.01 && c[2] > 0.01 && c[0] < 0.99)
        .count();
    assert!(blended > 0, "the colour is filtered across the edge");
    assert_beauty_unchanged(&s, &r);
}

/// "Integer IDs": a `constant` `int` primvar reads exactly, closest, and
/// the var is written as UINT.
#[test]
fn an_integer_id_reads_exactly() {
    let world = quad("mesh", -1.0, 1.0, 0.0, "        int primvars:objectId = 7");
    let text = stage(&world, &[("objectId", &primvar_var("objectId", "int", ""))]);
    let scene = load("int_id", &text);
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    let var = &scene.aovs.products[0].vars[1];
    assert_eq!(var.precision, crust_core::Precision::Uint);
    assert_eq!(var.accumulation, crust_core::Accumulation::Closest);
    let s = setup(scene);
    let r = render(&s);
    let (p, id) = (r.pixels("P"), r.pixels("objectId"));
    let mut n = 0;
    for (p, id) in p.iter().zip(&id) {
        if hit(p) {
            n += 1;
            assert_eq!(id, &[7.0]);
        } else {
            assert_eq!(id, &[0.0]);
        }
    }
    assert!(n > 0);
}

/// "Mismatched component count": one warning naming the var, the prim and
/// both types; the mesh reads the clear value.
#[test]
fn a_mismatched_component_count_is_refused_on_that_prim() {
    let world = quad("mesh", -1.0, 1.0, 0.0, REST_POINTS);
    let text = stage(&world, &[("rest2d", &primvar_var("rest", "float2", ""))]);
    let scene = load("mismatch", &text);
    let w: Vec<_> = scene
        .warnings
        .iter()
        .filter(|w| w.code == WarningCode::AovPrimvarMismatch)
        .collect();
    assert_eq!(w.len(), 1, "{:#?}", scene.warnings);
    assert_eq!(w[0].count, 1);
    assert_eq!(w[0].prims, ["/World/mesh"]);
    for needle in ["/Render/rest2d", "/World/mesh", "point3f[]", "2 component"] {
        assert!(w[0].message.contains(needle), "{needle}: {}", w[0].message);
    }
    let s = setup(scene);
    let r = render(&s);
    assert!(r.pixels("rest2d").iter().all(|v| v == &[0.0, 0.0]));
}

/// A string primvar has no numeric value: refused the same way.
#[test]
fn a_string_primvar_is_refused() {
    let world = quad(
        "mesh",
        -1.0,
        1.0,
        0.0,
        "        string primvars:label = \"a\"",
    );
    let text = stage(&world, &[("label", &primvar_var("label", "float", ""))]);
    let scene = load("string", &text);
    assert!(
        scene
            .warnings
            .iter()
            .any(|w| w.code == WarningCode::AovPrimvarMismatch),
        "{:#?}",
        scene.warnings
    );
}

/// Tiles and scanlines agree bitwise on every primvar channel.
#[test]
fn tiles_and_scanlines_agree_on_primvar_channels() {
    let text = stage(
        &overlapping(),
        &[
            ("Pref", &primvar_var("rest", "point3f", "")),
            ("Cd", &primvar_var("displayColor", "color3f", "")),
        ],
    );
    let s = setup(load("tiles", &text));
    let vars = s.request.products[0].vars.clone();
    let (tb, tf, _) = s.r.render_with_aovs(true, &|_, _| {}, &s.request);
    let (sb, sf, _) = s.r.render_with_aovs(false, &|_, _| {}, &s.request);
    let bits = |f: &AovFilm, b: &Buffer| -> Vec<Vec<u32>> {
        vars.iter()
            .flat_map(|v| f.var_channels(b, v))
            .map(|c| c.into_iter().map(f32::to_bits).collect())
            .collect()
    };
    assert_eq!(bits(&tf, &tb), bits(&sf, &sb));
}

/// A two-mesh prototype (top and bottom halves of a quad), each mesh's
/// `rest` its own points: a placement of it is one *group* instance whose
/// hits forward their slot's id, so no transform can be read back from a
/// hit — the primvar comes through the kernel's own barycentrics.
const PROTO: &str = r#"    class Xform "Proto"
    {
        def Mesh "top"
        {
            uniform token subdivisionScheme = "none"
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [(-0.6, 0, 0), (0.6, 0, 0), (0.6, 0.9, 0), (-0.6, 0.9, 0)]
            point3f[] primvars:rest = [(-0.6, 0, 0), (0.6, 0, 0), (0.6, 0.9, 0), (-0.6, 0.9, 0)] (
                interpolation = "vertex"
            )
        }
        def Mesh "bottom"
        {
            uniform token subdivisionScheme = "none"
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [(-0.6, -0.9, 0), (0.6, -0.9, 0), (0.6, 0, 0), (-0.6, 0, 0)]
            point3f[] primvars:rest = [(-0.6, -0.9, 0), (0.6, -0.9, 0), (0.6, 0, 0), (-0.6, 0, 0)] (
                interpolation = "vertex"
            )
        }
    }
"#;

/// Native instances of [`PROTO`] — one translated, one mirrored — and a
/// PointInstancer placing it twice: every placement reads the prototype's
/// table, `Pref` the local position of the hit.
#[test]
fn every_placement_of_a_prototype_reads_its_table() {
    let world = format!(
        r#"{proto}
    def Xform "left" (
        instanceable = true
        references = </World/Proto>
    )
    {{
        double3 xformOp:translate = (-1.4, 0.6, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def Xform "mirrored" (
        instanceable = true
        references = </World/Proto>
    )
    {{
        double3 xformOp:translate = (1.4, 0.6, 0)
        float3 xformOp:scale = (-1, 1, 1)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:scale"]
    }}
    def PointInstancer "scatter"
    {{
        point3f[] positions = [(-1.4, -1.3, 0), (1.4, -1.3, 0)]
        int[] protoIndices = [0, 0]
        rel prototypes = [</World/scatter/Protos/Proto>]
        def Scope "Protos"
        {{
            def Xform "Proto" (
                references = </World/Proto>
            )
            {{
            }}
        }}
    }}
"#,
        proto = PROTO
    );
    let text = stage(&world, &[("Pref", &primvar_var("rest", "point3f", ""))]);
    let scene = load("placements", &text);
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    // Every placement reads the one table of each prototype mesh: two
    // quads of four 16-byte corners and two triangles each, counted once.
    assert_eq!(
        scene.stats.scene.primvar_tables_bytes,
        2 * (4 * 16 + 2 * 12)
    );
    let s = setup(scene);
    let r = render(&s);
    let (p, pref) = (r.pixels("P"), r.pixels("Pref"));
    let mut seen = [0usize; 4];
    for (p, pref) in p.iter().zip(&pref) {
        if !hit(p) {
            continue;
        }
        // The upper placements span y in [-0.3, 1.5], the lower [-2.2, -0.4].
        let (k, local) = match (p[0] < 0.0, p[1] > -0.35) {
            (true, true) => (0, [p[0] + 1.4, p[1] - 0.6, p[2]]),
            (false, true) => (1, [-(p[0] - 1.4), p[1] - 0.6, p[2]]),
            (true, false) => (2, [p[0] + 1.4, p[1] + 1.3, p[2]]),
            (false, false) => (3, [p[0] - 1.4, p[1] + 1.3, p[2]]),
        };
        seen[k] += 1;
        assert!(
            close(pref, &local, 1e-4),
            "placement {k}: Pref {pref:?}, want {local:?} (P {p:?})"
        );
    }
    assert!(
        seen.iter().all(|&n| n > 0),
        "every placement seen: {seen:?}"
    );
    assert_beauty_unchanged(&s, &r);
}

/// Two prims with the same points and topology but different `rest` keep
/// a table each: the mesh key folds the kept primvars in.
#[test]
fn meshes_sharing_points_keep_their_own_rest() {
    let mesh = |name: &str, x: f32, rest: f32| {
        format!(
            r#"    def Xform "{name}"
    {{
        double3 xformOp:translate = ({x}, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        def Mesh "m"
        {{
            uniform token subdivisionScheme = "none"
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [(-0.8, -1, 0), (0.8, -1, 0), (0.8, 1, 0), (-0.8, 1, 0)]
            point3f primvars:rest = ({rest}, {rest}, {rest})
        }}
    }}
"#
        )
    };
    let world = mesh("a", -1.0, 2.0) + &mesh("b", 1.0, 3.0);
    let text = stage(
        &world,
        &[(
            "Pref",
            &primvar_var(
                "rest",
                "point3f",
                "        bool driver:parameters:aov:multiSampled = 0",
            ),
        )],
    );
    let s = setup(load("shared_points", &text));
    let r = render(&s);
    let (p, pref) = (r.pixels("P"), r.pixels("Pref"));
    let (mut a, mut b) = (0, 0);
    for (p, pref) in p.iter().zip(&pref) {
        if !hit(p) || p[0].abs() < 0.3 {
            continue;
        }
        if p[0] < 0.0 {
            a += 1;
            assert_eq!(pref, &[2.0, 2.0, 2.0]);
        } else {
            b += 1;
            assert_eq!(pref, &[3.0, 3.0, 3.0]);
        }
    }
    assert!(a > 0 && b > 0);
}

/// `varying` reads as `vertex` on a mesh; an array-valued primvar
/// (`elementSize` 2) and one whose indices leave its values are refused on
/// their prim, one warning each.
#[test]
fn varying_reads_as_vertex_and_malformed_primvars_are_refused() {
    let varying = REST_POINTS.replace("\"vertex\"", "\"varying\"");
    let array = "        float[] primvars:rest = [0, 1, 2, 3, 4, 5, 6, 7] (\n            elementSize = 2\n            interpolation = \"vertex\"\n        )";
    let bad_index = REST_POINTS.to_owned() + "\n        int[] primvars:rest:indices = [0, 1, 2, 9]";
    let world = quad(
        "varying",
        -2.5,
        -1.0,
        0.0,
        &varying.replace("(-1, ", "(-2.5, ").replace("(1, ", "(-1, "),
    ) + &quad("array", -0.7, 0.7, 0.0, array)
        + &quad("bad_index", 1.0, 2.5, 0.0, &bad_index);
    let text = stage(&world, &[("Pref", &primvar_var("rest", "point3f", ""))]);
    let scene = load("malformed", &text);
    let w = scene
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::AovPrimvarMismatch)
        .unwrap_or_else(|| panic!("{:#?}", scene.warnings));
    let mut prims = w.prims.clone();
    prims.sort();
    assert_eq!(prims, ["/World/array", "/World/bad_index"]);
    let s = setup(scene);
    let r = render(&s);
    let (p, pref) = (r.pixels("P"), r.pixels("Pref"));
    for (p, pref) in p.iter().zip(&pref) {
        if hit(p) && p[0] < -1.05 {
            assert!(close(pref, p, 1e-4), "varying: {pref:?} at {p:?}");
        } else if hit(p) && p[0] > -0.65 {
            assert_eq!(pref, &[0.0, 0.0, 0.0], "refused: at {p:?}");
        }
    }
}

/// A 1.2-unit `catmullClark` cube turned to show three faces, its `rest`
/// its own points, bound to `material` (empty for none).
fn cube(material: &str) -> String {
    let binding = if material.is_empty() {
        String::new()
    } else {
        format!("        rel material:binding = <{material}>")
    };
    let pts = "[(-0.6, -0.6, 0.6), (0.6, -0.6, 0.6), (0.6, 0.6, 0.6), (-0.6, 0.6, 0.6), (-0.6, -0.6, -0.6), (0.6, -0.6, -0.6), (0.6, 0.6, -0.6), (-0.6, 0.6, -0.6)]";
    format!(
        r#"    def Mesh "cube" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token subdivisionScheme = "catmullClark"
        int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]
        int[] faceVertexIndices = [0, 1, 2, 3, 5, 4, 7, 6, 4, 0, 3, 7, 1, 5, 6, 2, 3, 2, 6, 7, 4, 5, 1, 0]
        point3f[] points = {pts}
        point3f[] primvars:rest = {pts} (
            interpolation = "vertex"
        )
        float3 xformOp:rotateXYZ = (30, 40, 0)
        uniform token[] xformOpOrder = ["xformOp:rotateXYZ"]
{binding}
    }}
"#
    )
}

/// The rotation [`cube`] is placed by, to carry `P` back into the frame
/// `rest` is authored in.
fn unrotate(p: &[f32]) -> [f32; 3] {
    let m = glam::Mat3::from_euler(
        glam::EulerRot::ZYX,
        0.0,
        40f32.to_radians(),
        30f32.to_radians(),
    );
    m.transpose()
        .mul_vec3(glam::Vec3::new(p[0], p[1], p[2]))
        .to_array()
}

/// "Rest position on a subdivided mesh": `rest` equal to the points is
/// refined like them, so it agrees with `P` (carried back into the mesh's
/// frame) on every hit pixel — at uniform level 3, and tessellated
/// adaptively per face.
#[test]
fn a_subdivided_rest_position_agrees_with_p() {
    for (what, settings) in [
        ("uniform level 3", "        int crust:subdivisionLevel = 3"),
        ("adaptive", "        float crust:subdivisionEdgeLength = 2"),
    ] {
        let text = stage_with(
            &cube(""),
            &[("Pref", &primvar_var("rest", "point3f", ""))],
            settings,
        );
        let scene = load(&format!("subdiv_rest_{}", what.len()), &text);
        assert!(scene.warnings.is_empty(), "{what}: {:#?}", scene.warnings);
        let s = setup(scene);
        let r = render(&s);
        let (p, pref) = (r.pixels("P"), r.pixels("Pref"));
        let mut hits = 0;
        let mut worst = 0f32;
        for (p, pref) in p.iter().zip(&pref) {
            if !hit(p) {
                continue;
            }
            hits += 1;
            let local = unrotate(p);
            for (a, b) in local.iter().zip(pref) {
                worst = worst.max((a - b).abs());
            }
        }
        assert!(hits > W * H / 20, "{what}: the cube is seen");
        assert!(worst < 1e-4, "{what}: rest is {worst} off P");
        assert_beauty_unchanged(&s, &r);
    }
}

/// "Displacement leaves the rest position": the same cube pushed 0.1 along
/// its normals reads the undisplaced rest position, 0.1 from `P`.
#[test]
fn displacement_leaves_the_rest_position() {
    let material = r#"    def Material "Puffed"
    {
        token outputs:surface.connect = </World/Puffed/Surface.outputs:surface>
        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            float inputs:displacement = 0.1
            token outputs:surface
        }
    }
"#;
    let text = stage_with(
        &(material.to_owned() + &cube("/World/Puffed")),
        &[("Pref", &primvar_var("rest", "point3f", ""))],
        "        int crust:subdivisionLevel = 3",
    );
    let scene = load("displaced_rest", &text);
    let s = setup(scene);
    let r = render(&s);
    let (p, pref) = (r.pixels("P"), r.pixels("Pref"));
    let mut hits = 0;
    for (p, pref) in p.iter().zip(&pref) {
        if !hit(p) {
            continue;
        }
        hits += 1;
        let local = glam::Vec3::from(unrotate(p));
        let d = local.distance(glam::Vec3::new(pref[0], pref[1], pref[2]));
        assert!((d - 0.1).abs() < 0.01, "rest is {d} from P, not 0.1");
    }
    assert!(hits > W * H / 20);
    assert_beauty_unchanged(&s, &r);
}

/// A face-varying primvar on a `loop` and on a `bilinear` mesh is refined
/// with the surface: an affine chart stays affine in the mesh's position.
#[test]
fn loop_and_bilinear_meshes_carry_a_face_varying_primvar() {
    // An octahedron (all triangles) for loop; the same as quads would need
    // another cage, so bilinear refines the cube. The chart is `(x, y)` of
    // each face-vertex's point, affine, so any refinement reproduces it.
    let octahedron = r#"    def Mesh "octa"
    {
        uniform token subdivisionScheme = "loop"
        int[] faceVertexCounts = [3, 3, 3, 3, 3, 3, 3, 3]
        int[] faceVertexIndices = [0, 2, 4, 2, 1, 4, 1, 3, 4, 3, 0, 4, 2, 0, 5, 1, 2, 5, 3, 1, 5, 0, 3, 5]
        point3f[] points = [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)]
        texCoord2f[] primvars:xy = [(1, 0), (0, 1), (0, 0), (0, 1), (-1, 0), (0, 0), (-1, 0), (0, -1), (0, 0), (0, -1), (1, 0), (0, 0), (0, 1), (1, 0), (0, 0), (-1, 0), (0, 1), (0, 0), (0, -1), (-1, 0), (0, 0), (1, 0), (0, -1), (0, 0)] (
            interpolation = "faceVarying"
        )
        uniform token faceVaryingLinearInterpolation = "all"
    }
"#;
    let cube_xy = cube("")
        .replace("\"catmullClark\"", "\"bilinear\"")
        .replace(
            "        point3f[] primvars:rest",
            "        texCoord2f[] primvars:xy = [(-0.6, -0.6), (0.6, -0.6), (0.6, 0.6), (-0.6, 0.6), (0.6, -0.6), (-0.6, -0.6), (-0.6, 0.6), (0.6, 0.6), (-0.6, -0.6), (-0.6, -0.6), (-0.6, 0.6), (-0.6, 0.6), (0.6, -0.6), (0.6, -0.6), (0.6, 0.6), (0.6, 0.6), (-0.6, 0.6), (0.6, 0.6), (0.6, 0.6), (-0.6, 0.6), (-0.6, -0.6), (0.6, -0.6), (0.6, -0.6), (-0.6, -0.6)] (\n            interpolation = \"faceVarying\"\n        )\n        point3f[] primvars:rest",
        );
    for (what, world, unrot) in [
        ("loop", octahedron.to_owned(), false),
        ("bilinear", cube_xy, true),
    ] {
        let text = stage_with(
            &world,
            &[(
                "xy",
                &primvar_var(
                    "xy",
                    "texCoord2f",
                    "        bool driver:parameters:aov:multiSampled = 0",
                ),
            )],
            "        int crust:subdivisionLevel = 2",
        );
        let scene = load(&format!("fvar_{what}"), &text);
        assert!(scene.warnings.is_empty(), "{what}: {:#?}", scene.warnings);
        let s = setup(scene);
        let r = render(&s);
        let (p, xy) = (r.pixels("P"), r.pixels("xy"));
        let mut hits = 0;
        for (p, xy) in p.iter().zip(&xy) {
            if !hit(p) {
                continue;
            }
            hits += 1;
            let local = if unrot {
                unrotate(p)
            } else {
                [p[0], p[1], p[2]]
            };
            // Loop moves the surface off the cage, so only the bilinear
            // cube's chart is exactly its position; the octahedron's stays
            // inside the chart's range.
            if unrot {
                assert!(close(xy, &local[..2], 1e-4), "{what}: {xy:?} at {local:?}");
            } else {
                assert!(xy.iter().all(|c| c.abs() <= 1.0 + 1e-5), "{what}: {xy:?}");
            }
        }
        assert!(hits > 0, "{what}: seen");
    }
}

/// "Curves primvar": a `uniform` `displayColor` on a `BasisCurves` prim
/// gives each strand's pixels its curve's colour, though the material reads
/// no primvar at all.
#[test]
fn a_curves_primvar_is_per_strand() {
    let world = r#"    def BasisCurves "hair"
    {
        uniform token type = "linear"
        int[] curveVertexCounts = [2, 2]
        point3f[] points = [(-0.8, -1.2, 0), (-0.8, 1.2, 0), (0.8, -1.2, 0), (0.8, 1.2, 0)]
        float[] widths = [0.3] (
            interpolation = "constant"
        )
        color3f[] primvars:displayColor = [(1, 0, 0), (0, 1, 0)] (
            interpolation = "uniform"
        )
    }
"#;
    let text = stage(
        world,
        &[(
            "Cd",
            &primvar_var(
                "displayColor",
                "color3f",
                "        bool driver:parameters:aov:multiSampled = 0",
            ),
        )],
    );
    let scene = load("curves", &text);
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    let s = setup(scene);
    let r = render(&s);
    let (p, cd) = (r.pixels("P"), r.pixels("Cd"));
    let (mut red, mut green) = (0, 0);
    for (p, cd) in p.iter().zip(&cd) {
        if !hit(p) {
            assert_eq!(cd, &[0.0, 0.0, 0.0]);
        } else if p[0] < 0.0 {
            red += 1;
            assert_eq!(cd, &[1.0, 0.0, 0.0], "at {p:?}");
        } else {
            green += 1;
            assert_eq!(cd, &[0.0, 1.0, 0.0], "at {p:?}");
        }
    }
    assert!(red > 0 && green > 0);
    assert_beauty_unchanged(&s, &r);
}

/// "Ancestor primvar is reported, not inherited": an `Xform` authoring
/// `constant int primvars:objectId = 3` above a mesh that authors none —
/// one warning, and the mesh reads the clear value. A `PointInstancer`'s
/// per-instance primvar is reported the same way.
#[test]
fn an_ancestor_or_instancer_primvar_is_reported_not_inherited() {
    let world = r#"    def Xform "group"
    {
        int primvars:objectId = 3
"#
    .to_owned()
        + &quad("a", -2.0, -0.2, 0.0, "")
        + &quad("b", 0.2, 2.0, 0.0, "")
        + "    }\n";
    let text = stage(&world, &[("objectId", &primvar_var("objectId", "int", ""))]);
    let scene = load("ancestor", &text);
    let w: Vec<_> = scene
        .warnings
        .iter()
        .filter(|w| w.code == WarningCode::AovPrimvarNotInherited)
        .collect();
    assert_eq!(w.len(), 1, "{:#?}", scene.warnings);
    assert_eq!(w[0].count, 1, "once per var, not per prim");
    assert_eq!(w[0].prims, ["/World/group"]);
    assert!(
        w[0].message.contains("/Render/objectId"),
        "{}",
        w[0].message
    );
    let s = setup(scene);
    let r = render(&s);
    assert!(r.pixels("objectId").iter().all(|v| v == &[0.0]));

    let world = format!(
        r#"{PROTO}
    def PointInstancer "scatter"
    {{
        point3f[] positions = [(-1, 0, 0), (1, 0, 0)]
        int[] protoIndices = [0, 0]
        int[] primvars:objectId = [1, 2] (
            interpolation = "vertex"
        )
        rel prototypes = [</World/scatter/Protos/Proto>]
        def Scope "Protos"
        {{
            def Xform "Proto" (
                references = </World/Proto>
            )
            {{
            }}
        }}
    }}
"#
    );
    let text = stage(&world, &[("objectId", &primvar_var("objectId", "int", ""))]);
    let scene = load("instancer_primvar", &text);
    let w: Vec<_> = scene
        .warnings
        .iter()
        .filter(|w| w.code == WarningCode::AovPrimvarNotInherited)
        .collect();
    assert_eq!(w.len(), 1, "{:#?}", scene.warnings);
    assert_eq!(w[0].prims, ["/World/scatter"]);
    assert!(w[0].message.contains("PointInstancer"), "{}", w[0].message);
}

/// "No primvar var" and "Primvar tables are reported": with `primvars:rest`
/// on the mesh and `primvars:displayColor` on a groom but only `depth` and
/// `normal` asked for, no table is built (0 bytes of either) and the beauty
/// is bit-identical to the render of the stage with neither primvar
/// authored; a `rest` and a `displayColor` var build and report them.
#[test]
fn primvar_tables_cost_nothing_unless_asked_for() {
    let groom = |extra: &str| {
        format!(
            r#"    def BasisCurves "hair"
    {{
        uniform token type = "linear"
        int[] curveVertexCounts = [2]
        point3f[] points = [(1.5, -1, 0.5), (1.5, 1, 0.5)]
        float[] widths = [0.2] (
            interpolation = "constant"
        )
{extra}
    }}
"#
        )
    };
    let cd = "        color3f[] primvars:displayColor = [(1, 0, 0)] (\n            interpolation = \"uniform\"\n        )";
    let authored = quad("mesh", -1.0, 1.0, 0.0, REST_POINTS) + &groom(cd);
    let bare = quad("mesh", -1.0, 1.0, 0.0, "") + &groom("");
    let depth_normal: &[(&str, &str)] = &[
        (
            "depth",
            "        uniform token dataType = \"float\"\n        uniform string sourceName = \"depth\"",
        ),
        (
            "N",
            "        uniform token dataType = \"normal3f\"\n        uniform string sourceName = \"normal\"",
        ),
    ];
    let scene = load("no_var_authored", &stage(&authored, depth_normal));
    assert_eq!(scene.stats.scene.primvar_tables_bytes, 0);
    assert_eq!(scene.stats.scene.strand_tables_bytes, 0);
    let a = render(&setup(scene));
    let b = render(&setup(load("no_var_bare", &stage(&bare, depth_normal))));
    assert_eq!(beauty_bits(&a.beauty), beauty_bits(&b.beauty));

    let scene = load(
        "var_authored",
        &stage(
            &authored,
            &[
                ("Pref", &primvar_var("rest", "point3f", "")),
                ("Cd", &primvar_var("displayColor", "color3f", "")),
            ],
        ),
    );
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    // Four corners of 16 bytes and one triangle-corner triple per triangle.
    assert_eq!(scene.stats.scene.primvar_tables_bytes, 4 * 16 + 2 * 12);
    assert!(scene.stats.scene.strand_tables_bytes > 0);
    assert!(scene.stats.report().contains("primvar tables"));
}

/// `samples/primvar_aovs.usda`, in numbers: on the cube, `Pref` is `P`
/// carried back through the cube's transform and `uv2` lies in its squares;
/// on the floor, which authors no primvar, every primvar layer holds 0.
#[test]
fn the_sample_scene_reads_its_primvars() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/primvar_aovs.usda");
    let scene = Scene::from_usd(&path).expect("samples/primvar_aovs.usda");
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    let (w, h) = scene.settings.get_dimensions();
    let request = scene.aovs.clone();
    let vars = request.products[0].vars.clone();
    let settings = scene.settings.with_samples_per_pixel(16);
    let r = Renderer::new(scene.camera, scene.world, scene.lights, settings);
    let (beauty, film, _) = r.render_with_aovs(true, &|_, _| {}, &request);
    let channel = |name: &str| -> Vec<Vec<f32>> {
        let v = vars.iter().find(|v| v.name == name).expect(name);
        let ch = film.var_channels(&beauty, v);
        (0..w * h)
            .map(|i| ch.iter().map(|c| c[i]).collect())
            .collect()
    };
    let (p, pref, uv2, cd) = (channel("P"), channel("Pref"), channel("uv2"), channel("Cd"));
    // The cube's transform: translate (0, 1, 0), rotateXYZ (20, 35, 0).
    let rot = glam::Mat3::from_euler(
        glam::EulerRot::ZYX,
        0.0,
        35f32.to_radians(),
        20f32.to_radians(),
    );
    let on_floor = |i: usize| (p[i][1] + 0.2).abs() < 1e-3;
    let (mut cube, mut floor) = (0, 0);
    for i in 0..w * h {
        if p[i] == [0.0, 0.0, 0.0] {
            continue; // the sky
        }
        if on_floor(i) {
            floor += 1;
            // Closest: never the cube's, even on its silhouette.
            assert_eq!(pref[i], [0.0; 3]);
            // Filtered: blended on the silhouette, so read inside the floor.
            let (x, y) = (i % w, i / w);
            let inside = x > 0
                && y > 0
                && x + 1 < w
                && y + 1 < h
                && [i - 1, i + 1, i - w, i + w].iter().all(|&j| on_floor(j));
            if inside {
                assert_eq!(uv2[i], [0.0; 2]);
                assert_eq!(cd[i], [0.0; 3]);
            }
            continue;
        }
        cube += 1;
        let local = rot.transpose() * (glam::Vec3::from_slice(&p[i]) - glam::Vec3::Y);
        assert!(
            close(&pref[i], &local.to_array(), 1e-4),
            "Pref {:?} vs P {:?} carried back {local}",
            pref[i],
            p[i]
        );
        assert!(
            uv2[i].iter().all(|c| (-1e-5..=1.0 + 1e-5).contains(c)),
            "{:?}",
            uv2[i]
        );
    }
    assert!(cube > 0 && floor > 0);
}
