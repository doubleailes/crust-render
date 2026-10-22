//! `UsdLuxMeshLightAPI` → a mesh light: the mesh's triangles as an
//! [`AreaLight`]'s [`MeshShape`], and the light's one radiance carried to
//! every ray by its geometry's material (lighting design record, "Mesh
//! lights").
//!
//! Read in two steps around the mesh's placement. [`plan`], before it, reads
//! the light's inputs and decides whether the mesh can be a light at all —
//! every refusal leaves the mesh as it would be without the API. [`emit`],
//! after it, builds the shape from the triangles the kernel holds, divides
//! by their world area under `normalize`, binds the light's emission to the
//! geometry and adds the light. Both run on the traversal, so the light is
//! added while its prim is the one being visited, as every UsdLux light is:
//! its links, its pass membership and its live-edit origin follow from that.

use std::sync::Arc;

use glam::Vec3A;
use openusd::usd::{Prim, Stage};
use openusd_schemas::lux::LightAPI;
use tracing::debug;

use crate::color::Space;
use crate::light::{AreaLight, LightShape, LobeGains, MeshShape};
use crate::material::{Emissive, Material, MeshLightMaterial};
use crate::ray::RayMask;
use crate::warning;

use super::ImportCtx;
use super::attrs::{custom_token, mesh_light_ray_mask, prim_motion_translate};
use super::lights::{lux_params, tag_last};
use super::xform::Span;

/// The applied API schema that makes a mesh a light.
pub(super) const MESH_LIGHT_API: &str = "MeshLightAPI";

/// Whether `prim` applies `MeshLightAPI`.
pub(super) fn applies(prim: &Prim) -> bool {
    prim.has_api_schema(MESH_LIGHT_API).unwrap_or(false)
}

/// Warns when `prim`, geometry that is not a `Mesh` (an implicit shape,
/// curves), applies `MeshLightAPI`: it is not a light.
pub(super) fn refuse_not_a_mesh(prim: &Prim) {
    if applies(prim) {
        warning!(
            LightMeshLightNotAMesh,
            at = prim.path(),
            "{}: MeshLightAPI applies to a Mesh only — this prim is not a light",
            prim.path()
        );
    }
}

/// `light:materialSyncMode`: which radiance the mesh light has, read by crust
/// as one radiance for every ray (see [`plan`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SyncMode {
    /// The glow, tinted and scaled by the light's inputs — the
    /// `MeshLightAPI` fallback.
    MaterialGlowTintsLight,
    /// The light's inputs alone; the material still reflects.
    Independent,
    /// The light's inputs alone, and no surface response at all.
    NoMaterialResponse,
}

impl SyncMode {
    fn read(prim: &Prim) -> SyncMode {
        match custom_token(prim, "light:materialSyncMode").as_deref() {
            None | Some("materialGlowTintsLight") => SyncMode::MaterialGlowTintsLight,
            Some("independent") => SyncMode::Independent,
            Some("noMaterialResponse") => SyncMode::NoMaterialResponse,
            Some(other) => {
                warning!(
                    LightUnknownToken,
                    at = prim.path(),
                    "{}: light:materialSyncMode = {other:?} is not a sync mode — using its \
                     fallback materialGlowTintsLight",
                    prim.path()
                );
                SyncMode::MaterialGlowTintsLight
            }
        }
    }
}

/// What [`plan`] decided for a mesh light, before its triangles are known.
pub(super) struct Plan {
    /// The radiance before `normalize`.
    radiance: Vec3A,
    normalize: bool,
    gains: LobeGains,
    sync: SyncMode,
    mask: RayMask,
}

/// Reads `prim`'s `MeshLightAPI`, bound to `material` and placed over
/// `span`: `None` when it does not apply the API, or applies it but cannot
/// be a light — then warned, and the mesh renders as without the API.
///
/// The radiance, with `L` the light's inputs (`color · 2^exposure ·
/// intensity`, times the blackbody under `enableColorTemperature`):
/// `glow · L` under `materialGlowTintsLight`, where glow is the material's
/// [`Material::uniform_emission`], and `L` under `independent` and
/// `noMaterialResponse`. The schema has light rays see `L` and other rays the
/// glow; read literally, NEE and a bounce would see two radiances for one
/// emitter and the image would depend on the MIS weights, so crust gives
/// every ray the one radiance. With the inputs unauthored, `L = 1` and the
/// mesh light is as bright as the glow: applying the API changes the noise,
/// not the picture.
pub(super) fn plan(
    stage: &Stage,
    prim: &Prim,
    material: &Arc<dyn Material>,
    span: &Span,
    working: Space,
) -> Option<Plan> {
    if !applies(prim) {
        return None;
    }
    let path = prim.path();
    let light = LightAPI::get(stage, path.clone()).ok().flatten()?;
    let params = lux_params(prim, &light, working);
    let sync = SyncMode::read(prim);
    if span.moves() || prim_motion_translate(prim).is_some() {
        warning!(
            LightMeshLightMoving,
            at = path,
            "{path}: a mesh light that moves over the shutter cannot be sampled — it is \
             emissive geometry only"
        );
        return None;
    }
    if sync != SyncMode::NoMaterialResponse && material.has_cutout() {
        warning!(
            LightMeshLightCutout,
            at = path,
            "{path}: a mesh light whose material is a cutout cannot be sampled — it is \
             emissive geometry only"
        );
        return None;
    }
    let radiance = match sync {
        SyncMode::MaterialGlowTintsLight => {
            let Some(glow) = material.uniform_emission() else {
                warning!(
                    LightMeshLightVaryingEmission,
                    at = path,
                    "{path}: the material's glow varies over the surface or with the view \
                     (a texture, a primvar, a coat or a fuzz), which a mesh light under \
                     materialGlowTintsLight cannot sample — it is emissive geometry only"
                );
                return None;
            };
            glow * params.emission
        }
        SyncMode::Independent | SyncMode::NoMaterialResponse => params.emission,
    };
    if !(radiance.is_finite() && radiance.max_element() > 0.0) {
        warning!(
            LightMeshLightNoEmission,
            at = path,
            "{path}: the mesh light emits nothing ({:?}, light:materialSyncMode {sync:?}) — \
             not a light",
            radiance.to_array()
        );
        return None;
    }
    Some(Plan {
        radiance,
        normalize: params.normalize,
        gains: params.gains,
        sync,
        mask: mesh_light_ray_mask(prim),
    })
}

/// Makes the mesh placed at `geom_id`, of world-space `tris` in the kernel's
/// order, the light `plan` describes: binds the light's emission to its
/// geometry over `inner` (or alone, under `noMaterialResponse`), applies the
/// light's camera visibility, and adds the [`AreaLight`].
pub(super) fn emit(
    ctx: &mut ImportCtx,
    prim: &Prim,
    plan: Plan,
    geom_id: u32,
    tris: Vec<[[f32; 3]; 3]>,
    inner: Arc<dyn Material>,
) {
    let count = tris.len();
    let Some(shape) = MeshShape::new(tris) else {
        warning!(
            LightDegenerateShape,
            at = prim.path(),
            "{}: the mesh light's {count} triangle(s) enclose no area — skipped",
            prim.path()
        );
        return;
    };
    let area = shape.area();
    let radiance = if plan.normalize {
        plan.radiance / area.max(1e-30)
    } else {
        plan.radiance
    };
    // Two-sided, as material emission is.
    let emitter = Arc::new(Emissive::new(radiance));
    let material: Arc<dyn Material> = match plan.sync {
        SyncMode::NoMaterialResponse => emitter.clone(),
        _ => Arc::new(MeshLightMaterial::new(inner, emitter.clone())),
    };
    ctx.world.set_material(geom_id, material);
    ctx.world.set_mask(geom_id, plan.mask);
    ctx.lights.add(AreaLight::new(shape, emitter, geom_id));
    tag_last(&mut ctx.lights, prim, plan.gains);
    debug!(
        "Mesh light {}: {count} triangles, area={area} normalize={} radiance={:?} ({:?})",
        prim.path(),
        plan.normalize,
        radiance,
        plan.sync
    );
}
