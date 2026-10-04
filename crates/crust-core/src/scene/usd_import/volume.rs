//! `crust:volume:*` prims → [`VolumeRegion`]s.

use glam::{Mat4 as GMat4, Vec3A};
use openusd::usd::Prim;
use tracing::{debug, warn};

use crate::color::Space;
use crate::volume::{DensityField, VolumeRegion};

use super::attrs::{
    custom_color, custom_f32, custom_f32_array, custom_i32, custom_i32_array, custom_token,
};

/// Import a `crust:volume:*` prim as a `VolumeRegion`. The local box is
/// `[-size/2, size/2]^3` when the prim authors a `size` attribute (a
/// `Cube`'s convention; USD's default cube size is 2) and the unit cube
/// `[-0.5, 0.5]^3` otherwise; placement, orientation and scale come from
/// the composed prim transform.
pub(super) fn emit_volume(
    prim: &Prim,
    world_xf: GMat4,
    volumes: &mut Vec<VolumeRegion>,
    working: Space,
) {
    let ty = custom_token(prim, "crust:volume:type").expect("checked by dispatch");

    let field = match ty.as_str() {
        "homogeneous" => DensityField::Homogeneous,
        "smoke" => DensityField::Noise {
            scale: custom_f32(prim, "crust:volume:noiseScale").unwrap_or(4.0),
            octaves: custom_i32(prim, "crust:volume:noiseOctaves")
                .unwrap_or(4)
                .max(1) as u32,
            gain: custom_f32(prim, "crust:volume:noiseGain").unwrap_or(0.5),
            lacunarity: custom_f32(prim, "crust:volume:noiseLacunarity").unwrap_or(2.0),
            threshold: custom_f32(prim, "crust:volume:noiseThreshold").unwrap_or(0.3),
            seed: custom_i32(prim, "crust:volume:noiseSeed").unwrap_or(0) as u32,
        },
        "grid" => {
            let dims = custom_i32_array(prim, "crust:volume:gridDims");
            let data = custom_f32_array(prim, "crust:volume:gridData");
            match (dims, data) {
                (Some(d), Some(data)) if d.len() == 3 => {
                    let (nx, ny, nz) = (
                        d[0].max(1) as usize,
                        d[1].max(1) as usize,
                        d[2].max(1) as usize,
                    );
                    if nx * ny * nz != data.len() {
                        warn!(
                            "Volume at {}: gridDims {}x{}x{} does not match gridData length {} — skipped",
                            prim.path(),
                            nx,
                            ny,
                            nz,
                            data.len()
                        );
                        return;
                    }
                    DensityField::Grid { nx, ny, nz, data }
                }
                _ => {
                    warn!(
                        "Volume at {}: grid type needs int[3] crust:volume:gridDims and float[] crust:volume:gridData — skipped",
                        prim.path()
                    );
                    return;
                }
            }
        }
        other => {
            warn!(
                "Volume at {}: unknown crust:volume:type \"{}\" (expected homogeneous | smoke | grid) — skipped",
                prim.path(),
                other
            );
            return;
        }
    };

    // Per-channel coefficients, so they are colours in the working space
    // like any other: converted only when `colorSpace` names another.
    let sigma_s = custom_color(prim, "crust:volume:sigmaS", working).unwrap_or(Vec3A::splat(0.5));
    let sigma_a = custom_color(prim, "crust:volume:sigmaA", working).unwrap_or(Vec3A::ZERO);
    let emission = custom_color(prim, "crust:volume:emission", working).unwrap_or(Vec3A::ZERO);
    let g = custom_f32(prim, "crust:volume:anisotropy").unwrap_or(0.0);
    let density_scale = custom_f32(prim, "crust:volume:densityScale").unwrap_or(1.0);
    let half = custom_f32(prim, "size").map_or(0.5, |s| s * 0.5);

    debug!(
        "Imported {} volume at {} (densityScale={})",
        ty,
        prim.path(),
        density_scale
    );
    volumes.push(VolumeRegion::new(
        world_xf,
        Vec3A::splat(half),
        sigma_s,
        sigma_a,
        g,
        emission,
        density_scale,
        field,
    ));
}
