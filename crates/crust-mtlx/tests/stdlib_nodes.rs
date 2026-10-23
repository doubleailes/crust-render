//! The standard nodes a studio node library is built from — `separate2/3/4`,
//! `switch`, the compositing nodes, `premult`, `place2d`, `transformmatrix` —
//! checked against MaterialX 1.39's own definitions: each expected value is
//! the stdlib's expression (its genosl `sourcecode`, `mx_*.osl` function or
//! `NG_*` graph), written out here in the same order of operations.
//!
//! These are not yet in the OSL oracle's fixture (`scripts/osl_oracle.py`
//! lists them; regenerating needs an exact-math OSL build), so they are
//! pinned here by hand. `transformmatrix` never will be: genosl's
//! `transform(matrix, vector)` drops the translation its GLSL applies.

use crust_mtlx::{Compiler, Doc, Host, ShadeCtx, TextureRef, Val};
use glam::Vec3A;

fn decline(_: &str, _: crust_mtlx::TexelSpace<'_>) -> Option<TextureRef> {
    None
}

fn ctx(u: f32, v: f32) -> ShadeCtx<'static> {
    ShadeCtx {
        uv: (u, v),
        normal: Vec3A::Z,
        tangent: Vec3A::X,
        view: -Vec3A::Z,
        position: Vec3A::new(1.0, 2.0, 3.0),
        uv_width: 0.0,
        curve: None,
        primvars: &[],
    }
}

/// `name` (selecting `output`) of a document whose body is `body`, at
/// `(u, v)`, and the categories reported unsupported.
fn eval_full(body: &str, name: &str, output: Option<&str>, u: f32, v: f32) -> (Val, Vec<String>) {
    let doc = Doc::parse(&format!("<materialx>{body}</materialx>")).expect("parses");
    let host = Host::new(&decline);
    let mut c = Compiler::new(&doc, &host);
    let slot = c.compile_named("", name, output);
    let mut slots = Vec::new();
    c.program.eval(&ctx(u, v), &mut slots);
    (
        slots[slot as usize],
        c.unsupported.iter().cloned().collect(),
    )
}

fn eval(body: &str, name: &str) -> [f32; 4] {
    let (v, unsupported) = eval_full(body, name, None, 0.25, 0.75);
    assert!(unsupported.is_empty(), "{unsupported:?}");
    v.v
}

fn eval_out(body: &str, name: &str, output: &str) -> f32 {
    let (v, unsupported) = eval_full(body, name, Some(output), 0.25, 0.75);
    assert!(unsupported.is_empty(), "{unsupported:?}");
    v.x()
}

fn close(a: &[f32], b: &[f32]) {
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert!(
            (x - y).abs() <= 1e-6 * (1.0 + y.abs()),
            "lane {i}: {a:?} vs {b:?}"
        );
    }
}

// ---- separate -------------------------------------------------------------

#[test]
fn separate_reads_one_lane_per_output() {
    let c3 = r#"<separate3 name="s" type="multioutput"><input name="in" type="color3" value="0.1, 0.2, 0.3" /></separate3>"#;
    assert_eq!(eval_out(c3, "s", "outr"), 0.1);
    assert_eq!(eval_out(c3, "s", "outg"), 0.2);
    assert_eq!(eval_out(c3, "s", "outb"), 0.3);
    let v3 = r#"<separate3 name="s" type="multioutput"><input name="in" type="vector3" value="4, 5, 6" /></separate3>"#;
    assert_eq!(eval_out(v3, "s", "outz"), 6.0);
    let c4 = r#"<separate4 name="s" type="multioutput"><input name="in" type="color4" value="0.1, 0.2, 0.3, 0.4" /></separate4>"#;
    assert_eq!(eval_out(c4, "s", "outa"), 0.4);
    let v4 = r#"<separate4 name="s" type="multioutput"><input name="in" type="vector4" value="1, 2, 3, 4" /></separate4>"#;
    assert_eq!(eval_out(v4, "s", "outw"), 4.0);
    let v2 = r#"<separate2 name="s" type="multioutput"><input name="in" type="vector2" value="7, 8" /></separate2>"#;
    assert_eq!(eval_out(v2, "s", "outy"), 8.0);
}

// ---- switch ---------------------------------------------------------------

fn switch_doc(which: &str) -> String {
    let ins: String = (1..=10)
        .map(|k| format!(r#"<input name="in{k}" type="float" value="{}" />"#, k * 10))
        .collect();
    format!(r#"<switch name="s" type="float">{ins}{which}</switch>"#)
}

/// `NG_switch_*`'s chain: `in1` below 1, … `in10` below 10, zero from 10.
fn switch_ref(which: f32) -> f32 {
    (1..=10)
        .find(|&k| k as f32 > which)
        .map_or(0.0, |k| (k * 10) as f32)
}

#[test]
fn a_literal_which_selects_as_the_stdlib_chain_does() {
    for w in [-2.0, 0.0, 0.5, 1.0, 3.0, 9.0, 9.99, 10.0, 12.0] {
        let doc = switch_doc(&format!(
            r#"<input name="which" type="float" value="{w}" />"#
        ));
        assert_eq!(eval(&doc, "s")[0], switch_ref(w), "which = {w}");
    }
    // Unauthored, `which` is 0: `in1`.
    assert_eq!(eval(&switch_doc(""), "s")[0], 10.0);
    // An unauthored input is zero.
    let sparse = r#"<switch name="s" type="color4"><input name="which" type="integer" value="4" /></switch>"#;
    assert_eq!(eval(sparse, "s"), [0.0; 4]);
}

#[test]
fn a_varying_which_selects_the_same_way() {
    let doc = format!(
        r#"<texcoord name="uv" type="vector2" />
           <extract name="u" type="float"><input name="in" type="vector2" nodename="uv" /><input name="index" type="integer" value="0" /></extract>
           <multiply name="w" type="float"><input name="in1" type="float" nodename="u" /><input name="in2" type="float" value="12" /></multiply>
           {}"#,
        switch_doc(r#"<input name="which" type="float" nodename="w" />"#)
    );
    for u in [0.0, 0.05, 0.1, 0.4, 0.75, 0.8, 0.9, 1.0] {
        let (v, unsupported) = eval_full(&doc, "s", None, u, 0.0);
        assert!(unsupported.is_empty());
        assert_eq!(v.x(), switch_ref(u * 12.0), "u = {u}");
    }
}

// ---- compositing ----------------------------------------------------------

const FG: [f32; 4] = [0.6, 0.3, 0.9, 0.25];
const BG: [f32; 4] = [0.2, 0.8, 0.4, 0.5];
const MIX: f32 = 0.7;

fn composite(category: &str, mix: Option<f32>) -> [f32; 4] {
    let m = mix.map_or(String::new(), |m| {
        format!(r#"<input name="mix" type="float" value="{m}" />"#)
    });
    eval(
        &format!(
            r#"<{category} name="c" type="color4">
                 <input name="fg" type="color4" value="0.6, 0.3, 0.9, 0.25" />
                 <input name="bg" type="color4" value="0.2, 0.8, 0.4, 0.5" />
                 {m}
               </{category}>"#
        ),
        "c",
    )
}

fn per_lane(f: impl Fn(f32, f32) -> f32) -> [f32; 4] {
    std::array::from_fn(|i| f(FG[i], BG[i]))
}

#[test]
fn over_in_out_difference_are_the_genosl_expressions() {
    let m = MIX;
    // IM_over_color4: (fg + bg*(1-fg.a)) * mix + bg * (1-mix).
    let over = per_lane(|f, b| (f + b * (1.0 - FG[3])) * m + b * (1.0 - m));
    close(&composite("over", Some(m)), &over);
    // IM_in_color4: fg*bg.a*mix + bg*(1-mix).
    let inside = per_lane(|f, b| f * BG[3] * m + b * (1.0 - m));
    close(&composite("in", Some(m)), &inside);
    // IM_out_color4: fg*(1-bg.a)*mix + bg*(1-mix).
    let outside = per_lane(|f, b| f * (1.0 - BG[3]) * m + b * (1.0 - m));
    close(&composite("out", Some(m)), &outside);
    // IM_difference_color4: mix*abs(bg-fg) + (1-mix)*bg.
    let diff = per_lane(|f, b| m * (b - f).abs() + (1.0 - m) * b);
    close(&composite("difference", Some(m)), &diff);
    // `mix` defaults to 1: no `bg` term.
    let over1 = per_lane(|f, b| f + b * (1.0 - FG[3]));
    close(&composite("over", None), &over1);
}

#[test]
fn difference_also_reads_float_and_color3() {
    let f = r#"<difference name="d" type="float"><input name="fg" type="float" value="0.2" /><input name="bg" type="float" value="0.9" /><input name="mix" type="float" value="0.5" /></difference>"#;
    close(
        &eval(f, "d")[..1],
        &[0.5 * (0.9f32 - 0.2).abs() + 0.5 * 0.9],
    );
}

#[test]
fn premult_scales_rgb_by_alpha() {
    let p = r#"<premult name="p" type="color4"><input name="in" type="color4" value="0.8, 0.4, 0.2, 0.5" /></premult>"#;
    assert_eq!(eval(p, "p"), [0.4, 0.2, 0.1, 0.5]);
    // Unauthored, `in` is (0, 0, 0, 1).
    let d = r#"<premult name="p" type="color4" />"#;
    assert_eq!(eval(d, "p"), [0.0, 0.0, 0.0, 1.0]);
}

// ---- place2d --------------------------------------------------------------

/// `mx_rotate_vector2`.
fn rotate(v: [f32; 2], degrees: f32) -> [f32; 2] {
    let r = degrees * (std::f32::consts::PI / 180.0);
    let (sa, ca) = (r.sin(), r.cos());
    [ca * v[0] + sa * v[1], -sa * v[0] + ca * v[1]]
}

fn place2d(order: &str) -> [f32; 4] {
    eval(
        &format!(
            r#"<place2d name="p" type="vector2">
                 <input name="texcoord" type="vector2" value="0.3, 0.8" />
                 <input name="pivot" type="vector2" value="0.5, 0.5" />
                 <input name="scale" type="vector2" value="2, 0.5" />
                 <input name="rotate" type="float" value="30" />
                 <input name="offset" type="vector2" value="0.1, -0.2" />
                 {order}
               </place2d>"#
        ),
        "p",
    )
}

#[test]
fn place2d_applies_srt_or_trs_as_its_nodegraph_does() {
    let (tc, pivot, scale, rot, offset) = (
        [0.3f32, 0.8],
        [0.5f32, 0.5],
        [2.0f32, 0.5],
        30.0,
        [0.1f32, -0.2],
    );
    let sub = [tc[0] - pivot[0], tc[1] - pivot[1]];
    // SRT: rotate((tc - pivot) / scale) - offset + pivot.
    let r = rotate([sub[0] / scale[0], sub[1] / scale[1]], rot);
    let srt = [r[0] - offset[0] + pivot[0], r[1] - offset[1] + pivot[1]];
    close(&place2d("")[..2], &srt);
    close(
        &place2d(r#"<input name="operationorder" type="integer" value="0" />"#)[..2],
        &srt,
    );
    // TRS: rotate(tc - pivot - offset) / scale + pivot.
    let r = rotate([sub[0] - offset[0], sub[1] - offset[1]], rot);
    let trs = [r[0] / scale[0] + pivot[0], r[1] / scale[1] + pivot[1]];
    close(
        &place2d(r#"<input name="operationorder" type="integer" value="1" />"#)[..2],
        &trs,
    );
    // Unauthored, it is the identity on the coordinate.
    let id = r#"<place2d name="p" type="vector2"><input name="texcoord" type="vector2" value="0.3, 0.8" /></place2d>"#;
    close(&eval(id, "p")[..2], &tc);
}

// ---- transformmatrix ------------------------------------------------------

/// A row vector times a row-major `n`×`n` matrix.
fn row_times(v: &[f32], m: &[f32], n: usize) -> Vec<f32> {
    (0..n)
        .map(|c| (0..n).fold(0.0, |acc, r| acc + v[r] * m[r * n + c]))
        .collect()
}

/// A 30° turn about Y and a translation of (12, −5, 3), row-major.
const M44: &str = "0.8660254, 0, -0.5, 0, 0, 1, 0, 0, 0.5, 0, 0.8660254, 0, 12, -5, 3, 1";

fn m44() -> Vec<f32> {
    M44.split(',').map(|x| x.trim().parse().unwrap()).collect()
}

#[test]
fn a_vector3_by_a_matrix44_is_a_point_translated_by_the_last_row() {
    let doc = format!(
        r#"<constant name="cam" type="matrix44"><input name="value" type="matrix44" value="{M44}" /></constant>
           <transformmatrix name="t" type="vector3">
             <input name="in" type="vector3" value="4, 2, -6" />
             <input name="mat" type="matrix44" nodename="cam" />
           </transformmatrix>"#
    );
    let want = row_times(&[4.0, 2.0, -6.0, 1.0], &m44(), 4);
    let got = eval(&doc, "t");
    for i in 0..3 {
        assert!((got[i] - want[i]).abs() < 1e-3, "{got:?} vs {want:?}");
    }
    // A literal matrix is the same matrix.
    let literal = format!(
        r#"<transformmatrix name="t" type="vector3">
             <input name="in" type="vector3" value="4, 2, -6" />
             <input name="mat" type="matrix44" value="{M44}" />
           </transformmatrix>"#
    );
    assert_eq!(eval(&literal, "t"), got);
}

#[test]
fn the_other_signatures_and_the_identity_default() {
    // vector4 by matrix44: no homogeneous coordinate added.
    let v4 = format!(
        r#"<transformmatrix name="t" type="vector4">
             <input name="in" type="vector4" value="1, 2, 3, 0" />
             <input name="mat" type="matrix44" value="{M44}" />
           </transformmatrix>"#
    );
    close(
        &eval(&v4, "t"),
        &row_times(&[1.0, 2.0, 3.0, 0.0], &m44(), 4),
    );
    // vector3 by matrix33, and vector2 by matrix33 (homogeneous).
    let m33 = [0.0f32, 1.0, 0.0, -1.0, 0.0, 0.0, 0.5, 0.25, 1.0];
    let text = m33.map(|x| x.to_string()).join(", ");
    let v3 = format!(
        r#"<transformmatrix name="t" type="vector3"><input name="in" type="vector3" value="2, 3, 4" /><input name="mat" type="matrix33" value="{text}" /></transformmatrix>"#
    );
    close(&eval(&v3, "t")[..3], &row_times(&[2.0, 3.0, 4.0], &m33, 3));
    let v2 = format!(
        r#"<transformmatrix name="t" type="vector2"><input name="in" type="vector2" value="2, 3" /><input name="mat" type="matrix33" value="{text}" /></transformmatrix>"#
    );
    close(
        &eval(&v2, "t")[..2],
        &row_times(&[2.0, 3.0, 1.0], &m33, 3)[..2],
    );
    // Unauthored, the matrix is the identity.
    let id = r#"<transformmatrix name="t" type="vector3"><input name="in" type="vector3" value="2, 3, 4" /></transformmatrix>"#;
    assert_eq!(&eval(id, "t")[..3], &[2.0, 3.0, 4.0]);
}

#[test]
fn a_matrix_that_is_not_a_constant_is_reported_and_passes_the_input_through() {
    let doc = r#"<texcoord name="uv" type="vector2" />
                 <transformmatrix name="t" type="vector3">
                   <input name="in" type="vector3" value="2, 3, 4" />
                   <input name="mat" type="matrix44" nodename="uv" />
                 </transformmatrix>"#;
    let (v, unsupported) = eval_full(doc, "t", None, 0.0, 0.0);
    assert_eq!(&v.v[..3], &[2.0, 3.0, 4.0]);
    assert_eq!(
        unsupported,
        ["transformmatrix (a matrix that is not a constant)"]
    );
}

#[test]
fn a_position_in_another_space_is_reported() {
    let world = r#"<position name="p" type="vector3"><input name="space" type="string" value="world" /></position>"#;
    assert_eq!(&eval(world, "p")[..3], &[1.0, 2.0, 3.0]);
    let object = r#"<position name="p" type="vector3"><input name="space" type="string" value="object" /></position>"#;
    let (_, unsupported) = eval_full(object, "p", None, 0.0, 0.0);
    assert_eq!(unsupported, [r#"position space="object""#]);
}
