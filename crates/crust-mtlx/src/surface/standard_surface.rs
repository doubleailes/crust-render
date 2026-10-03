//! `standard_surface`: `NG_standard_surface_surfaceshader_100`.

use super::B;
use crate::bsdf::{Bsdf, DiffuseModel, EdfFalloff, ScatterMode, SheenMode, Slot};
use crate::eval::{BinOp, Op};

pub(super) fn standard_surface(b: &mut B<'_, '_>) {
    for (name, why) in [
        (
            "transmission_depth",
            "ignored, as MaterialX's own graph does",
        ),
        (
            "transmission_scatter",
            "ignored, as MaterialX's own graph does",
        ),
        (
            "transmission_dispersion",
            "ignored, as MaterialX's own graph does",
        ),
    ] {
        b.report(name, why);
    }

    let normal = b.geom("normal");
    let coat_normal = b.geom("coat_normal");
    let tangent = b.geom("tangent");
    // `main_tangent` / `coat_tangent`: the tangent turned by a fraction of a
    // full turn about `normal` / `coat_normal`, only where the lobe is
    // anisotropic — the leaves' own normals, so a leaf rotation.
    let main_rotation = standard_rotation(b, "specular_rotation", "specular_anisotropy");
    let coat_rotation = standard_rotation(b, "coat_rotation", "coat_anisotropy");

    let coat_affect_roughness = b.get("coat_affect_roughness");
    let coat = b.get("coat");
    let coat_roughness = b.get("coat_roughness");
    let m1 = b.mul(coat_affect_roughness, coat); // coat_affect_roughness_multiply1
    let m2 = b.mul(m1, coat_roughness); // coat_affect_roughness_multiply2
    let specular_roughness = b.get("specular_roughness");
    let one = b.k(1.0);
    let coat_affected = b.mix(one, specular_roughness, m2); // coat_affected_roughness
    let specular_anisotropy = b.get("specular_anisotropy");
    let main_roughness = b.roughness_anisotropy(coat_affected, specular_anisotropy); // main_roughness
    let extra = b.get("transmission_extra_roughness");
    let tr_add = b.add(specular_roughness, extra); // transmission_roughness_add
    let tr_clamped = b.clamp(tr_add, 0.0, 1.0); // transmission_roughness_clamped
    let coat_affected_tr = b.mix(one, tr_clamped, m2); // coat_affected_transmission_roughness
    let transmission_roughness = b.roughness_anisotropy(coat_affected_tr, specular_anisotropy); // transmission_roughness

    let coat_clamped = b.clamp(coat, 0.0, 1.0); // coat_clamped
    let coat_affect_color = b.get("coat_affect_color");
    let cg_m = b.mul(coat_clamped, coat_affect_color); // coat_gamma_multiply
    let coat_gamma = b.add(cg_m, one); // coat_gamma
    let zero = b.k(0.0);
    let base_color = b.get("base_color");
    let bcn = b.bin(BinOp::Max, base_color, zero); // base_color_nonnegative
    let diffuse_color = b.bin(BinOp::Pow, bcn, coat_gamma); // coat_affected_diffuse_color
    let subsurface_color = b.get("subsurface_color");
    let scn = b.bin(BinOp::Max, subsurface_color, zero); // subsurface_color_nonnegative
    let sss_color = b.bin(BinOp::Pow, scn, coat_gamma); // coat_affected_subsurface_color

    let base = b.get("base");
    let diffuse_roughness = b.get("diffuse_roughness");
    let diffuse = b.leaf(
        Bsdf::Diffuse {
            model: DiffuseModel::OrenNayar,
            color: diffuse_color,
            roughness: diffuse_roughness,
        },
        base,
        normal,
        None,
    ); // diffuse_bsdf
    let subsurface = b.get("subsurface");
    let sss_mix = if b.is(subsurface, 0.0) {
        diffuse
    } else {
        let one_w = b.k(1.0);
        let translucent = b.leaf(Bsdf::Translucent { color: sss_color }, one_w, normal, None); // translucent_bsdf
        let radius = b.get("subsurface_radius");
        let scale = b.get("subsurface_scale");
        let radius_scaled = b.mul(radius, scale); // subsurface_radius_scaled
        let ss_aniso = b.get("subsurface_anisotropy");
        let sss = b.leaf(
            Bsdf::Subsurface {
                color: sss_color,
                radius: radius_scaled,
                anisotropy: ss_aniso,
            },
            one_w,
            normal,
            None,
        ); // subsurface_bsdf
        let thin_walled = b.get("thin_walled");
        let selector = b.convert(thin_walled, 1); // subsurface_selector
        let selected = b.mixc(translucent, sss, selector); // selected_subsurface_bsdf
        b.mixc(selected, diffuse, subsurface) // subsurface_mix
    };
    let sheen_w = b.get("sheen");
    let sheen_color = b.get("sheen_color");
    let sheen_roughness = b.get("sheen_roughness");
    let sheen = b.leaf(
        Bsdf::Sheen {
            color: sheen_color,
            roughness: sheen_roughness,
            mode: SheenMode::ContyKulla,
        },
        sheen_w,
        normal,
        None,
    ); // sheen_bsdf
    let sheen_layer = b.layer(sheen, sss_mix); // sheen_layer
    let transmission = b.get("transmission");
    let transmission_color = b.get("transmission_color");
    let specular_ior = b.get("specular_IOR");
    let one_w = b.k(1.0);
    let trans_mix = if b.is(transmission, 0.0) {
        sheen_layer
    } else {
        let trans = b.leaf(
            Bsdf::Dielectric {
                tint: transmission_color,
                ior: specular_ior,
                roughness: transmission_roughness,
                mode: ScatterMode::T,
                thin_film: None,
                abbe: None,
            },
            one_w,
            normal,
            tangent,
        ); // transmission_bsdf
        b.rotate(trans, main_rotation);
        b.mixc(trans, sheen_layer, transmission) // transmission_mix
    };
    let specular = b.get("specular");
    let specular_color = b.get("specular_color");
    let tf_thickness = b.get("thin_film_thickness");
    let tf_ior = b.get("thin_film_IOR");
    let film = b.film(tf_thickness, tf_ior);
    let spec = b.leaf(
        Bsdf::Dielectric {
            tint: specular_color,
            ior: specular_ior,
            roughness: main_roughness,
            mode: ScatterMode::R,
            thin_film: film,
            abbe: None,
        },
        specular,
        normal,
        tangent,
    ); // specular_bsdf
    b.rotate(spec, main_rotation);
    let spec_layer = b.layer(spec, trans_mix); // specular_layer
    let metalness = b.get("metalness");
    let metal_mix = if b.is(metalness, 0.0) {
        spec_layer
    } else {
        let metal_refl = b.mul(base_color, base); // metal_reflectivity
        let metal_edge = b.mul(specular_color, specular); // metal_edgecolor
        let n = b.c.emit(Op::ArtisticIor {
            reflectivity: metal_refl,
            edge: metal_edge,
            extinction: false,
        }); // artistic_ior.ior
        let k = b.c.emit(Op::ArtisticIor {
            reflectivity: metal_refl,
            edge: metal_edge,
            extinction: true,
        }); // artistic_ior.extinction
        let metal = b.leaf(
            Bsdf::Conductor {
                ior: n,
                extinction: k,
                roughness: main_roughness,
                thin_film: film,
            },
            one_w,
            normal,
            tangent,
        ); // metal_bsdf
        b.rotate(metal, main_rotation);
        b.mixc(metal, spec_layer, metalness) // metalness_mix
    };
    let coat_color = b.get("coat_color");
    let white = b.k3(1.0, 1.0, 1.0);
    let coat_att = b.mix(coat_color, white, coat); // coat_attenuation
    let attenuated = b.multiply(metal_mix, coat_att); // thin_film_layer_attenuated
    let coat_anisotropy = b.get("coat_anisotropy");
    let coat_rough_v = b.roughness_anisotropy(coat_roughness, coat_anisotropy); // coat_roughness_vector
    let coat_ior = b.get("coat_IOR");
    let coat_bsdf = b.leaf(
        Bsdf::Dielectric {
            tint: white,
            ior: coat_ior,
            roughness: coat_rough_v,
            mode: ScatterMode::R,
            thin_film: None,
            abbe: None,
        },
        coat,
        coat_normal,
        tangent,
    ); // coat_bsdf
    b.rotate(coat_bsdf, coat_rotation);
    b.out.root = b.layer(coat_bsdf, attenuated); // coat_layer

    // Emission, uncoated and through the coat's Fresnel.
    let coat_f0 = b.ior_to_f0(coat_ior); // coat_ior_to_F0
    let one_minus_f0 = b.one_minus(coat_f0); // one_minus_coat_ior_to_F0
    let emission_color = b.get("emission_color");
    let emission = b.get("emission");
    let ew = b.mul(emission_color, emission); // emission_weight
    let uncoated_w = b.one_minus(coat);
    b.emit_edf(ew, uncoated_w, None); // emission_edf (bg of blended_coat_emission_edf)
    let coated_w = b.mul(coat_color, coat); // coat_tinted_emission_edf · mix
    let c0 = b.convert(one_minus_f0, 3); // emission_color0
    let zero3 = b.k3(0.0, 0.0, 0.0);
    let five = b.k(5.0);
    b.emit_edf(
        ew,
        coated_w,
        Some(EdfFalloff {
            color0: c0,
            color90: zero3,
            exponent: five,
        }),
    ); // coat_emission_edf (fg)

    let thin_walled = b.get("thin_walled");
    b.out.thin_walled = Some(thin_walled);
    let opacity = b.get("opacity");
    let luminance = b.c.luminance(opacity); // opacity_luminance(_float)
    b.set_opacity(luminance); // shader_constructor.opacity
}

/// `standard_surface`'s `main_tangent` / `coat_tangent`: `rotate3d` by
/// `rotation · 360` degrees, selected by `ifgreater(anisotropy, 0)`.
fn standard_rotation(
    b: &mut B<'_, '_>,
    rotation: &'static str,
    anisotropy: &'static str,
) -> Option<Slot> {
    let r = b.get(rotation);
    let full_turn = b.k(360.0);
    let degrees = b.mul(r, full_turn); // tangent_rotate_degree
    let angle = b.tangent_rotation(degrees)?; // tangent_rotate
    let a = b.get(anisotropy);
    let zero = b.k(0.0);
    let selected = b.gt(a, zero, angle, zero); // main_tangent / coat_tangent
    (!b.is(selected, 0.0)).then_some(selected)
}
