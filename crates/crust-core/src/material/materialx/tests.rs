use super::*;
use crate::material::Material;

#[test]
fn a_usd_reference_target_names_the_materialx_node() {
    assert_eq!(
        material_node_of("/MaterialX/Materials/surfacematerial_teapot_ceramic"),
        Some("surfacematerial_teapot_ceramic")
    );
    assert_eq!(material_node_of(""), None);
}

/// Writes `body` as a `.mtlx` and loads its first material.
fn load_inline(name: &str, body: &str) -> Result<Loaded, MtlxError> {
    let dir = std::env::temp_dir().join(format!("crust_mtlx_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.mtlx");
    std::fs::write(&path, format!("<materialx>{body}</materialx>")).unwrap();
    let loaded = load(&path, None, &crust_mtlx::Host::new(&|_, _| None));
    let _ = std::fs::remove_dir_all(&dir);
    loaded
}

fn upward() -> (Ray, HitRecord) {
    let mut rec = HitRecord::new();
    rec.p = Vec3A::ZERO;
    rec.normal = Vec3A::Z;
    rec.tangent = Vec3A::X;
    rec.front_face = true;
    (Ray::new(Vec3A::Z, -Vec3A::Z), rec)
}

/// A `surface` with an EDF built from `edf` (whose root node is `e`) and a
/// grey diffuse BSDF.
fn emitter(edf: &str) -> String {
    format!(
        r#"<oren_nayar_diffuse_bsdf name="d" type="BSDF" />
           {edf}
           <surface name="s" type="surfaceshader">
             <input name="bsdf" type="BSDF" nodename="d" />
             <input name="edf" type="EDF" nodename="e" />
           </surface>
           <surfacematerial name="m" type="material">
             <input name="surfaceshader" type="surfaceshader" nodename="s" />
           </surfacematerial>"#
    )
}

fn emission_of(name: &str, edf: &str) -> Vec3A {
    let loaded = load_inline(name, &emitter(edf)).expect("loads");
    let (r, rec) = upward();
    loaded.material.emitted_at(&r, &rec, 1.0)
}

#[test]
fn emission_terms_add_and_are_not_clamped() {
    let e = emission_of(
        "add",
        r#"<uniform_edf name="a" type="EDF"><input name="color" type="color3" value="3, 2, 1" /></uniform_edf>
           <uniform_edf name="b" type="EDF"><input name="color" type="color3" value="4, 4, 4" /></uniform_edf>
           <add name="e" type="EDF">
             <input name="in1" type="EDF" nodename="a" />
             <input name="in2" type="EDF" nodename="b" />
           </add>"#,
    );
    assert!(e.abs_diff_eq(Vec3A::new(7.0, 6.0, 5.0), 1e-5), "{e}");
}

#[test]
fn a_colour_weight_multiplies_emission_per_channel() {
    // Taking lane 0 of a `color3` weight would make this emitter black.
    let e = emission_of(
        "tint",
        r#"<uniform_edf name="u" type="EDF"><input name="color" type="color3" value="10, 10, 10" /></uniform_edf>
           <multiply name="e" type="EDF">
             <input name="in1" type="EDF" nodename="u" />
             <input name="in2" type="color3" value="0, 0.6, 0.9" />
           </multiply>"#,
    );
    assert!(e.abs_diff_eq(Vec3A::new(0.0, 6.0, 9.0), 1e-5), "{e}");
}

#[test]
fn a_bad_emission_channel_does_not_cost_the_others() {
    // A negative channel contributes nothing; the rest survive.
    let e = emission_of(
        "neg",
        r#"<uniform_edf name="u" type="EDF"><input name="color" type="color3" value="-1, 2, 3" /></uniform_edf>
           <multiply name="e" type="EDF">
             <input name="in1" type="EDF" nodename="u" />
             <input name="in2" type="color3" value="-1, 1, 1" />
           </multiply>"#,
    );
    assert!(e.abs_diff_eq(Vec3A::new(0.0, 2.0, 3.0), 1e-5), "{e}");
}

#[test]
fn a_pure_edf_surface_does_not_also_reflect() {
    let loaded = load_inline(
        "pure",
        r#"<uniform_edf name="e" type="EDF"><input name="color" type="color3" value="2, 2, 2" /></uniform_edf>
           <surface name="s" type="surfaceshader"><input name="edf" type="EDF" nodename="e" /></surface>
           <surfacematerial name="m" type="material">
             <input name="surfaceshader" type="surfaceshader" nodename="s" />
           </surfacematerial>"#,
    )
    .expect("loads");
    let (r, rec) = upward();
    assert_eq!(loaded.material.emitted_at(&r, &rec, 1.0), Vec3A::splat(2.0));
    assert!(
        loaded.material.eval(&r, &rec, Vec3A::Z).is_none(),
        "no BSDF leaf"
    );
    // And never a light-list entry: the hit-free emission stays zero.
    assert_eq!(loaded.material.emitted(), Vec3A::ZERO);
}

#[test]
fn a_tree_above_capacity_is_refused_not_truncated() {
    // Nine diffuse leaves chained through `add`.
    let mut body = String::new();
    for i in 0..9 {
        body += &format!(r#"<oren_nayar_diffuse_bsdf name="d{i}" type="BSDF" />"#);
    }
    body += r#"<add name="a1" type="BSDF"><input name="in1" type="BSDF" nodename="d0" /><input name="in2" type="BSDF" nodename="d1" /></add>"#;
    for i in 2..9 {
        body += &format!(
            r#"<add name="a{i}" type="BSDF"><input name="in1" type="BSDF" nodename="a{}" /><input name="in2" type="BSDF" nodename="d{i}" /></add>"#,
            i - 1
        );
    }
    body += r#"<surface name="s" type="surfaceshader"><input name="bsdf" type="BSDF" nodename="a8" /></surface>
               <surfacematerial name="m" type="material"><input name="surfaceshader" type="surfaceshader" nodename="s" /></surfacematerial>"#;
    let err = load_inline("cap", &body).err().expect("refused");
    assert!(err.to_string().contains("9 BSDF leaves"), "{err}");
}

#[test]
fn every_scatter_agrees_with_the_resolved_closure() {
    // The trait methods and a `ShadingPoint`'s resolution answer alike.
    let loaded = load_inline(
        "agree",
        r#"<dielectric_bsdf name="g" type="BSDF"><input name="roughness" type="vector2" value="0.2, 0.2" /></dielectric_bsdf>
           <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
           <layer name="l" type="BSDF"><input name="top" type="BSDF" nodename="g" /><input name="base" type="BSDF" nodename="d" /></layer>
           <surface name="s" type="surfaceshader"><input name="bsdf" type="BSDF" nodename="l" /></surface>
           <surfacematerial name="m" type="material"><input name="surfaceshader" type="surfaceshader" nodename="s" /></surfacematerial>"#,
    )
    .expect("loads");
    let (r, rec) = upward();
    let wi = Vec3A::new(0.3, 0.1, 0.9).normalize();
    let direct = loaded.material.eval(&r, &rec, wi).unwrap();
    let res = loaded.material.resolve(&r, &rec, 1.0).unwrap();
    let via = res.closure_bsdf().unwrap().eval(&r, &rec, wi).unwrap();
    assert_eq!(direct, via);
}

#[test]
fn a_schlick_edf_falls_off_with_the_exit_cosine() {
    // mix(color0, color90, (1 - cos)^exponent) times the base.
    let edf = r#"<uniform_edf name="u" type="EDF"><input name="color" type="color3" value="4, 4, 4" /></uniform_edf>
        <generalized_schlick_edf name="e" type="EDF">
          <input name="color0" type="color3" value="1, 0.5, 0.25" />
          <input name="color90" type="color3" value="0, 0, 0" />
          <input name="exponent" type="float" value="2" />
          <input name="base" type="EDF" nodename="u" />
        </generalized_schlick_edf>"#;
    let loaded = load_inline("schlick", &emitter(edf)).expect("loads");
    let (r, rec) = upward();
    let head_on = loaded.material.emitted_at(&r, &rec, 1.0);
    assert!(
        head_on.abs_diff_eq(Vec3A::new(4.0, 2.0, 1.0), 1e-5),
        "{head_on}"
    );
    // cos = 0.6: the blend weight is 0.4^2 = 0.16.
    let grazing = loaded.material.emitted_at(&r, &rec, 0.6);
    let want = Vec3A::new(4.0, 2.0, 1.0) * 0.84;
    assert!(grazing.abs_diff_eq(want, 1e-4), "{grazing} vs {want}");
}

/// A displacement graph over the vertex's position, normal and chart.
const DISPLACED: &str = r#"
    <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
    <surface name="s" type="surfaceshader"><input name="bsdf" type="BSDF" nodename="d" /></surface>
    <position name="p" type="vector3" />
    <normal name="n" type="vector3" />
    <texcoord name="t" type="vector2" />
    <dotproduct name="pn" type="float">
      <input name="in1" type="vector3" nodename="p" /><input name="in2" type="vector3" nodename="n" />
    </dotproduct>
    <extract name="u" type="float">
      <input name="in" type="vector2" nodename="t" /><input name="index" type="integer" value="0" />
    </extract>
    <sin name="w" type="float"><input name="in" type="float" nodename="pn" /></sin>
    <multiply name="h" type="float">
      <input name="in1" type="float" nodename="w" /><input name="in2" type="float" nodename="u" />
    </multiply>
    <displacement name="disp" type="displacementshader">
      <input name="displacement" type="float" nodename="h" />
      <input name="scale" type="float" value="0.25" />
    </displacement>
    <surfacematerial name="m" type="material">
      <input name="surfaceshader" type="surfaceshader" nodename="s" />
      <input name="displacementshader" type="displacementshader" nodename="disp" />
    </surfacematerial>"#;

fn vertex(i: usize) -> crate::VertexCtx {
    let f = i as f32 * 0.37;
    crate::VertexCtx {
        uv: Some([f.fract(), (f * 1.3).fract()]),
        uv_width: 0.01,
        ptex: None,
        ptex_width: 0.0,
        position: Vec3A::new(f.sin() * 3.0, f.cos(), f * 0.1),
        normal: Vec3A::new(f.cos(), 0.5, f.sin()).normalize(),
    }
}

/// The displacement program evaluates the graph at the vertex: `scale ·
/// sin(p · n) · u`, from the local position, normal and owner chart.
#[test]
fn a_materialx_displacement_evaluates_at_the_vertex() {
    let loaded = load_inline("displaced", DISPLACED).expect("loads");
    let d = loaded.displacement.expect("a displacement");
    for i in 0..64 {
        let v = vertex(i);
        let want = 0.25 * v.position.dot(v.normal).sin() * v.uv.unwrap()[0];
        let got = d.eval_interpreted(&v);
        assert!((got - want).abs() < 1e-5, "vertex {i}: {got} vs {want}");
    }
}

/// The JIT build of the displacement root matches the interpreter bit for
/// bit, the pin every other MaterialX program carries.
#[cfg(feature = "jit")]
#[test]
fn the_jit_displacement_matches_the_interpreter() {
    use crate::VertexField;
    let loaded = load_inline("displaced_jit", DISPLACED).expect("loads");
    let d = loaded.displacement.expect("a displacement");
    if !crate::config().shader_jit {
        return;
    }
    assert!(d.is_jit(), "the displacement program is JIT-compiled");
    for i in 0..256 {
        let v = vertex(i);
        assert_eq!(
            d.eval(&v).to_bits(),
            d.eval_interpreted(&v).to_bits(),
            "vertex {i}"
        );
    }
}
