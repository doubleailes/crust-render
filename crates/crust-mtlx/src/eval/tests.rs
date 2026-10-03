use super::apply::{artistic_ior, hsv_to_rgb, rgb_to_hsv};
use super::*;
use crate::parse::Doc;
use crate::texture::TextureRef;

fn no_textures(_: &str, _: Option<&str>) -> Option<TextureRef> {
    None
}

fn run(doc_text: &str, node: &str) -> Val {
    let doc = Doc::parse(doc_text).unwrap();
    let loader = no_textures;
    let mut c = Compiler::new(&doc, &loader);
    let slot = c.compile_named("", node, None);
    let mut slots = Vec::new();
    c.program.eval(
        &ShadeCtx {
            uv: (0.25, 0.75),
            normal: Vec3A::Z,
            tangent: Vec3A::X,
            view: -Vec3A::Z,
            position: Vec3A::ZERO,
            uv_width: 0.0,
        },
        &mut slots,
    );
    slots[slot as usize]
}

#[test]
fn mix_blends_bg_toward_fg() {
    let v = run(
        r#"<materialx>
             <constant name="a" type="float"><input name="value" type="float" value="0" /></constant>
             <constant name="b" type="float"><input name="value" type="float" value="1" /></constant>
             <mix name="m" type="float">
               <input name="bg" type="float" nodename="a" />
               <input name="fg" type="float" nodename="b" />
               <input name="mix" type="float" value="0.25" />
             </mix>
           </materialx>"#,
        "m",
    );
    assert!((v.x() - 0.25).abs() < 1e-6, "got {}", v.x());
}

#[test]
fn artistic_ior_round_trips_to_its_reflectivity() {
    // The conductor lobe reduces (n, k) back to a reflectivity colour, so
    // the two conversions must be inverses or every metal shifts hue.
    for r in [0.05f32, 0.4, 0.94] {
        let (n, k) = artistic_ior(Vec3A::splat(r), Vec3A::ONE);
        let back = reflectivity_from_ior(n, k);
        assert!((back.x - r).abs() < 1e-4, "r={r} -> {}", back.x);
    }
}

#[test]
fn a_graph_output_keeps_the_output_it_selected() {
    // A `<nodegraph>`'s `<output>` may select one output of a multioutput
    // node. Dropping that selection is silent: the reference falls back to
    // the node's first output, so a graph publishing `artistic_ior`'s
    // extinction hands its consumer the ior instead.
    let doc = r#"<materialx>
             <nodegraph name="g">
               <artistic_ior name="ai" type="multioutput">
                 <input name="reflectivity" type="color3" value="0.5, 0.5, 0.5" />
                 <input name="edge_color" type="color3" value="1, 1, 1" />
               </artistic_ior>
               <output name="n" type="color3" nodename="ai" output="ior" />
               <output name="k" type="color3" nodename="ai" output="extinction" />
             </nodegraph>
             <multiply name="take_n" type="color3">
               <input name="in1" type="color3" nodegraph="g" output="n" />
               <input name="in2" type="color3" value="1, 1, 1" />
             </multiply>
             <multiply name="take_k" type="color3">
               <input name="in1" type="color3" nodegraph="g" output="k" />
               <input name="in2" type="color3" value="1, 1, 1" />
             </multiply>
           </materialx>"#;
    let (n, k) = artistic_ior(Vec3A::splat(0.5), Vec3A::ONE);
    let got_n = run(doc, "take_n");
    let got_k = run(doc, "take_k");
    assert!((got_n.x() - n.x).abs() < 1e-5, "ior: got {}", got_n.x());
    assert!(
        (got_k.x() - k.x).abs() < 1e-5,
        "extinction: got {}",
        got_k.x()
    );
    // The two outputs are what the test is about; equal values would make
    // the assertions above pass for the wrong reason.
    assert!((n.x - k.x).abs() > 1e-3);
}

#[test]
fn a_graph_output_without_a_selection_takes_the_first_output() {
    // The single-output majority authors no `output` attribute, and must
    // keep resolving as it did.
    let v = run(
        r#"<materialx>
             <nodegraph name="g">
               <artistic_ior name="ai" type="multioutput">
                 <input name="reflectivity" type="color3" value="0.5, 0.5, 0.5" />
                 <input name="edge_color" type="color3" value="1, 1, 1" />
               </artistic_ior>
               <output name="out" type="color3" nodename="ai" />
             </nodegraph>
             <multiply name="take" type="color3">
               <input name="in1" type="color3" nodegraph="g" output="out" />
               <input name="in2" type="color3" value="1, 1, 1" />
             </multiply>
           </materialx>"#,
        "take",
    );
    let (n, _) = artistic_ior(Vec3A::splat(0.5), Vec3A::ONE);
    assert!((v.x() - n.x).abs() < 1e-5, "got {}", v.x());
}

#[test]
fn a_cycle_terminates_instead_of_overflowing_the_stack() {
    let v = run(
        r#"<materialx>
             <multiply name="a" type="float">
               <input name="in1" type="float" nodename="b" />
             </multiply>
             <multiply name="b" type="float">
               <input name="in1" type="float" nodename="a" />
             </multiply>
           </materialx>"#,
        "a",
    );
    assert!(v.x().is_finite());
}

/// `colorcorrect` of a constant `in`, with `params` authored as floats.
fn colorcorrect(input: [f32; 3], params: &[(&str, f32)]) -> Vec3A {
    let inputs: String = params
        .iter()
        .map(|(n, v)| format!(r#"<input name="{n}" type="float" value="{v}" />"#))
        .collect();
    let doc = format!(
        r#"<materialx>
             <colorcorrect name="cc" type="color3">
               <input name="in" type="color3" value="{}, {}, {}" />
               {inputs}
             </colorcorrect>
           </materialx>"#,
        input[0], input[1], input[2]
    );
    let v = run(&doc, "cc");
    assert_eq!(v.arity, 3);
    v.rgb()
}

fn close(got: Vec3A, want: Vec3A) {
    assert!(
        (got - want).abs().max_element() < 1e-5,
        "got {got}, want {want}"
    );
}

#[test]
fn colorcorrect_defaults_are_the_identity() {
    // Bitwise: every stage folds to its identity and is left out.
    let c = [0.1, 0.7, 2.5];
    assert_eq!(colorcorrect(c, &[]), Vec3A::from(c));
    assert_eq!(
        colorcorrect(c, &[("hue", 0.0), ("gain", 1.0), ("exposure", 0.0)]),
        Vec3A::from(c)
    );
}

#[test]
fn colorcorrect_applies_each_stage_as_the_stdlib_graph_does() {
    let c = [0.125, 0.5, 1.0];
    close(colorcorrect(c, &[("gain", 4.0)]), Vec3A::new(0.5, 2.0, 4.0));
    // `range` with gamma: x^(1/gamma), sign-preserving, unclamped.
    close(
        colorcorrect([0.125, 8.0, -0.125], &[("gamma", 3.0)]),
        Vec3A::new(0.5, 2.0, -0.5),
    );
    // Lift raises black to `lift` and leaves white alone.
    close(
        colorcorrect([0.0, 0.5, 1.0], &[("lift", 0.5)]),
        Vec3A::new(0.5, 0.75, 1.0),
    );
    close(
        colorcorrect(
            [0.25, 0.5, 1.0],
            &[("contrast", 2.0), ("contrastpivot", 0.5)],
        ),
        Vec3A::new(0.0, 0.5, 1.5),
    );
    close(
        colorcorrect(c, &[("exposure", -1.0)]),
        Vec3A::new(0.0625, 0.25, 0.5),
    );
    // Saturation 0 is the luminance grey (MaterialX's ACEScg default
    // `lumacoeffs`); 2 pushes away from it.
    let l = 0.2722287 * 0.125 + 0.6740818 * 0.5 + 0.0536895;
    close(colorcorrect(c, &[("saturation", 0.0)]), Vec3A::splat(l));
    close(
        colorcorrect(c, &[("saturation", 2.0)]),
        Vec3A::from(c) * 2.0 - Vec3A::splat(l),
    );
    // Hue rotates in turns and wraps: a third of a turn takes red to
    // green, and 1.5 turns is the same as a half.
    close(
        colorcorrect([1.0, 0.0, 0.0], &[("hue", 1.0 / 3.0)]),
        Vec3A::new(0.0, 1.0, 0.0),
    );
    close(
        colorcorrect([1.0, 0.0, 0.0], &[("hue", 1.5)]),
        Vec3A::new(0.0, 1.0, 1.0),
    );
}

#[test]
fn colorcorrect_stages_run_in_the_stdlib_order() {
    // Gamma before lift before gain before contrast before exposure: any
    // two swapped gives a different answer on this input.
    let got = colorcorrect(
        [0.25; 3],
        &[
            ("gamma", 2.0),
            ("lift", 0.5),
            ("gain", 2.0),
            ("contrast", 0.5),
            ("exposure", 1.0),
        ],
    );
    let x = 0.25f32.sqrt(); // gamma → 0.5
    let x = x * (1.0 - 0.5) + 0.5; // lift → 0.75
    let x = x * 2.0; // gain → 1.5
    let x = (x - 0.5) * 0.5 + 0.5; // contrast → 1.0
    let x = x * 2.0; // exposure → 2.0
    close(got, Vec3A::splat(x));
}

#[test]
fn hsv_round_trips() {
    for c in [
        Vec3A::new(0.9, 0.2, 0.1),
        Vec3A::new(0.1, 0.8, 0.3),
        Vec3A::new(0.2, 0.3, 0.95),
        Vec3A::new(0.7, 0.1, 0.6),
        Vec3A::splat(0.4),
        Vec3A::new(3.0, 1.0, 0.5),
    ] {
        close(hsv_to_rgb(rgb_to_hsv(c)), c);
    }
}

/// A height of `slope_u · u + slope_v · v`, sampled through a real
/// `image` node so the shifted lookups are what is being differentiated.
struct Ramp {
    slope_u: f32,
    slope_v: f32,
}
impl crate::Texture for Ramp {
    fn eval(&self, u: f32, v: f32, _: f32) -> [f32; 4] {
        [self.slope_u * u + self.slope_v * v; 4]
    }
}

const HEIGHT_DOC: &str = r#"<materialx>
         <image name="h" type="float">
           <input name="file" type="filename" value="height.tif" />
         </image>
         <heighttonormal name="n" type="vector3">
           <input name="in" type="float" nodename="h" />
           <input name="scale" type="float" value="1" />
         </heighttonormal>
         <normalmap name="world" type="vector3">
           <input name="in" type="vector3" nodename="n" />
         </normalmap>
       </materialx>"#;

fn height_to_normal_at(slope_u: f32, slope_v: f32, uv_width: f32, node: &str) -> Vec3A {
    let doc = Doc::parse(HEIGHT_DOC).unwrap();
    let loader = move |_: &str, _: Option<&str>| {
        Some(TextureRef(std::sync::Arc::new(Ramp { slope_u, slope_v })))
    };
    let mut c = Compiler::new(&doc, &loader);
    let slot = c.compile_named("", node, None);
    let mut slots = Vec::new();
    let ctx = ShadeCtx {
        uv: (0.3, 0.6),
        normal: Vec3A::Z,
        tangent: Vec3A::X,
        view: -Vec3A::Z,
        position: Vec3A::ZERO,
        uv_width,
    };
    c.program.eval(&ctx, &mut slots);
    slots[slot as usize].rgb()
}

#[test]
fn heighttonormal_is_the_osl_reference_over_one_footprint() {
    // A height rising 10 per UV unit, over a 0.01-wide footprint, changes
    // by 0.1 across it: the reference's `Dx(in)`.
    let (du, dv) = (0.1f32, 0.0f32);
    let dz = (1.0 - du * du - dv * dv).sqrt();
    let want = Vec3A::new(-du, -dv, dz).normalize() * 0.5 + Vec3A::splat(0.5);
    close(height_to_normal_at(10.0, 0.0, 0.01, "n"), want);
    let want = Vec3A::new(0.0, -0.1, dz).normalize() * 0.5 + Vec3A::splat(0.5);
    close(height_to_normal_at(0.0, 10.0, 0.01, "n"), want);
}

#[test]
fn heighttonormal_tilts_away_from_rising_height() {
    // Through `normalmap` in a frame with the tangent along +u: a height
    // rising along +u (+v) is a slope facing −u (−v), which is where the
    // normal must lean.
    let n = height_to_normal_at(10.0, 0.0, 0.01, "world");
    assert!(n.x < -0.05 && n.y.abs() < 1e-6 && n.z > 0.9, "{n}");
    let n = height_to_normal_at(0.0, 10.0, 0.01, "world");
    assert!(n.y < -0.05 && n.x.abs() < 1e-6 && n.z > 0.9, "{n}");
}

#[test]
fn heighttonormal_without_a_footprint_is_flat() {
    // No ray cone, no derivative — the nodedef's own default output.
    assert_eq!(
        height_to_normal_at(10.0, 5.0, 0.0, "n"),
        Vec3A::new(0.5, 0.5, 1.0)
    );
}

/// `node` of `doc`, every image in it served by `tex`, at uv (0.3, 0.6)
/// with a footprint `uv_width` wide. Also returns the compiled program.
fn eval_with(
    doc: &str,
    node: &str,
    tex: impl crate::Texture + Clone + 'static,
    uv_width: f32,
) -> (Val, Program) {
    let doc = Doc::parse(doc).unwrap();
    let loader = move |_: &str, _: Option<&str>| Some(TextureRef(std::sync::Arc::new(tex.clone())));
    let mut c = Compiler::new(&doc, &loader);
    let slot = c.compile_named("", node, None);
    let mut slots = Vec::new();
    let ctx = ShadeCtx {
        uv: (0.3, 0.6),
        normal: Vec3A::Z,
        tangent: Vec3A::X,
        view: -Vec3A::Z,
        position: Vec3A::ZERO,
        uv_width,
    };
    c.program.eval(&ctx, &mut slots);
    (slots[slot as usize], c.program)
}

#[derive(Clone)]
struct RampTex(f32);
impl crate::Texture for RampTex {
    fn eval(&self, u: f32, _: f32, _: f32) -> [f32; 4] {
        [self.0 * u; 4]
    }
}

/// `heighttonormal` over an image whose `texcoord` is `coord` — a
/// fragment of nodes naming the connected one `c`, or empty for none.
fn height_doc(coord: &str) -> String {
    let conn = if coord.is_empty() {
        String::new()
    } else {
        r#"<input name="texcoord" type="vector2" nodename="c" />"#.into()
    };
    format!(
        r#"<materialx>
             {coord}
             <image name="h" type="float">
               <input name="file" type="filename" value="height.tif" />
               {conn}
             </image>
             <heighttonormal name="n" type="vector3">
               <input name="in" type="float" nodename="h" />
             </heighttonormal>
           </materialx>"#
    )
}

fn texture_coords(p: &Program) -> Vec<Option<u32>> {
    p.ops
        .iter()
        .filter_map(|op| match op {
            Op::Texture { coord, .. } => Some(*coord),
            _ => None,
        })
        .collect()
}

#[test]
fn heighttonormal_of_a_constant_coordinate_is_flat() {
    // Every tap reads the same texel, whatever the footprint.
    let doc = height_doc(
        r#"<constant name="c" type="vector2"><input name="value" type="vector2" value="0.5, 0.5" /></constant>"#,
    );
    let (n, _) = eval_with(&doc, "n", RampTex(10.0), 0.01);
    assert_eq!(n.rgb(), Vec3A::new(0.5, 0.5, 1.0));
}

#[test]
fn the_default_chart_spelled_out_is_the_implicit_one() {
    // `geompropvalue st` and `texcoord` are what `ctx.uv` already holds:
    // same answer as no connection, and still the JIT's inline lookup.
    let (want, p) = eval_with(&height_doc(""), "n", RampTex(10.0), 0.01);
    assert!(texture_coords(&p).iter().all(Option::is_none));
    for c in [
        r#"<geompropvalue name="c" type="vector2"><input name="geomprop" type="string" value="st" /></geompropvalue>"#,
        r#"<texcoord name="c" type="vector2" />"#,
    ] {
        let (got, p) = eval_with(&height_doc(c), "n", RampTex(10.0), 0.01);
        assert_eq!(bits_of(got), bits_of(want), "{c}");
        assert!(texture_coords(&p).iter().all(Option::is_none), "{c}");
    }
}

fn bits_of(v: Val) -> ([u32; 4], u8) {
    (v.v.map(f32::to_bits), v.arity)
}

#[test]
fn an_authored_coordinate_is_sampled_and_differentiated_through() {
    // `texcoord · 2`: the image reads at twice the chart, and the height
    // changes twice as fast across the same footprint.
    let doc = height_doc(
        r#"<texcoord name="t" type="vector2" />
           <multiply name="c" type="vector2">
             <input name="in1" type="vector2" nodename="t" />
             <input name="in2" type="float" value="2" />
           </multiply>"#,
    );
    let (h, _) = eval_with(&doc, "h", RampTex(10.0), 0.01);
    assert!((h.x() - 10.0 * 0.6).abs() < 1e-5, "height {}", h.x());
    let (n, _) = eval_with(&doc, "n", RampTex(10.0), 0.01);
    let du = 0.2f32; // 10 per unit, 2x the chart, 0.01 across
    let want = Vec3A::new(-du, 0.0, (1.0 - du * du).sqrt()).normalize() * 0.5 + Vec3A::splat(0.5);
    close(n.rgb(), want);
}

#[test]
fn an_uncompilable_coordinate_falls_back_to_the_chart() {
    // `place2d` has no operator here; its constant stand-in would pin the
    // lookup to one texel, so the chart is used instead (and reported).
    let doc = height_doc(r#"<place2d name="c" type="vector2" />"#);
    let (h, p) = eval_with(&doc, "h", RampTex(10.0), 0.01);
    assert!((h.x() - 3.0).abs() < 1e-5, "height {}", h.x());
    assert!(texture_coords(&p).iter().all(Option::is_none));
}

#[derive(Clone)]
struct Channels;
impl crate::Texture for Channels {
    fn eval(&self, _: f32, _: f32, _: f32) -> [f32; 4] {
        [0.25, 0.5, 0.75, 1.0]
    }
}

#[test]
fn colorcorrect_of_a_float_image_is_grey() {
    // A one-lane lookup keeps its file's other channels in lanes 1..3; the
    // correction must promote lane 0, not correct and expose the rest.
    let doc = r#"<materialx>
             <image name="m" type="float">
               <input name="file" type="filename" value="mask.tif" />
             </image>
             <colorcorrect name="cc" type="color3">
               <input name="in" type="float" nodename="m" />
               <input name="gain" type="float" value="2" />
             </colorcorrect>
             <convert name="out" type="color3">
               <input name="in" type="color3" nodename="cc" />
             </convert>
           </materialx>"#;
    for node in ["cc", "out"] {
        let (v, _) = eval_with(doc, node, Channels, 0.0);
        assert_eq!(v.arity, 3, "{node}");
        assert_eq!(v.rgb(), Vec3A::splat(0.5), "{node}");
    }
}

#[test]
fn heighttonormal_asks_the_host_for_its_image_once() {
    // Four shifted copies of the lookup, one sampler: the host's decode
    // and residency are per file, not per tap.
    let doc = Doc::parse(HEIGHT_DOC).unwrap();
    let calls = std::cell::Cell::new(0);
    let loader = |_: &str, _: Option<&str>| {
        calls.set(calls.get() + 1);
        Some(TextureRef(std::sync::Arc::new(Ramp {
            slope_u: 1.0,
            slope_v: 0.0,
        })))
    };
    let mut c = Compiler::new(&doc, &loader);
    c.compile_named("", "n", None);
    c.compile_named("", "h", None);
    assert_eq!(calls.get(), 1);
}

#[test]
fn division_by_zero_stays_finite() {
    // `1 / transmittance` with a black channel is authored in the teapot's
    // own ceramic graph; an infinity there survives every later multiply
    // and reaches the framebuffer as a NaN pixel.
    let v = run(
        r#"<materialx>
             <divide name="d" type="float">
               <input name="in1" type="float" value="1" />
               <input name="in2" type="float" value="0" />
             </divide>
           </materialx>"#,
        "d",
    );
    assert!(v.x().is_finite());
}
