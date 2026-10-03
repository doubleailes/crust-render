//! `open_pbr_surface`: `NG_open_pbr_surface_surfaceshader` (OpenPBR 1.1).

use super::B;
use crate::bsdf::{Bsdf, DiffuseModel, EdfFalloff, ScatterMode, SheenMode, Volume};
use crate::eval::{BinOp, UnOp};

pub(super) fn open_pbr_surface(b: &mut B<'_, '_>) {
    b.report(
        "transmission_dispersion_scale",
        "ignored, as MaterialX's own graph does",
    );

    let normal = b.geom("geometry_normal");
    let tangent = b.geom("geometry_tangent");
    let coat_normal = b.geom("geometry_coat_normal");
    let coat_tangent = b.geom("geometry_coat_tangent");

    // Roughness: the coat broadens the base specular.
    let coat_roughness = b.get("coat_roughness");
    let specular_roughness = b.get("specular_roughness");
    let coat_weight = b.get("coat_weight");
    let four = b.k(4.0);
    let cr4 = b.bin(BinOp::Pow, coat_roughness, four); // coat_roughness_to_power_4
    let two = b.k(2.0);
    let two_cr4 = b.mul(cr4, two); // two_times_coat_roughness_to_power_4
    let sr4 = b.bin(BinOp::Pow, specular_roughness, four); // specular_roughness_to_power_4
    let sum = b.add(two_cr4, sr4); // add_coat_and_spec_roughnesses_to_power_4
    let one = b.k(1.0);
    let min1 = b.bin(BinOp::Min, one, sum); // min_1_add_coat_and_spec_roughnesses_to_power_4
    let quarter = b.k(0.25);
    let coat_affected = b.bin(BinOp::Pow, min1, quarter); // coat_affected_specular_roughness
    let effective = b.mix(coat_affected, specular_roughness, coat_weight); // effective_specular_roughness
    let aniso = b.get("specular_roughness_anisotropy");
    let main_roughness = b.open_pbr_anisotropy(effective, aniso); // main_roughness

    // Subsurface, thin-walled and not, built only when it is live: a
    // literal-zero `subsurface_weight` (the default) makes `opaque_base` the
    // diffuse alone.
    let subsurface_color = b.get("subsurface_color");
    let zero = b.k(0.0);
    let diffuse_roughness = b.get("base_diffuse_roughness");
    let one_w = b.k(1.0);
    let base_color = b.get("base_color");
    let bcn = b.bin(BinOp::Max, base_color, zero); // base_color_nonnegative
    let base_weight = b.get("base_weight");
    let diffuse = b.leaf(
        Bsdf::Diffuse {
            model: DiffuseModel::Eon,
            color: bcn,
            roughness: diffuse_roughness,
        },
        base_weight,
        normal,
        None,
    ); // diffuse_bsdf
    let thin_walled = b.get("geometry_thin_walled");
    let subsurface_weight = b.get("subsurface_weight");
    let opaque_base = if b.is(subsurface_weight, 0.0) {
        diffuse
    } else {
        let ssc = b.bin(BinOp::Max, subsurface_color, zero); // subsurface_color_nonnegative
        let sss_refl_bsdf = b.leaf(
            Bsdf::Diffuse {
                model: DiffuseModel::OrenNayar,
                color: ssc,
                roughness: diffuse_roughness,
            },
            one_w,
            normal,
            None,
        ); // subsurface_thin_walled_reflection_bsdf
        let ss_aniso = b.get("subsurface_scatter_anisotropy");
        let one_minus_aniso = b.one_minus(ss_aniso); // one_minus_subsurface_scatter_anisotropy
        let brdf_factor = b.mul(subsurface_color, one_minus_aniso); // subsurface_thin_walled_brdf_factor
        let sss_refl = b.multiply(sss_refl_bsdf, brdf_factor); // subsurface_thin_walled_reflection
        let sss_trans_bsdf = b.leaf(Bsdf::Translucent { color: ssc }, one_w, normal, None); // subsurface_thin_walled_transmission_bsdf
        let one = b.k(1.0);
        let one_plus_aniso = b.add(one, ss_aniso); // one_plus_subsurface_scatter_anisotropy
        let btdf_factor = b.mul(subsurface_color, one_plus_aniso); // subsurface_thin_walled_btdf_factor
        let sss_trans = b.multiply(sss_trans_bsdf, btdf_factor); // subsurface_thin_walled_transmission
        let half = b.k(0.5);
        let sss_thin = b.mixc(sss_refl, sss_trans, half); // subsurface_thin_walled
        let radius_scale = b.get("subsurface_radius_scale");
        let radius = b.get("subsurface_radius");
        let radius_scaled = b.mul(radius_scale, radius); // subsurface_radius_scaled
        let sss_bsdf = b.leaf(
            Bsdf::Subsurface {
                color: ssc,
                radius: radius_scaled,
                anisotropy: ss_aniso,
            },
            one_w,
            normal,
            None,
        ); // subsurface_bsdf
        let selector = b.convert(thin_walled, 1); // subsurface_selector
        let selected = b.mixc(sss_thin, sss_bsdf, selector); // selected_subsurface
        b.mixc(selected, diffuse, subsurface_weight) // opaque_base
    };

    // The transmission volume.
    let transmission_color = b.get("transmission_color");
    let tcv = b.convert(transmission_color, 3); // transmission_color_vector
    let tcl = b.un(UnOp::Ln, tcv); // transmission_color_ln
    let minus_one = b.k(-1.0);
    let ext_den = b.mul(tcl, minus_one); // extinction_coeff_denom
    let depth = b.get("transmission_depth");
    let depth_v = b.convert(depth, 3); // transmission_depth_vector
    let extinction = b.div(ext_den, depth_v); // extinction_coeff
    let scatter = b.get("transmission_scatter");
    let scatter_v = b.convert(scatter, 3); // transmission_scatter_vector
    let scattering = b.div(scatter_v, depth_v); // scattering_coeff
    let absorption = b.sub(extinction, scattering); // absorption_coeff
    let ax = b.extract(absorption, 0);
    let ay = b.extract(absorption, 1);
    let az = b.extract(absorption, 2);
    let min_xy = b.bin(BinOp::Min, ax, ay);
    let amin = b.bin(BinOp::Min, min_xy, az); // absorption_coeff_min
    let amin_v = b.convert(amin, 3);
    let shifted = b.sub(absorption, amin_v); // absorption_coeff_shifted
    let if_shifted = b.gt(zero, amin, shifted, absorption); // if_absorption_coeff_shifted
    let zero3 = b.k3(0.0, 0.0, 0.0);
    let vol_abs = b.gt(depth, zero, if_shifted, zero3); // if_volume_absorption
    let vol_scat = b.gt(depth, zero, scattering, zero3); // if_volume_scattering
    let vol_aniso = b.get("transmission_scatter_anisotropy");
    let transmission_weight = b.get("transmission_weight");
    let one = b.k(1.0);
    if !b.is(transmission_weight, 0.0) {
        b.out.volume = Some(Volume {
            absorption: vol_abs,
            scattering: vol_scat,
            anisotropy: vol_aniso,
        }); // dielectric_volume
    }

    // The dielectric interface's IOR: relative to the coat, and modulated by
    // specular_weight through F0.
    let tf_thickness = b.get("thin_film_thickness");
    let thousand = b.k(1000.0);
    let tf_nm = b.mul(tf_thickness, thousand); // thin_film_thickness_nm
    let specular_ior = b.get("specular_ior");
    let coat_ior = b.get("coat_ior");
    let s2c = b.div(specular_ior, coat_ior); // specular_to_coat_ior_ratio
    let c2s = b.div(coat_ior, specular_ior); // coat_to_specular_ior_ratio
    let tir_fix = b.gt(s2c, one, s2c, c2s); // specular_to_coat_ior_ratio_tir_fix
    let eta_s = b.mix(tir_fix, specular_ior, coat_weight); // eta_s
    let em1 = b.sub(eta_s, one); // eta_s_minus_one
    let ep1 = b.add(eta_s, one); // eta_s_plus_one
    let f0_sqrt = b.div(em1, ep1); // specular_F0_sqrt
    let f0 = b.mul(f0_sqrt, f0_sqrt); // specular_F0
    let specular_weight = b.get("specular_weight");
    let scaled_f0 = b.mul(specular_weight, f0); // scaled_specular_F0
    let scaled_f0c = b.clamp(scaled_f0, 0.0, 0.99999); // scaled_specular_F0_clamped
    let sqrt_f0 = b.un(UnOp::Sqrt, scaled_f0c); // sqrt_scaled_specular_F0
    let sign = b.un(UnOp::Sign, em1); // sign_eta_s_minus_one
    let eps = b.mul(sign, sqrt_f0); // modulated_eta_s_epsilon
    let one_minus_eps = b.one_minus(eps); // one_minus_modulated_eta_s_epsilon
    let one_plus_eps = b.add(one, eps); // one_plus_modulated_eta_s_epsilon
    let modulated_eta = b.div(one_plus_eps, one_minus_eps); // modulated_eta_s

    // Transmission over the opaque base, then the reflection over that.
    let white = b.k3(1.0, 1.0, 1.0);
    let substrate = if b.is(transmission_weight, 0.0) {
        opaque_base
    } else {
        let t_tint = b.gt(depth, zero, white, transmission_color); // if_transmission_tint
        let d_trans = b.leaf(
            Bsdf::Dielectric {
                tint: t_tint,
                ior: modulated_eta,
                roughness: main_roughness,
                mode: ScatterMode::T,
                thin_film: None,
                abbe: None,
            },
            one_w,
            normal,
            tangent,
        ); // dielectric_transmission (+ dielectric_volume_transmission)
        b.mixc(d_trans, opaque_base, transmission_weight) // dielectric_substrate
    };
    let specular_color = b.get("specular_color");
    let d_refl = b.leaf(
        Bsdf::Dielectric {
            tint: specular_color,
            ior: modulated_eta,
            roughness: main_roughness,
            mode: ScatterMode::R,
            thin_film: None,
            abbe: None,
        },
        one_w,
        normal,
        tangent,
    ); // dielectric_reflection
    let tf_ior = b.get("thin_film_ior");
    let thin_film_weight = b.get("thin_film_weight");
    let d_refl_mix = if b.is(thin_film_weight, 0.0) {
        d_refl
    } else {
        let film = b.film(tf_nm, tf_ior);
        let d_refl_tf = b.leaf(
            Bsdf::Dielectric {
                tint: specular_color,
                ior: modulated_eta,
                roughness: main_roughness,
                mode: ScatterMode::R,
                thin_film: film,
                abbe: None,
            },
            one_w,
            normal,
            tangent,
        ); // dielectric_reflection_tf
        b.mixc(d_refl_tf, d_refl, thin_film_weight) // dielectric_reflection_tf_mix
    };
    let dielectric_base = b.layer(d_refl_mix, substrate); // dielectric_base

    // The metal.
    let metal_refl = b.mul(base_color, base_weight); // metal_reflectivity
    let metal_edge = b.mul(specular_color, specular_weight); // metal_edgecolor
    let five = b.k(5.0);
    let metalness = b.get("base_metalness");
    let metal_mix = if b.is(metalness, 0.0) {
        None
    } else {
        let metal = b.leaf(
            Bsdf::Schlick {
                color0: metal_refl,
                color82: metal_edge,
                color90: white,
                exponent: five,
                roughness: main_roughness,
                mode: ScatterMode::R,
                thin_film: None,
            },
            specular_weight,
            normal,
            tangent,
        ); // metal_bsdf
        if b.is(thin_film_weight, 0.0) {
            metal
        } else {
            let film = b.film(tf_nm, tf_ior);
            let metal_tf = b.leaf(
                Bsdf::Schlick {
                    color0: metal_refl,
                    color82: metal_edge,
                    color90: white,
                    exponent: five,
                    roughness: main_roughness,
                    mode: ScatterMode::R,
                    thin_film: film,
                },
                specular_weight,
                normal,
                tangent,
            ); // metal_bsdf_tf
            b.mixc(metal_tf, metal, thin_film_weight) // metal_bsdf_tf_mix
        }
    };
    let base_substrate = b.mixc(metal_mix, dielectric_base, metalness); // base_substrate

    // Coat darkening and tint.
    let coat_f0 = b.ior_to_f0(coat_ior); // coat_ior_to_F0
    let one_minus_coat_f0 = b.one_minus(coat_f0); // one_minus_coat_F0
    let coat_ior_sq = b.mul(coat_ior, coat_ior); // coat_ior_sqr
    let omf0_eta2 = b.div(one_minus_coat_f0, coat_ior_sq); // one_minus_coat_F0_over_eta2
    let k_coat = b.one_minus(omf0_eta2); // Kcoat
    let e_metal = b.mul(base_color, specular_weight); // Emetal
    let e_diel = b.mix(subsurface_color, base_color, subsurface_weight); // Edielectric
    let e_base = b.mix(e_metal, e_diel, metalness); // Ebase
    let ebk = b.mul(e_base, k_coat); // Ebase_Kcoat
    let one_minus_k = b.one_minus(k_coat); // one_minus_Kcoat
    let one_minus_ebk = b.sub(white, ebk); // one_minus_Ebase_Kcoat
    let omk3 = b.convert(one_minus_k, 3); // one_minus_Kcoat_color
    let darkening = b.div(omk3, one_minus_ebk); // base_darkening
    let coat_darkening = b.get("coat_darkening");
    let cwd = b.mul(coat_weight, coat_darkening); // coat_weight_times_coat_darkening
    let mod_dark = b.mix(darkening, white, cwd); // modulated_base_darkening
    let darkened = b.multiply(base_substrate, mod_dark); // darkened_base_substrate
    let coat_color = b.get("coat_color");
    let coat_att = b.mix(coat_color, white, coat_weight); // coat_attenuation
    let attenuated = b.multiply(darkened, coat_att); // coat_substrate_attenuated
    let coat_aniso = b.get("coat_roughness_anisotropy");
    let coat_rough_v = b.open_pbr_anisotropy(coat_roughness, coat_aniso); // coat_roughness_vector
    let coat = b.leaf(
        Bsdf::Dielectric {
            tint: white,
            ior: coat_ior,
            roughness: coat_rough_v,
            mode: ScatterMode::R,
            thin_film: None,
            abbe: None,
        },
        coat_weight,
        coat_normal,
        coat_tangent,
    ); // coat_bsdf
    let coat_layer = b.layer(coat, attenuated); // coat_layer
    let fuzz_color = b.get("fuzz_color");
    let fuzz_roughness = b.get("fuzz_roughness");
    let fuzz_weight = b.get("fuzz_weight");
    let fuzz = b.leaf(
        Bsdf::Sheen {
            color: fuzz_color,
            roughness: fuzz_roughness,
            mode: SheenMode::Zeltner,
        },
        fuzz_weight,
        normal,
        None,
    ); // fuzz_bsdf
    b.out.root = b.layer(fuzz, coat_layer); // fuzz_layer

    // Emission: uncoated, and through the coat's Fresnel.
    let emission_color = b.get("emission_color");
    let luminance = b.get("emission_luminance");
    let ew = b.mul(emission_color, luminance); // emission_weight
    let uncoated_w = b.one_minus(coat_weight);
    b.emit_edf(ew, uncoated_w, None); // uncoated_emission_edf (bg of emission_edf)
    let coated_w = b.mul(coat_color, coat_weight); // coat_tinted_emission_edf · mix
    let c0 = b.convert(one_minus_coat_f0, 3); // one_minus_coat_F0_color
    let falloff = EdfFalloff {
        color0: c0,
        color90: zero3,
        exponent: five,
    };
    b.emit_edf(ew, coated_w, Some(falloff)); // coated_emission_edf (fg)

    b.out.thin_walled = Some(thin_walled);
    let opacity = b.get("geometry_opacity");
    b.set_opacity(opacity); // shader_constructor.opacity
}
