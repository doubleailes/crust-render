//! The POD structs of the C contract and their mapping onto engine types.

use crate::status::CrustStatus;
use crate::validate::{CResult, MAX_DIM};
use crust_core::{OpenPBR, RenderSettings, Vec3A};
use std::mem::size_of;

/// Mirrors `CrustMaterial` in `crust.h`: a portable subset of the OpenPBR
/// übershader. Field-for-field POD; size pinned on both sides.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CrustMaterial {
    pub base_color: [f32; 3],
    pub metalness: f32,
    pub roughness: f32,
    pub ior: f32,
    pub transmission: f32,
    pub opacity: f32,
    pub emission_color: [f32; 3],
    pub emission_luminance: f32,
    pub coat_weight: f32,
    pub coat_roughness: f32,
    pub thin_walled: i32,
    pub reserved: i32,
}

const _: () = assert!(size_of::<CrustMaterial>() == 64, "CrustMaterial ABI drift");

impl CrustMaterial {
    /// The header's default: the OpenPBR default surface (grey diffuse).
    fn defaults() -> Self {
        let pbr = OpenPBR::default();
        CrustMaterial {
            base_color: pbr.base_color.to_array(),
            metalness: pbr.base_metalness,
            roughness: pbr.specular_roughness,
            ior: pbr.specular_ior,
            transmission: pbr.transmission_weight,
            opacity: pbr.geometry_opacity,
            emission_color: pbr.emission_color.to_array(),
            emission_luminance: pbr.emission_luminance,
            coat_weight: pbr.coat_weight,
            coat_roughness: pbr.coat_roughness,
            thin_walled: pbr.geometry_thin_walled as i32,
            reserved: 0,
        }
    }

    pub(crate) fn to_openpbr(self) -> CResult<OpenPBR> {
        let all = [
            self.base_color[0],
            self.base_color[1],
            self.base_color[2],
            self.metalness,
            self.roughness,
            self.ior,
            self.transmission,
            self.opacity,
            self.emission_color[0],
            self.emission_color[1],
            self.emission_color[2],
            self.emission_luminance,
            self.coat_weight,
            self.coat_roughness,
        ];
        if !all.iter().all(|v| v.is_finite()) {
            return Err(CrustStatus::InvalidArgument);
        }
        let mut pbr = OpenPBR::default();
        pbr.base_color = Vec3A::from_array(self.base_color);
        pbr.base_metalness = self.metalness.clamp(0.0, 1.0);
        pbr.specular_roughness = self.roughness.clamp(0.0, 1.0);
        pbr.specular_ior = self.ior.max(1.0);
        pbr.transmission_weight = self.transmission.clamp(0.0, 1.0);
        pbr.geometry_opacity = self.opacity.clamp(0.0, 1.0);
        pbr.emission_color = Vec3A::from_array(self.emission_color);
        pbr.emission_luminance = self.emission_luminance.max(0.0);
        pbr.coat_weight = self.coat_weight.clamp(0.0, 1.0);
        pbr.coat_roughness = self.coat_roughness.clamp(0.0, 1.0);
        pbr.geometry_thin_walled = self.thin_walled != 0;
        Ok(pbr)
    }
}

/// `void crust_material_default(CrustMaterial* out);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_material_default(out: *mut CrustMaterial) {
    crate::validate::write_out(out, CrustMaterial::defaults());
}

/// Mirrors `CrustRenderSettings` in `crust.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CrustRenderSettings {
    pub width: u32,
    pub height: u32,
    pub samples_per_pixel: u32,
    pub max_depth: u32,
    pub min_samples_per_pixel: u32,
    pub variance_threshold: f32,
    pub frame: u32,
}

const _: () = assert!(
    size_of::<CrustRenderSettings>() == 28,
    "CrustRenderSettings ABI drift"
);

impl CrustRenderSettings {
    pub(crate) fn to_settings(self) -> CResult<RenderSettings> {
        if self.width == 0
            || self.height == 0
            || self.width > MAX_DIM
            || self.height > MAX_DIM
            || self.samples_per_pixel == 0
            || self.max_depth == 0
            || !self.variance_threshold.is_finite()
            || self.variance_threshold < 0.0
        {
            return Err(CrustStatus::InvalidArgument);
        }
        Ok(RenderSettings::new(
            self.samples_per_pixel,
            self.max_depth,
            self.width as usize,
            self.height as usize,
            self.min_samples_per_pixel,
            self.variance_threshold,
            self.frame as isize,
        ))
    }
}

/// `void crust_render_settings_default(CrustRenderSettings* out);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_render_settings_default(out: *mut CrustRenderSettings) {
    crate::validate::write_out(
        out,
        CrustRenderSettings {
            width: 640,
            height: 360,
            samples_per_pixel: 64,
            max_depth: 8,
            min_samples_per_pixel: 0,
            variance_threshold: 0.0,
            frame: 0,
        },
    );
}
