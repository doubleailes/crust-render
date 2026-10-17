//! The curve mass field, end to end from USD: the `crust:mass:*` attributes
//! and their grouping, the material's colour terminal, the lookups a
//! material reads at a hit (through a placement, or falling back to their
//! defaults), the stats, and the bake's determinism.
//!
//! Each stage binds pure emitters whose colour is a lookup, so a hit's
//! `emitted_at` reads back exactly what the material computed there: the
//! normal encoded as `n · 0.5 + 0.5` (emission clamps a negative channel),
//! the colour as it is, and depth and density in the first two channels.

use crust_core::{Ray, Scene, Vec3A, WarningCode};
use std::path::PathBuf;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir()
        .join("crust_mass_field_tests")
        .join(name);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// The MaterialX materials every stage can bind: `normal`, `color`, `depth`
/// (depth and density) and `plain`, each a pure emitter.
const MTLX: &str = r#"<?xml version="1.0"?>
<materialx version="1.39">
  <crust_mass_normal name="n" type="vector3"><input name="default" type="vector3" value="0, 0, 1" /></crust_mass_normal>
  <multiply name="nh" type="vector3"><input name="in1" type="vector3" nodename="n" /><input name="in2" type="vector3" value="0.5, 0.5, 0.5" /></multiply>
  <add name="ne" type="vector3"><input name="in1" type="vector3" nodename="nh" /><input name="in2" type="vector3" value="0.5, 0.5, 0.5" /></add>
  <convert name="nc" type="color3"><input name="in" type="vector3" nodename="ne" /></convert>
  <uniform_edf name="edf_n" type="EDF"><input name="color" type="color3" nodename="nc" /></uniform_edf>
  <surface name="surf_n" type="surfaceshader"><input name="edf" type="EDF" nodename="edf_n" /></surface>
  <surfacematerial name="normal" type="material"><input name="surfaceshader" type="surfaceshader" nodename="surf_n" /></surfacematerial>

  <crust_mass_color name="c" type="color3"><input name="default" type="color3" value="0.5, 0.5, 0.5" /></crust_mass_color>
  <uniform_edf name="edf_c" type="EDF"><input name="color" type="color3" nodename="c" /></uniform_edf>
  <surface name="surf_c" type="surfaceshader"><input name="edf" type="EDF" nodename="edf_c" /></surface>
  <surfacematerial name="color" type="material"><input name="surfaceshader" type="surfaceshader" nodename="surf_c" /></surfacematerial>

  <crust_mass_depth name="d" type="float" />
  <crust_mass_density name="rho" type="float" />
  <combine3 name="dd" type="color3"><input name="in1" type="float" nodename="d" /><input name="in2" type="float" nodename="rho" /><input name="in3" type="float" value="0" /></combine3>
  <uniform_edf name="edf_d" type="EDF"><input name="color" type="color3" nodename="dd" /></uniform_edf>
  <surface name="surf_d" type="surfaceshader"><input name="edf" type="EDF" nodename="edf_d" /></surface>
  <surfacematerial name="depth" type="material"><input name="surfaceshader" type="surfaceshader" nodename="surf_d" /></surfacematerial>

  <constant name="grey" type="color3"><input name="value" type="color3" value="0.25, 0.5, 0.75" /></constant>
  <uniform_edf name="edf_p" type="EDF"><input name="color" type="color3" nodename="grey" /></uniform_edf>
  <surface name="surf_p" type="surfaceshader"><input name="edf" type="EDF" nodename="edf_p" /></surface>
  <surfacematerial name="plain" type="material"><input name="surfaceshader" type="surfaceshader" nodename="surf_p" /></surfacematerial>
</materialx>
"#;

/// A stage of `body` under `/World`, with `/World/Looks/<name>` for each
/// material of [`MTLX`] — `extra` authored inside each Material prim (a
/// colour terminal, say).
fn load_with(name: &str, extra: &str, body: &str) -> Scene {
    let d = dir(name);
    let mtlx = d.join("m.mtlx");
    std::fs::write(&mtlx, MTLX).expect("write mtlx");
    let looks: String = ["normal", "color", "depth", "plain"]
        .iter()
        .map(|m| {
            format!(
                "        def Material \"{m}\" (\n            prepend references = @{}@</MaterialX/Materials/{m}>\n        )\n        {{\n{extra}\n        }}\n",
                mtlx.display()
            )
        })
        .collect();
    let stage = d.join("stage.usda");
    std::fs::write(
        &stage,
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
    }}
    def Scope "Looks"
    {{
{looks}    }}
{body}
}}
"#
        ),
    )
    .expect("write stage");
    Scene::from_usd(&stage).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// The prims of every field, each field's sorted, the fields sorted.
fn field_prims(scene: &Scene) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = scene
        .world
        .mass_fields()
        .iter()
        .map(|f| f.prims().to_vec())
        .collect();
    out.sort();
    out
}

fn load(name: &str, body: &str) -> Scene {
    load_with(name, "", body)
}

/// What the material emits where a ray from `origin` along `dir` first hits.
fn emitted(scene: &Scene, origin: Vec3A, dir: Vec3A) -> Vec3A {
    let ray = Ray::new(origin, dir);
    let hit = scene
        .world
        .intersect(&ray, 1e-4, 1e4)
        .unwrap_or_else(|| panic!("no hit from {origin} along {dir}"));
    hit.mat.emitted_at(&ray, &hit.rec, 1.0)
}

/// What a ray straight down the −z axis at `(x, y)` reads.
fn at(scene: &Scene, x: f32, y: f32) -> Vec3A {
    emitted(scene, Vec3A::new(x, y, 5.0), -Vec3A::Z)
}

/// The normal a `normal` material encoded.
fn decode(e: Vec3A) -> Vec3A {
    (e - 0.5) * 2.0
}

/// `n · 0.5 + 0.5`, as the program computes it, lane by lane.
fn encode(n: Vec3A) -> Vec3A {
    Vec3A::new(n.x * 0.5 + 0.5, n.y * 0.5 + 0.5, n.z * 0.5 + 0.5)
}

fn bits(v: Vec3A) -> [u32; 3] {
    [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]
}

fn codes(scene: &Scene) -> Vec<WarningCode> {
    scene.warnings.iter().map(|w| w.code).collect()
}

fn record<'a>(scene: &'a Scene, code: WarningCode) -> &'a crust_core::Warning {
    scene
        .warnings
        .iter()
        .find(|w| w.code == code)
        .unwrap_or_else(|| panic!("no {code} in {:#?}", scene.warnings))
}

/// A linear `BasisCurves` prim of the given strands, bound to `material`.
fn curves(
    name: &str,
    strands: &[Vec<[f32; 3]>],
    width: f32,
    material: &str,
    extra: &str,
) -> String {
    let counts: Vec<String> = strands.iter().map(|s| s.len().to_string()).collect();
    let points: Vec<String> = strands
        .iter()
        .flatten()
        .map(|p| format!("({}, {}, {})", p[0], p[1], p[2]))
        .collect();
    format!(
        r#"
    def BasisCurves "{name}" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token type = "linear"
        int[] curveVertexCounts = [{}]
        point3f[] points = [{}]
        float[] widths = [{width}] (interpolation = "constant")
        rel material:binding = </World/Looks/{material}>
        {extra}
    }}"#,
        counts.join(", "),
        points.join(", ")
    )
}

/// The spike's sheets: `n` straight strands in the plane `z = tilt · x`,
/// running at `angle` degrees from the x axis, `spacing` apart across them,
/// `half` long either side of their centre. Sheet A (`tilt` 0.25, +25°) and
/// sheet B (−0.25, −25°) cross along the y axis.
fn sheet_strands(n: i32, spacing: f32, angle: f32, tilt: f32, half: f32) -> Vec<Vec<[f32; 3]>> {
    let a = angle.to_radians();
    let d = Vec3A::new(a.cos(), a.sin(), tilt * a.cos()).normalize();
    let plane_n = Vec3A::new(-tilt, 0.0, 1.0).normalize();
    let e = d.cross(plane_n).normalize();
    (-n / 2..=n / 2)
        .map(|k| {
            let c = e * (k as f32 * spacing);
            let (p0, p1) = (c - d * half, c + d * half);
            vec![p0.to_array(), p1.to_array()]
        })
        .collect()
}

const SPACING: f32 = 0.04;
const WIDTH: f32 = 0.05;

fn sheet(name: &str, tilt: f32, angle: f32, material: &str, extra: &str) -> String {
    curves(
        name,
        &sheet_strands(25, SPACING, angle, tilt, 1.2),
        WIDTH,
        material,
        extra,
    )
}

/// A round clump of parallel strands along y: a mass of its own.
fn clump_strands() -> Vec<Vec<[f32; 3]>> {
    let mut out = Vec::new();
    let n = 7;
    for i in -n..=n {
        for j in -n..=n {
            let (x, z) = (i as f32 * SPACING, j as f32 * SPACING);
            if x * x + z * z <= 0.25 * 0.25 {
                out.push(vec![[x, -1.0, z], [x, 1.0, z]]);
            }
        }
    }
    out
}

fn mass(radius: f32) -> String {
    format!("float crust:mass:radius = {radius}")
}

// ---------------------------------------------------------------------------
// Attributes
// ---------------------------------------------------------------------------

#[test]
fn curves_without_a_radius_bake_nothing() {
    let scene = load(
        "no_radius",
        &(curves("A", &clump_strands(), WIDTH, "normal", "")
            + &curves(
                "B",
                &clump_strands(),
                WIDTH,
                "normal",
                "float crust:mass:radius = 0",
            )),
    );
    assert!(scene.world.mass_fields().is_empty());
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    // The lookup reads its default, and the curves carry no table.
    assert_eq!(bits(at(&scene, 0.0, 0.0)), bits(encode(Vec3A::Z)));
    let ray = Ray::new(Vec3A::new(0.0, 0.0, 5.0), -Vec3A::Z);
    let hit = scene.world.intersect(&ray, 1e-4, 1e4).expect("a strand");
    assert!(hit.rec.curve().is_none());
}

#[test]
fn out_of_domain_settings_are_refused() {
    let scene = load(
        "refused",
        &(curves(
            "Negative",
            &clump_strands(),
            WIDTH,
            "normal",
            "float crust:mass:radius = -0.1",
        ) + &curves(
            "Cap",
            &clump_strands(),
            WIDTH,
            "normal",
            "float crust:mass:radius = 0.12\n        int crust:mass:maxResolution = 0\n        float crust:mass:threshold = -1",
        )),
    );
    let fields = scene.world.mass_fields();
    assert_eq!(fields.len(), 1, "{fields:?}");
    assert_eq!(fields[0].prims(), ["/World/Cap"]);
    let s = fields[0].settings();
    assert_eq!(s.max_resolution, 512);
    assert_eq!(s.threshold, 0.5);
    assert_eq!(s.radius, 0.12);
    let w = record(&scene, WarningCode::MassInvalidSetting);
    assert_eq!(w.count, 3);
    let mut prims = w.prims.clone();
    prims.sort();
    assert_eq!(prims, ["/World/Cap", "/World/Negative"]);
    assert_eq!(codes(&scene), [WarningCode::MassInvalidSetting]);
}

#[test]
fn the_defaults_are_half_and_five_hundred_and_twelve() {
    let scene = load(
        "defaults",
        &curves("A", &clump_strands(), WIDTH, "normal", &mass(0.12)),
    );
    let f = &scene.world.mass_fields()[0];
    let s = f.settings();
    assert_eq!((s.radius, s.threshold, s.max_resolution), (0.12, 0.5, 512));
    assert_eq!(f.voxel(), 0.06);
    assert!(f.has_surface() && !f.has_color() && !f.capped());
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
}

#[test]
fn the_radius_is_in_the_curves_own_units() {
    let under = |name: &str, scale: &str| {
        load(
            name,
            &format!(
                r#"
    def Xform "Rig"
    {{
        float3 xformOp:scale = {scale}
        uniform token[] xformOpOrder = ["xformOp:scale"]
        {}
    }}"#,
                curves("Groom", &clump_strands(), WIDTH, "normal", &mass(0.05))
            ),
        )
    };
    let uniform = under("scale_uniform", "(2, 2, 2)");
    assert_eq!(uniform.world.mass_fields()[0].settings().radius, 0.1);
    // A non-uniform scale takes the cube root of its determinant, the same
    // in every direction: the field is baked in world space.
    let stretched = under("scale_stretched", "(1, 8, 1)");
    assert_eq!(stretched.world.mass_fields()[0].settings().radius, 0.1);
    // Baked in world space: the scaled clump's field spans the scaled
    // strands, twice the unscaled one's extent along y.
    let plain = load(
        "scale_none",
        &curves("Groom", &clump_strands(), WIDTH, "normal", &mass(0.05)),
    );
    let extent = |s: &Scene| {
        let f = &s.world.mass_fields()[0];
        f.dims()[1] as f32 * f.voxel()
    };
    assert!(
        extent(&uniform) > 1.8 * extent(&plain),
        "{} {}",
        extent(&uniform),
        extent(&plain)
    );
}

#[test]
fn mass_attributes_on_a_mesh_bake_nothing() {
    let card = |extra: &str| {
        format!(
            r#"
    def Mesh "Card" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)]
        rel material:binding = </World/Looks/plain>
        {extra}
    }}"#
        )
    };
    let with = load("mesh_with", &card(&mass(0.05)));
    let without = load("mesh_without", &card(""));
    assert!(with.world.mass_fields().is_empty());
    let w = record(&with, WarningCode::MassNotCurves);
    assert_eq!(w.prims, ["/World/Card"]);
    assert_eq!(codes(&with), [WarningCode::MassNotCurves]);
    let ray = Ray::new(Vec3A::new(0.2, 0.3, 5.0), -Vec3A::Z);
    let a = with.world.intersect(&ray, 1e-4, 1e4).expect("the card");
    let b = without.world.intersect(&ray, 1e-4, 1e4).expect("the card");
    assert_eq!(a.rec.t.to_bits(), b.rec.t.to_bits());
    assert_eq!(
        bits(a.mat.emitted_at(&ray, &a.rec, 1.0)),
        bits(b.mat.emitted_at(&ray, &b.rec, 1.0))
    );
}

// ---------------------------------------------------------------------------
// Grouping
// ---------------------------------------------------------------------------

#[test]
fn prims_sharing_a_group_bake_one_field() {
    let strands = clump_strands();
    let third = strands.len() / 3;
    let group = "float crust:mass:radius = 0.12\n        token crust:mass:group = \"hair\"";
    let scene = load(
        "grouped",
        &(curves("A", &strands[..third], WIDTH, "normal", group)
            + &curves("B", &strands[third..2 * third], WIDTH, "normal", group)
            + &curves("C", &strands[2 * third..], WIDTH, "normal", group)),
    );
    let fields = scene.world.mass_fields();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].prims(), ["/World/A", "/World/B", "/World/C"]);
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    // Ungrouped, each prim bakes its own.
    let alone = load(
        "ungrouped",
        &(curves("A", &strands[..third], WIDTH, "normal", &mass(0.12))
            + &curves("B", &strands[third..], WIDTH, "normal", &mass(0.12))),
    );
    assert_eq!(alone.world.mass_fields().len(), 2);
}

#[test]
fn two_placements_never_share_a_field() {
    let group = "float crust:mass:radius = 0.12\n        token crust:mass:group = \"hair\"";
    let character = |name: &str, x: f32| {
        format!(
            r#"
    def Xform "{name}"
    {{
        double3 xformOp:translate = ({x}, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        {}
    }}"#,
            curves("Groom", &clump_strands(), WIDTH, "normal", group)
        )
    };
    let two = load(
        "two_characters",
        &(character("Left", -3.0) + &character("Right", 3.0)),
    );
    let fields = two.world.mass_fields();
    assert_eq!(fields.len(), 2, "{fields:?}");
    assert_eq!(
        field_prims(&two),
        [vec!["/World/Left/Groom"], vec!["/World/Right/Groom"]]
    );
    // Siblings under one transform share one.
    let one = load(
        "one_placement",
        &format!(
            r#"
    def Xform "Char"
    {{
        double3 xformOp:translate = (3, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        {}
        {}
    }}"#,
            curves("A", &clump_strands()[..40], WIDTH, "normal", group),
            curves("B", &clump_strands()[40..], WIDTH, "normal", group)
        ),
    );
    assert_eq!(one.world.mass_fields().len(), 1);
}

#[test]
fn grouped_prims_that_disagree_take_the_firsts_settings() {
    let strands = clump_strands();
    let scene = load(
        "conflict",
        &(curves(
            "A",
            &strands[..60],
            WIDTH,
            "normal",
            "float crust:mass:radius = 0.1\n        token crust:mass:group = \"hair\"",
        ) + &curves(
            "B",
            &strands[60..],
            WIDTH,
            "normal",
            "float crust:mass:radius = 0.2\n        token crust:mass:group = \"hair\"",
        )),
    );
    let fields = scene.world.mass_fields();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].settings().radius, 0.1);
    let w = record(&scene, WarningCode::MassGroupConflict);
    assert_eq!(w.prims, ["/World/B"]);
    assert!(
        w.message.contains("\"hair\"") && w.message.contains("/World/A"),
        "{}",
        w.message
    );
    assert_eq!(codes(&scene), [WarningCode::MassGroupConflict]);
}

// ---------------------------------------------------------------------------
// The colour terminal
// ---------------------------------------------------------------------------

/// A colour terminal driven by an inline `ND_constant_color3`.
fn constant_terminal(rgb: &str) -> String {
    format!(
        r#"            token outputs:crust:mass:color.connect = </World/Looks/color/Tint.outputs:out>
            def Shader "Tint"
            {{
                uniform token info:id = "ND_constant_color3"
                color3f inputs:value = {rgb}
                color3f outputs:out
            }}"#
    )
}

#[test]
fn the_colour_terminal_bakes_a_colour_and_leaves_the_surface_alone() {
    let body = curves("Clump", &clump_strands(), WIDTH, "color", &mass(0.12));
    let tinted = load_with("terminal", &constant_terminal("(0.9, 0.3, 0.1)"), &body);
    let f = &tinted.world.mass_fields()[0];
    assert!(f.has_color());
    assert!(tinted.warnings.is_empty(), "{:#?}", tinted.warnings);
    let c = at(&tinted, 0.0, 0.0);
    assert!((c - Vec3A::new(0.9, 0.3, 0.1)).length() < 1e-3, "{c}");
    // Without a terminal the field holds no colour and the lookup gives its
    // default.
    let bare = load("terminal_bare", &body);
    assert!(!bare.world.mass_fields()[0].has_color());
    assert_eq!(bits(at(&bare, 0.0, 0.0)), bits(Vec3A::splat(0.5)));
    // The terminal does not change how the material shades: the `plain`
    // material, which reads nothing, emits the same bits with and without.
    let plain = curves("Clump", &clump_strands(), WIDTH, "plain", &mass(0.12));
    let with = load_with(
        "terminal_plain",
        &constant_terminal("(0.9, 0.3, 0.1)"),
        &plain,
    );
    let without = load("terminal_plain_bare", &plain);
    let ray = Ray::new(Vec3A::new(0.01, 0.2, 5.0), -Vec3A::Z);
    let a = with.world.intersect(&ray, 1e-4, 1e4).expect("a strand");
    let b = without.world.intersect(&ray, 1e-4, 1e4).expect("a strand");
    assert_eq!(
        bits(a.mat.emitted_at(&ray, &a.rec, 1.0)),
        bits(b.mat.emitted_at(&ray, &b.rec, 1.0))
    );
    let wi = Vec3A::new(0.3, 0.2, 0.9).normalize();
    assert_eq!(
        a.mat
            .eval(&ray, &a.rec, wi)
            .map(|(v, p)| (bits(v), p.to_bits())),
        b.mat
            .eval(&ray, &b.rec, wi)
            .map(|(v, p)| (bits(v), p.to_bits()))
    );
    assert!(with.world.mass_fields()[0].has_color());
}

#[test]
fn a_terminal_on_a_meshes_material_bakes_nothing() {
    let card = r#"
    def Mesh "Card" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)]
        rel material:binding = </World/Looks/color>
    }"#;
    let with = load_with("mesh_terminal", &constant_terminal("(1, 0, 0)"), card);
    let without = load("mesh_terminal_bare", card);
    assert!(with.world.mass_fields().is_empty());
    assert!(with.warnings.is_empty(), "{:#?}", with.warnings);
    assert_eq!(bits(at(&with, 0.2, 0.3)), bits(at(&without, 0.2, 0.3)));
    assert_eq!(bits(at(&with, 0.2, 0.3)), bits(Vec3A::splat(0.5)));
}

#[test]
fn a_terminal_to_a_non_materialx_shader_is_ignored() {
    let extra = r#"            token outputs:crust:mass:color.connect = </World/Looks/color/Preview.outputs:surface>
            def Shader "Preview"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (1, 0, 0)
                token outputs:surface
            }"#;
    let body = curves("Clump", &clump_strands(), WIDTH, "color", &mass(0.12));
    let scene = load_with("terminal_preview", extra, &body);
    let w = record(&scene, WarningCode::MassColorUnsupported);
    assert_eq!(w.prims, ["/World/Looks/color"]);
    assert!(!scene.world.mass_fields()[0].has_color());
    assert_eq!(bits(at(&scene, 0.0, 0.0)), bits(Vec3A::splat(0.5)));
}

#[test]
fn a_view_dependent_colour_node_is_refused() {
    let extra = r#"            token outputs:crust:mass:color.connect = </World/Looks/color/ToColor.outputs:out>
            def Shader "View"
            {
                uniform token info:id = "ND_viewdirection_vector3"
                vector3f outputs:out
            }
            def Shader "ToColor"
            {
                uniform token info:id = "ND_convert_vector3_color3"
                vector3f inputs:in.connect = </World/Looks/color/View.outputs:out>
                color3f outputs:out
            }"#;
    let body = curves("Clump", &clump_strands(), WIDTH, "color", &mass(0.12));
    let scene = load_with("terminal_view", extra, &body);
    let w = record(&scene, WarningCode::MassColorUnsupported);
    assert_eq!(w.prims, ["/World/Looks/color"]);
    assert!(w.message.contains("viewdirection"), "{}", w.message);
    // The graph still bakes, with the node at its default.
    assert!(scene.world.mass_fields()[0].has_color());
}

#[test]
fn a_root_to_tip_ramp_bakes_into_the_mass() {
    let extra = r#"            token outputs:crust:mass:color.connect = </World/Looks/color/Ramp.outputs:out>
            def Shader "V"
            {
                uniform token info:id = "ND_geompropvalue_float"
                string inputs:geomprop = "curve:v"
                float outputs:out
            }
            def Shader "Ramp"
            {
                uniform token info:id = "ND_mix_color3"
                color3f inputs:bg = (1, 0, 0)
                color3f inputs:fg = (0, 0, 1)
                float inputs:mix.connect = </World/Looks/color/V.outputs:out>
                color3f outputs:out
            }"#;
    // A sheet of parallel strands from y = −1 (root) to y = +1 (tip).
    let strands: Vec<Vec<[f32; 3]>> = (-12..=12)
        .map(|k| {
            vec![
                [k as f32 * SPACING, -1.0, 0.0],
                [k as f32 * SPACING, 1.0, 0.0],
            ]
        })
        .collect();
    let scene = load_with(
        "ramp",
        extra,
        &curves("Sheet", &strands, WIDTH, "color", &mass(0.12)),
    );
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    let root = at(&scene, 0.0, -0.9);
    let tip = at(&scene, 0.0, 0.9);
    assert!(root.x > 0.9 && root.z < 0.1, "root {root}");
    assert!(tip.z > 0.9 && tip.x < 0.1, "tip {tip}");
    let mid = at(&scene, 0.0, 0.0);
    assert!(
        (mid.x - 0.5).abs() < 0.05 && (mid.z - 0.5).abs() < 0.05,
        "mid {mid}"
    );
}

// ---------------------------------------------------------------------------
// Lookups
// ---------------------------------------------------------------------------

#[test]
fn crossing_sheets_read_the_same_normal_and_colour() {
    let group = "float crust:mass:radius = 0.12\n        token crust:mass:group = \"sheets\"";
    let body = sheet("A", 0.25, 25.0, "normal", group) + &sheet("B", -0.25, -25.0, "normal", group);
    let scene = load("crossing", &body);
    assert_eq!(scene.world.mass_fields().len(), 1);
    // Just either side of the crossing line the front sheet changes: A for
    // x > 0, B for x < 0. The tube normals differ; the mass normals agree.
    let (ray_a, ray_b) = (
        Ray::new(Vec3A::new(0.03, 0.2, 5.0), -Vec3A::Z),
        Ray::new(Vec3A::new(-0.03, 0.2, 5.0), -Vec3A::Z),
    );
    let a = scene.world.intersect(&ray_a, 1e-4, 1e4).expect("sheet A");
    let b = scene.world.intersect(&ray_b, 1e-4, 1e4).expect("sheet B");
    assert_ne!(
        a.geom_id, b.geom_id,
        "the front sheet changes at the crossing"
    );
    let na = decode(a.mat.emitted_at(&ray_a, &a.rec, 1.0));
    let nb = decode(b.mat.emitted_at(&ray_b, &b.rec, 1.0));
    // The planes meet at 28°; the mass keeps that crease, smoothed over
    // about a radius, so a radius either side the two sheets' normals are
    // already within a few degrees, and at the crossing itself the field is
    // one continuous normal both sheets read.
    let angle = na.dot(nb).clamp(-1.0, 1.0).acos().to_degrees();
    assert!(
        angle < 10.0,
        "mass normals {na} and {nb} differ by {angle}°"
    );
    assert!(
        na.z > 0.9 && nb.z > 0.9,
        "the mass faces the camera: {na} {nb}"
    );
    // The planes themselves meet at 28°: the field has halved the
    // discontinuity a radius out, and removed it at the crossing.
    let read = |geom: u32, p: Vec3A| {
        scene
            .world
            .mass_lookup(geom)
            .expect("a lookup")
            .read(p, crust_core::mtlx::MassReads(2))
            .normal
            .expect("a normal")
    };
    // Reading the field from either sheet is the same function of position.
    let top = Vec3A::new(0.0, 0.2, 0.03);
    assert_eq!(
        bits(read(a.geom_id, top)),
        bits(read(b.geom_id, top)),
        "the same lookup from either sheet"
    );
    assert!(read(a.geom_id, top).z > 0.95, "{}", read(a.geom_id, top));
    // Each is exactly the field's lookup at its hit — a function of
    // position alone.
    for (hit, ray) in [(&a, &ray_a), (&b, &ray_b)] {
        let lookup = scene.world.mass_lookup(hit.geom_id).expect("a lookup");
        let n = lookup
            .read(hit.rec.p, crust_core::mtlx::MassReads(2))
            .normal
            .expect("a normal");
        assert_eq!(
            bits(hit.mat.emitted_at(ray, &hit.rec, 1.0)),
            bits(encode(n))
        );
    }
    // Away from the crossing the sheets are apart, and each reads its own
    // plane's normal: A's tilts toward −x, B's toward +x.
    let far_a = decode(at(&scene, 0.6, 0.2));
    let far_b = decode(at(&scene, -0.6, 0.2));
    assert!(far_a.x < -0.1 && far_b.x > 0.1, "{far_a} {far_b}");
}

#[test]
fn a_lookup_off_a_field_reads_its_default_without_a_warning() {
    let scene = load(
        "mesh_lookup",
        r#"
    def Mesh "Card" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)]
        rel material:binding = </World/Looks/normal>
    }"#,
    );
    assert_eq!(bits(at(&scene, 0.2, 0.3)), bits(encode(Vec3A::Z)));
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
}

#[test]
fn depth_and_density_read_the_clump() {
    let scene = load(
        "depth",
        &curves("Clump", &clump_strands(), WIDTH, "depth", &mass(0.12)),
    );
    // Seen from above, the axis is the top of the clump: no depth, and the
    // strands' coverage.
    let top = at(&scene, 0.0, 0.0);
    assert!(top.y > 0.3, "density {top}");
    // Inside the clump (a strand on the far side, reached through a gap)
    // the depth is positive: probe the field through the lookup instead,
    // which is what the material reads.
    let ray = Ray::new(Vec3A::new(0.0, 0.0, 5.0), -Vec3A::Z);
    let hit = scene.world.intersect(&ray, 1e-4, 1e4).expect("a strand");
    let lookup = scene.world.mass_lookup(hit.geom_id).expect("a lookup");
    let inside = lookup.read(Vec3A::new(0.1, 0.0, 0.0), crust_core::mtlx::MassReads(15));
    assert!(inside.depth > 0.0 && inside.density > 0.5, "{inside:?}");
    assert!(inside.normal.unwrap().x > 0.9, "{inside:?}");
    let outside = lookup.read(Vec3A::new(0.5, 0.0, 0.0), crust_core::mtlx::MassReads(15));
    assert_eq!(outside.depth, 0.0);
    assert_eq!(outside.density, 0.0);
}

// ---------------------------------------------------------------------------
// Placements
// ---------------------------------------------------------------------------

/// A prototype groom (a sheet, so its normal has a direction) — a `class`,
/// which the traversal does not draw on its own — and the placements of it
/// in `instances`.
fn instanced(name: &str, material: &str, instances: &str) -> Scene {
    load(
        name,
        &format!(
            r#"
    class Xform "_Groom"
    {{
        {}
    }}
{instances}"#,
            sheet("Sheet", 0.25, 25.0, material, &mass(0.12))
        ),
    )
}

/// The one prim an instanced prototype's warning names: the prototype's
/// own (a native instance's is numbered by composition).
fn proto_prim(w: &crust_core::Warning) -> bool {
    w.prims.len() == 1 && w.prims[0].ends_with("/Sheet")
}

#[test]
fn direct_instances_read_their_field_through_their_placement() {
    let scene = instanced(
        "instances",
        "normal",
        r#"
    def Xform "A" (instanceable = true; references = </World/_Groom>)
    {
        double3 xformOp:translate = (-3, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Xform "B" (instanceable = true; references = </World/_Groom>)
    {
        double3 xformOp:translate = (3, 0, 0)
        float3 xformOp:rotateXYZ = (0, 0, 90)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateXYZ"]
    }"#,
    );
    // One field per prototype — per chunk of the streamed import, which
    // composes a prototype met in two chunks twice (the epoch scoping): so
    // one or two here, never one per placement.
    assert!(
        (1..=2).contains(&scene.world.mass_fields().len()),
        "{:?}",
        field_prims(&scene)
    );
    for f in scene.world.mass_fields() {
        assert_eq!(f.prims().len(), 1);
        assert!(f.prims()[0].starts_with("/__Prototype_"), "{:?}", f.prims());
    }
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    // The same point of the groom, in each instance: B's normal is A's
    // turned a quarter turn about z.
    let p = Vec3A::new(0.5, 0.2, 0.0);
    let na = decode(at(&scene, -3.0 + p.x, p.y));
    let nb = decode(at(&scene, 3.0 - p.y, p.x));
    let rotated = Vec3A::new(-na.y, na.x, na.z);
    assert!(
        (nb - rotated).length() < 0.05,
        "A {na}, B {nb}, A turned {rotated}"
    );
    assert!(na.x < -0.1, "A tilts toward −x: {na}");
}

#[test]
fn a_uniformly_scaled_instance_scales_its_mass() {
    let scene = instanced(
        "instances_scaled",
        "depth",
        r#"
    def Xform "A" (instanceable = true; references = </World/_Groom>)
    {
        double3 xformOp:translate = (-3, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Xform "B" (instanceable = true; references = </World/_Groom>)
    {
        double3 xformOp:translate = (3, 0, 0)
        float3 xformOp:scale = (2, 2, 2)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:scale"]
    }"#,
    );
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    let ray = Ray::new(Vec3A::new(-2.5, 0.2, 5.0), -Vec3A::Z);
    let a = scene.world.intersect(&ray, 1e-4, 1e4).expect("instance A");
    let ray_b = Ray::new(Vec3A::new(4.0, 0.4, 5.0), -Vec3A::Z);
    let b = scene
        .world
        .intersect(&ray_b, 1e-4, 1e4)
        .expect("instance B");
    let (la, lb) = (
        scene.world.mass_lookup(a.geom_id).expect("A reads"),
        scene.world.mass_lookup(b.geom_id).expect("B reads"),
    );
    // Corresponding points: a point inside the sheet's mass and its image.
    let q = Vec3A::new(0.5, 0.2, 0.125);
    let all = crust_core::mtlx::MassReads(15);
    let ra = la.read(Vec3A::new(-3.0, 0.0, 0.0) + q, all);
    let rb = lb.read(Vec3A::new(3.0, 0.0, 0.0) + q * 2.0, all);
    assert!(ra.depth > 0.0, "{ra:?}");
    assert!((rb.depth - 2.0 * ra.depth).abs() < 1e-4, "{ra:?} {rb:?}");
    assert_eq!(ra.normal.map(bits), rb.normal.map(bits));
    assert!((ra.density - rb.density).abs() < 1e-5);
}

#[test]
fn a_non_uniformly_scaled_instance_falls_back() {
    let scene = instanced(
        "instances_squashed",
        "normal",
        r#"
    def Xform "B" (instanceable = true; references = </World/_Groom>)
    {
        float3 xformOp:scale = (1, 2, 1)
        uniform token[] xformOpOrder = ["xformOp:scale"]
    }"#,
    );
    let w = record(&scene, WarningCode::MassUnplaceable);
    assert!(proto_prim(w), "{:?}", w.prims);
    assert_eq!(codes(&scene), [WarningCode::MassUnplaceable]);
    assert_eq!(
        scene.world.mass_fields().len(),
        1,
        "the field is still baked"
    );
    assert_eq!(bits(at(&scene, 0.5, 0.4)), bits(encode(Vec3A::Z)));
}

#[test]
fn an_instancer_placed_groom_falls_back() {
    // The prototype under the instancer, as a `PointInstancer`'s usually is:
    // drawn through it, never on its own.
    let scene = load(
        "instancer",
        &format!(
            r#"
    def PointInstancer "Scatter"
    {{
        rel prototypes = [</World/Scatter/Protos/Groom>]
        int[] protoIndices = [0, 0]
        point3f[] positions = [(-3, 0, 0), (3, 0, 0)]
        def Scope "Protos"
        {{
            def Xform "Groom"
            {{
                {}
            }}
        }}
    }}"#,
            sheet("Sheet", 0.25, 25.0, "normal", &mass(0.12))
        ),
    );
    assert!(
        codes(&scene).contains(&WarningCode::MassUnplaceable),
        "{:?} fields {:?}",
        scene.warnings,
        field_prims(&scene)
    );
    let w = record(&scene, WarningCode::MassUnplaceable);
    assert!(proto_prim(w), "{:?}", w.prims);
    assert_eq!(w.count, 1, "once per prim, however many placements");
    assert_eq!(bits(at(&scene, -2.5, 0.2)), bits(encode(Vec3A::Z)));
    assert_eq!(bits(at(&scene, 3.5, 0.2)), bits(encode(Vec3A::Z)));
}

// ---------------------------------------------------------------------------
// Stats and determinism
// ---------------------------------------------------------------------------

#[test]
fn the_stats_list_the_fields() {
    let scene = load(
        "stats",
        &curves("Clump", &clump_strands(), WIDTH, "normal", &mass(0.12)),
    );
    let m = scene.stats.scene.mass_fields;
    assert_eq!(m.fields, 1);
    assert!(m.bricks > 0 && m.bytes > 0);
    assert!(m.time > std::time::Duration::ZERO);
    let report = scene.stats.report();
    assert!(report.contains("curve mass fields"), "{report}");
    assert!(report.contains("Bake mass fields"), "{report}");
    let json = scene.stats.to_json();
    assert!(json.contains("\"mass_fields\""), "{json}");
    // Nothing for a scene without a groom.
    let none = load(
        "stats_none",
        &curves("Clump", &clump_strands(), WIDTH, "normal", ""),
    );
    assert_eq!(none.stats.scene.mass_fields.fields, 0);
    assert!(!none.stats.report().contains("curve mass fields"));
}

#[test]
fn a_bake_is_bit_identical_on_one_thread_and_eight() {
    let body = curves("Clump", &clump_strands(), WIDTH, "color", &mass(0.12))
        + &sheet("Sheet", 0.25, 25.0, "normal", &mass(0.1));
    let extra = constant_terminal("(0.2, 0.4, 0.6)");
    let checksums = |threads: usize, name: &str| -> Vec<u64> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        pool.install(|| {
            let scene = load_with(name, &extra, &body);
            scene
                .world
                .mass_fields()
                .iter()
                .map(|f| f.checksum())
                .collect()
        })
    };
    let one = checksums(1, "threads_one");
    let eight = checksums(8, "threads_eight");
    assert_eq!(one.len(), 2);
    assert_eq!(one, eight);
}
