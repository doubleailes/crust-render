//! `gltf_pbr`: `IMPL_gltf_pbr_surfaceshader` (glTF PBR 2.0.1).

use super::B;
use crate::bsdf::{Bsdf, DiffuseModel, ScatterMode, SheenMode, Slot, Volume};
use crate::eval::{BinOp, Op, UnOp};

pub(super) fn gltf_pbr(b: &mut B<'_, '_>) {
    b.report("occlusion", "a path tracer computes its own");
    b.report("dispersion", "ignored, as MaterialX's own graph does");
    b.report("thickness", "ignored, as MaterialX's own graph does");

    let normal = b.geom("normal");
    let tangent = b.geom("tangent");
    let clearcoat_normal = b.geom("clearcoat_normal");
    // `selected_tangent`: the tangent turned by `anisotropy_rotation` radians
    // about `normal`, for every base leaf; the clearcoat keeps `tangent`.
    // The graph's `ifgreater(|rotation|, 0)` needs no select here, since a
    // zero angle turns nothing.
    let aniso_rotation = b.get("anisotropy_rotation");
    let to_degrees = b.k(-57.29578);
    let degrees = b.mul(aniso_rotation, to_degrees); // rad_2_deg
    let rotation = b.tangent_rotation(degrees); // rotate_tangent

    // The volume.
    let transmission = b.get("transmission");
    if !b.is(transmission, 0.0) {
        let ac = b.get("attenuation_color");
        let acv = b.convert(ac, 3); // attenuation_color_vec
        let ln = b.un(UnOp::Ln, acv); // ln_attenuation_color_vec
        let dist = b.get("attenuation_distance");
        let zero = b.k(0.0);
        let one = b.k(1.0);
        let safe = b.gt(dist, zero, dist, one); // safe_attenuation_distance
        let over = b.div(ln, safe); // ln_attenuation_color_vec_over_distance
        let minus_one = b.k(-1.0);
        let coeff = b.mul(over, minus_one); // attenuation_coeff
        let zero3 = b.k3(0.0, 0.0, 0.0);
        let g = b.k(0.0);
        b.out.volume = Some(Volume {
            absorption: coeff,
            scattering: zero3,
            anisotropy: g,
        }); // isotropic_volume
    }

    // The dielectric's Fresnel as a generalized Schlick.
    let ior = b.get("ior");
    let f0_ior = b.ior_to_f0(ior); // dielectric_f0_from_ior
    let specular_color = b.get("specular_color");
    let f0_sc = b.mul(specular_color, f0_ior); // dielectric_f0_from_ior_specular_color
    let one = b.k(1.0);
    let f0_cl = b.bin(BinOp::Min, f0_sc, one); // clamped_dielectric_f0_from_ior_specular_color
    let specular = b.get("specular");
    let f0 = b.mul(f0_cl, specular); // dielectric_f0
    let white = b.k3(1.0, 1.0, 1.0);
    let f90 = b.mul(white, specular); // dielectric_f90

    // Roughness.
    let roughness = b.get("roughness");
    let alpha = b.mul(roughness, roughness); // alpha_roughness
    let strength = b.get("anisotropy_strength");
    let s2 = b.mul(strength, strength); // strength_2
    let at = b.mix(one, alpha, s2); // at
    let at_c = b.clamp(at, 0.00001, 1.0); // clamped_at
    let ab_c = b.clamp(alpha, 0.00001, 1.0); // clamped_ab
    let ruv = b.c.emit(Op::Combine2 { a: at_c, b: ab_c }); // roughness_uv

    let base_color = b.get("base_color");
    let zero = b.k(0.0);
    let one_w = b.k(1.0);
    let diffuse = b.leaf(
        Bsdf::Diffuse {
            model: DiffuseModel::OrenNayar,
            color: base_color,
            roughness: zero,
        },
        one_w,
        normal,
        None,
    ); // diffuse_bsdf
    let trans_mix = if b.is(transmission, 0.0) {
        diffuse
    } else {
        let trans = b.leaf(
            Bsdf::Dielectric {
                tint: base_color,
                ior,
                roughness: ruv,
                mode: ScatterMode::T,
                thin_film: None,
                abbe: None,
            },
            one_w,
            normal,
            tangent,
        ); // transmission_bsdf (+ volume_transmission_bsdf)
        b.rotate(trans, rotation);
        b.mixc(trans, diffuse, transmission) // transmission_mix
    };
    let five = b.k(5.0);
    let refl = b.leaf(
        Bsdf::Schlick {
            color0: f0,
            color82: white,
            color90: f90,
            exponent: five,
            roughness: ruv,
            mode: ScatterMode::R,
            thin_film: None,
        },
        one_w,
        normal,
        tangent,
    ); // reflection_bsdf
    b.rotate(refl, rotation);
    let iridescence = b.get("iridescence");
    let irid_thickness = b.get("iridescence_thickness");
    let irid_ior = b.get("iridescence_ior");
    let mix_irid = if b.is(iridescence, 0.0) {
        refl
    } else {
        let film = b.film(irid_thickness, irid_ior);
        let tf_refl = b.leaf(
            Bsdf::Schlick {
                color0: f0,
                color82: white,
                color90: f90,
                exponent: five,
                roughness: ruv,
                mode: ScatterMode::R,
                thin_film: film,
            },
            one_w,
            normal,
            tangent,
        ); // tf_reflection_bsdf
        b.rotate(tf_refl, rotation);
        b.mixc(tf_refl, refl, iridescence) // mix_iridescent_dielectric_reflection
    };
    let irid_diel = b.layer(mix_irid, trans_mix); // iridescent_dielectric_bsdf
    let metallic = b.get("metallic");
    let metal_mix = if b.is(metallic, 0.0) {
        None
    } else {
        let metal = b.leaf(
            Bsdf::Schlick {
                color0: base_color,
                color82: white,
                color90: white,
                exponent: five,
                roughness: ruv,
                mode: ScatterMode::R,
                thin_film: None,
            },
            one_w,
            normal,
            tangent,
        ); // metal_bsdf
        b.rotate(metal, rotation);
        if b.is(iridescence, 0.0) {
            metal
        } else {
            let film = b.film(irid_thickness, irid_ior);
            let tf_metal = b.leaf(
                Bsdf::Schlick {
                    color0: base_color,
                    color82: white,
                    color90: white,
                    exponent: five,
                    roughness: ruv,
                    mode: ScatterMode::R,
                    thin_film: film,
                },
                one_w,
                normal,
                tangent,
            ); // tf_metal_bsdf
            b.rotate(tf_metal, rotation);
            b.mixc(tf_metal, metal, iridescence) // mix_iridescent_metal_bsdf
        }
    };
    let base_mix = b.mixc(metal_mix, irid_diel, metallic); // base_mix

    let sheen_color = b.get("sheen_color");
    let r = b.extract(sheen_color, 0);
    let g = b.extract(sheen_color, 1);
    let bl = b.extract(sheen_color, 2);
    let max_rg = b.bin(BinOp::Max, r, g); // sheen_color_max_rg
    let intensity = b.bin(BinOp::Max, max_rg, bl); // sheen_intensity
    let sheen_roughness = b.get("sheen_roughness");
    let sheen_rough_sq = b.mul(sheen_roughness, sheen_roughness); // sheen_roughness_sq
    let sheen_norm = b.div(sheen_color, intensity); // sheen_color_normalized
    let sheen = b.leaf(
        Bsdf::Sheen {
            color: sheen_norm,
            roughness: sheen_rough_sq,
            mode: SheenMode::ContyKulla,
        },
        intensity,
        normal,
        None,
    ); // sheen_bsdf
    let sheen_layer = b.layer(sheen, base_mix); // sheen_layer

    let cc_roughness = b.get("clearcoat_roughness");
    let cc_rough_v = b.roughness_anisotropy(cc_roughness, zero); // clearcoat_roughness_uv
    let clearcoat = b.get("clearcoat");
    let cc_ior = b.k(1.5);
    let cc = b.leaf(
        Bsdf::Dielectric {
            tint: white,
            ior: cc_ior,
            roughness: cc_rough_v,
            mode: ScatterMode::R,
            thin_film: None,
            abbe: None,
        },
        clearcoat,
        clearcoat_normal,
        tangent,
    ); // clearcoat_bsdf
    b.out.root = b.layer(cc, sheen_layer); // clearcoat_layer

    let emissive = b.get("emissive");
    let strength_e = b.get("emissive_strength");
    let ec = b.mul(emissive, strength_e); // emission_color
    let one_e = b.k(1.0);
    b.emit_edf(ec, one_e, None); // emission

    // Alpha. `alpha_mode` is a uniform, so it folds, and the graph's two
    // `ifequal`s select one branch outright: OPAQUE never reads `alpha`,
    // which exporters connect to a texture's alpha whatever the mode, and
    // folding the select through that texture is not possible. Nor is it
    // compiled there: compiling it would load that texture, or report its
    // unsupported nodes, for an input the surface cannot show.
    let mode = b.get("alpha_mode");
    let folded = b.c.fold(mode).map(|v| v.x());
    if folded == Some(0.0) {
        return; // OPAQUE
    }
    let alpha = b.get("alpha");
    let opacity = match folded {
        Some(1.0) => alpha_mask(b, alpha), // MASK
        Some(_) => alpha,                  // BLEND
        None => {
            let masked = alpha_mask(b, alpha);
            let mask_mode = b.k(1.0);
            let mask = b.eq(mode, mask_mode, masked, alpha); // opacity_mask
            let (opaque_mode, one) = (b.k(0.0), b.k(1.0));
            b.eq(mode, opaque_mode, one, mask) // opacity
        }
    };
    b.set_opacity(opacity); // shader_constructor.opacity
}

/// glTF's `opacity_mask_cutoff`: 1 where `alpha ≥ alpha_cutoff`, else 0.
fn alpha_mask(b: &mut B<'_, '_>, alpha: Slot) -> Slot {
    let cutoff = b.get("alpha_cutoff");
    let (one, zero) = (b.k(1.0), b.k(0.0));
    b.ge(alpha, cutoff, one, zero)
}
