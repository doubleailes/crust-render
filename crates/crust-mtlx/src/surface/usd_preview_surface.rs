//! UsdPreviewSurface — IMP_UsdPreviewSurface_surfaceshader (UsdPreviewSurface 2.6)

use super::*;

pub(super) fn usd_preview_surface(b: &mut B<'_, '_>) {
    b.report("occlusion", "a path tracer computes its own");
    b.report("displacement", "ignored, as MaterialX's own graph does");

    let use_specular = b.get("useSpecularWorkflow"); // use_specular_workflow_float

    // `surface_normal`: `normalmap(normal · 0.5 + 0.5)`, which at the default
    // tangent-space `(0, 0, 1)` is the shading normal itself.
    let normal_in = b.get("normal");
    let untouched =
        b.c.fold(normal_in)
            .is_some_and(|v| v.v[..3] == [0.0, 0.0, 1.0]);
    let normal = if untouched {
        None
    } else {
        let half = b.k(0.5);
        let scaled = b.mul(normal_in, half); // scale_normal
        let biased = b.add(scaled, half); // bias_normal
        let one = b.k(1.0);
        Some(b.c.emit(Op::NormalMap {
            a: biased,
            scale: one,
        })) // surface_normal
    };

    // Diffuse layer.
    let metallic = b.get("metallic");
    let inverse_metalness = b.one_minus(metallic); // inverse_metalness
    let one = b.k(1.0);
    let diffuse_weight = b.mix(one, inverse_metalness, use_specular); // diffuse_bsdf_weight
    let diffuse_color = b.get("diffuseColor");
    let zero = b.k(0.0);
    let diffuse = b.leaf(
        Bsdf::Diffuse {
            model: DiffuseModel::OrenNayar,
            color: diffuse_color,
            roughness: zero,
        },
        diffuse_weight,
        normal,
        None,
    ); // diffuse_bsdf

    // Transmission layer: below an opacity of 1 the surface refracts, unless
    // an `opacityThreshold` makes it a cutout.
    let ior = b.get("ior");
    let opacity = b.get("opacity");
    let threshold = b.get("opacityThreshold");
    let amount = b.gt(threshold, zero, one, opacity); // transmission_mix_amount
    let transmission_mix = if b.is(amount, 1.0) {
        diffuse
    } else {
        let white = b.k3(1.0, 1.0, 1.0);
        let smooth = b.c.constant(Val::vec2(0.0, 0.0));
        let transmission = b.leaf(
            Bsdf::Dielectric {
                tint: white,
                ior,
                roughness: smooth,
                mode: ScatterMode::T,
                thin_film: None,
                abbe: None,
            },
            one,
            normal,
            None,
        ); // transmission_bsdf
        b.mixc(diffuse, transmission, amount) // transmission_mix
    };

    let roughness = b.get("roughness");
    let specular_roughness = b.roughness_anisotropy(roughness, zero); // specular_roughness
    let white = b.k3(1.0, 1.0, 1.0);
    let five = b.k(5.0);
    let schlick =
        |b: &mut B<'_, '_>, color0: Slot, color90: Slot, roughness: Slot, weight: Slot| {
            b.leaf(
                Bsdf::Schlick {
                    color0,
                    color82: white,
                    color90,
                    exponent: five,
                    roughness,
                    mode: ScatterMode::R,
                    thin_film: None,
                },
                weight,
                normal,
                None,
            )
        };

    // Specular workflow.
    let specular_workflow = if b.is(use_specular, 0.0) {
        None
    } else {
        let specular_color = b.get("specularColor");
        let spec = schlick(b, specular_color, white, specular_roughness, one); // specular_bsdf1
        b.layer(spec, transmission_mix) // specular_workflow_bsdf
    };

    // Metalness workflow. `R_sq` is the coat's F0 too, so it is built either way.
    let one_minus_ior = b.sub(one, ior); // one_minus_ior
    let one_plus_ior = b.add(one, ior); // one_plus_ior
    let r = b.div(one_minus_ior, one_plus_ior); // R
    let r_sq = b.mul(r, r); // R_sq
    let metalness_workflow = if b.is(use_specular, 1.0) {
        None
    } else {
        let scm = b.mix(diffuse_color, white, metallic); // specular_color_metallic
        let scm_r_sq = b.mul(scm, r_sq); // specular_color_metallic_R_sq
        let f0 = b.mix(scm, scm_r_sq, metallic); // F0
        let spec = schlick(b, f0, scm, specular_roughness, one); // specular_bsdf2
        let metalness_specular = b.layer(spec, transmission_mix); // metalness_specular_bsdf
        let metal = if b.is(metallic, 0.0) {
            None
        } else {
            let n = b.c.emit(Op::ArtisticIor {
                reflectivity: diffuse_color,
                edge: diffuse_color,
                extinction: false,
            }); // artistic_ior.ior
            let k = b.c.emit(Op::ArtisticIor {
                reflectivity: diffuse_color,
                edge: diffuse_color,
                extinction: true,
            }); // artistic_ior.extinction
            b.leaf(
                Bsdf::Conductor {
                    ior: n,
                    extinction: k,
                    roughness: specular_roughness,
                    thin_film: None,
                },
                one,
                normal,
                None,
            ) // metalness_metal_bsdf
        };
        b.mixc(metal, metalness_specular, metallic) // metalness_workflow_bsdf
    };
    let workflow = b.mixc(specular_workflow, metalness_workflow, use_specular); // workflow_selector_bsdf

    // Clearcoat layer.
    let coat_roughness_in = b.get("clearcoatRoughness");
    let coat_roughness = b.roughness_anisotropy(coat_roughness_in, zero); // coat_roughness
    let coat_f0 = b.convert(r_sq, 3); // coat_F0
    let clearcoat = b.get("clearcoat");
    let coat = schlick(b, coat_f0, white, coat_roughness, clearcoat); // coat_dielectric_bsdf
    b.out.root = b.layer(coat, workflow); // coat_bsdf

    let emissive = b.get("emissiveColor");
    b.emit_edf(emissive, one, None); // emission_edf

    // `opacityMode` is an integer, so it folds and the graph's `switch`
    // selects one branch outright; `switch` takes `in1` below a `which` of 1.
    //
    // At a threshold of 0 the cutout is `opacity ≥ 0`: 1 for every opacity
    // the input can mean, so it is taken as 1 rather than compiled. Compiled,
    // it would make every preview surface with a connected opacity a cutout
    // to the renderer, whose shadow rays then walk every hit (a negative
    // opacity, which the graph would cut out, is not honoured).
    let cutout = if b.is(threshold, 0.0) {
        one
    } else {
        b.ge(opacity, threshold, one, zero)
    }; // cutout_opacity
    let mode = b.get("opacityMode");
    let presence = |b: &mut B<'_, '_>| b.eq(opacity, zero, zero, cutout); // opacity_presence
    let selected = match b.c.fold(mode).map(|v| v.x()) {
        Some(m) if m < 1.0 => cutout,
        Some(_) => presence(b),
        None => {
            let p = presence(b);
            b.ge(mode, one, p, cutout)
        }
    }; // opacity_switch
    b.set_opacity(selected); // surface_constructor.opacity
}
