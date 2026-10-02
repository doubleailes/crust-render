//! Typed readers for authored attributes: the `crust:*` custom attributes, the
//! per-prim geometry flags (ray mask, motion), the subdivision level, and
//! schema-attribute value decoding. Every read resolves at [`eval_time`].

use glam::{Vec3, Vec3A};
use openusd::sdf;
use openusd::usd::Prim;
use tracing::warn;

use crate::ray::{MASK_ALL, MASK_CAMERA, MASK_INDIRECT, MASK_SHADOW, RayMask};

use super::time::eval_time;

// -----------------------------------------------------------------------
// Per-prim geometry attributes (visibility mask, motion)
// -----------------------------------------------------------------------

/// `crust:rayMask` — which ray categories see this geometry (bit 0 camera,
/// bit 1 shadow, bit 2 indirect; default: all). E.g. `crust:rayMask = 6`
/// makes a light-blocker invisible to the camera.
pub(super) fn prim_ray_mask(prim: &Prim) -> RayMask {
    custom_i32(prim, "crust:rayMask")
        .map(|m| RayMask(m as u32))
        .unwrap_or(MASK_ALL)
}

/// RenderMan's per-light camera visibility, which published assets carry
/// (the Moana island's `islandPrman.usda` authors it on every light).
const RI_CAMERA_VISIBILITY: &str = "primvars:ri:attributes:visibility:camera";

/// Whether the camera sees a light, when anything says: crust's own
/// `crust:light:cameraVisible` first, RenderMan's primvar (an int, non-zero
/// meaning visible) as the portable fallback.
fn authored_camera_visibility(prim: &Prim) -> Option<bool> {
    custom_bool(prim, "crust:light:cameraVisible")
        .or_else(|| custom_bool(prim, RI_CAMERA_VISIBILITY))
}

/// Ray mask for a light's *source geometry*. Industry default (Arnold,
/// RenderMan, Karma): the surface is invisible to camera rays — lights sit
/// in frame without showing up — while shadow and indirect rays still see
/// it, so occlusion and the bounce side of MIS are unchanged.
/// `crust:light:cameraVisible = true` (or RenderMan's
/// `primvars:ri:attributes:visibility:camera = 1`) opts the surface back in
/// (classic Cornell-box look); an authored `crust:rayMask` wins outright.
pub(super) fn light_ray_mask(prim: &Prim) -> RayMask {
    if let Some(m) = custom_i32(prim, "crust:rayMask") {
        return RayMask(m as u32);
    }
    let visible = authored_camera_visibility(prim).unwrap_or(false);
    MASK_SHADOW | MASK_INDIRECT | if visible { MASK_CAMERA } else { RayMask::NONE }
}

/// Which escaping rays see a light at infinity: every category, unless the
/// camera is told not to (same attributes and precedence as
/// [`light_ray_mask`], but visible by default — a dome is the sky behind
/// the scene unless something hides it). `crust:rayMask` does not apply: a
/// light at infinity has no geometry to mask.
pub(super) fn infinite_light_escape_mask(prim: &Prim) -> RayMask {
    if authored_camera_visibility(prim).unwrap_or(true) {
        MASK_ALL
    } else {
        RayMask(MASK_ALL.0 & !MASK_CAMERA.0)
    }
}

/// `crust:motion:translate` — a world-space translation the prim moves
/// through over the shutter interval (transform motion blur).
pub(super) fn prim_motion_translate(prim: &Prim) -> Option<Vec3> {
    custom_color3(prim, "crust:motion:translate").map(|v| Vec3::new(v.x, v.y, v.z))
}

/// The refinement level a subdivision surface (any scheme but `none`) gets
/// when neither the host nor the stage's `RenderSettings` names one: 0, so
/// by default nothing is refined — a subdivision surface renders its cage,
/// shaded smooth — and refinement is something a render setting or
/// `--subdiv-level` asks for, as Hydra's `refineLevel` is. Conservative on
/// purpose: every mesh not authoring `none` is a subdivision surface (ALab
/// and Kitchen_set take the fallback scheme; Moana authors `catmullClark` in
/// 189 of its 213 mesh files), and even level 1 costs them 4× their
/// triangles (ALab: 32 → 49 GiB peak).
pub(super) const DEFAULT_SUBDIV_LEVEL: u32 = 0;

/// Hard cap on the refinement level — each level quadruples the face count,
/// so 6 turns one quad into 4096 and is already past what any of the
/// checked-in scenes could resolve.
pub(super) const MAX_SUBDIV_LEVEL: u32 = 6;

/// The one refinement level every subdivided mesh of a load is refined to.
///
/// USD has no per-prim refinement level — Hydra treats it as a render
/// setting — so this is resolved once per load, in order: the host's
/// override (`UsdImportOptions::subdivision_level`, the CLI's
/// `--subdiv-level`), then `crust:subdivisionLevel` on the `RenderSettings`
/// prim, then [`DEFAULT_SUBDIV_LEVEL`]; clamped to [`MAX_SUBDIV_LEVEL`].
/// *Whether* a mesh is refined is its `subdivisionScheme`'s call
/// (see `mesh::mesh_source`), never this one's.
///
/// `CRUST_SUBDIV=0` forces 0 — the A/B switch that separates a subdivision
/// artifact from a material or lighting one, like `CRUST_MESH_BAKE`.
pub(super) fn resolve_subdiv_level(host: Option<u32>, authored: Option<i32>) -> u32 {
    if !crate::config().subdiv {
        return 0;
    }
    let (level, source) = match (host, authored) {
        (Some(l), _) => (i64::from(l), "the host override"),
        (None, Some(l)) => (i64::from(l), "crust:subdivisionLevel"),
        (None, None) => (i64::from(DEFAULT_SUBDIV_LEVEL), "the default"),
    };
    if level > i64::from(MAX_SUBDIV_LEVEL) {
        warn!("Subdivision level {level} from {source} clamped to {MAX_SUBDIV_LEVEL}");
    } else if level < 0 {
        warn!("Subdivision level {level} from {source} clamped to 0");
    }
    level.clamp(0, i64::from(MAX_SUBDIV_LEVEL)) as u32
}

/// The ceiling on the adaptive level when neither the host nor the stage sets
/// a level. A round number: at 3 a cage already costs 64× its faces.
pub(super) const DEFAULT_ADAPTIVE_MAX_LEVEL: u32 = 3;

/// The adaptive-subdivision target, in pixels, in the same order as the level:
/// the host's override (`UsdImportOptions::subdivision_edge_length`, the CLI's
/// `--subdiv-edge-length`), then `crust:subdivisionEdgeLength` on the
/// `RenderSettings` prim. `None` means uniform subdivision. A value that is not
/// positive and finite is ignored with a warning — falling back to the stage's
/// when the host's is the bad one.
pub(super) fn resolve_subdiv_edge_length(host: Option<f32>, authored: Option<f32>) -> Option<f32> {
    let valid = |l: f32, source: &str| {
        if l.is_finite() && l > 0.0 {
            Some(l)
        } else {
            warn!(
                "Subdivision edge length {l} from {source} is not a positive pixel length — ignored"
            );
            None
        }
    };
    host.and_then(|l| valid(l, "the host override"))
        .or_else(|| authored.and_then(|l| valid(l, "crust:subdivisionEdgeLength")))
}

/// The ceiling on the adaptive level: the uniform level when the host or the
/// stage sets one (resolved and clamped as
/// [`resolve_subdiv_level`] does), else [`DEFAULT_ADAPTIVE_MAX_LEVEL`].
pub(super) fn resolve_adaptive_max_level(host: Option<u32>, authored: Option<i32>) -> u32 {
    if host.is_none() && authored.is_none() {
        return DEFAULT_ADAPTIVE_MAX_LEVEL;
    }
    resolve_subdiv_level(host, authored)
}

pub(super) fn custom_i32(prim: &Prim, name: &str) -> Option<i32> {
    let v = prim
        .attribute(name)
        .get_at::<sdf::Value>(eval_time())
        .ok()??;
    match v {
        sdf::Value::Int(i) => Some(i),
        _ => None,
    }
}

pub(super) fn custom_f32(prim: &Prim, name: &str) -> Option<f32> {
    let v = prim
        .attribute(name)
        .get_at::<sdf::Value>(eval_time())
        .ok()??;
    match v {
        sdf::Value::Float(f) => Some(f),
        sdf::Value::Double(d) => Some(d as f32),
        _ => None,
    }
}

pub(super) fn custom_bool(prim: &Prim, name: &str) -> Option<bool> {
    let v = prim
        .attribute(name)
        .get_at::<sdf::Value>(eval_time())
        .ok()??;
    match v {
        sdf::Value::Bool(b) => Some(b),
        // Authoring tools sometimes write bools as ints.
        sdf::Value::Int(i) => Some(i != 0),
        _ => None,
    }
}

pub(super) fn custom_token(prim: &Prim, name: &str) -> Option<String> {
    let v = prim
        .attribute(name)
        .get_at::<sdf::Value>(eval_time())
        .ok()??;
    match v {
        sdf::Value::Token(t) => Some(t.as_str().to_owned()),
        sdf::Value::String(s) => Some(s),
        _ => None,
    }
}

pub(super) fn custom_color3(prim: &Prim, name: &str) -> Option<Vec3A> {
    let v = prim
        .attribute(name)
        .get_at::<sdf::Value>(eval_time())
        .ok()??;
    match v {
        sdf::Value::Vec3f(c) => Some(Vec3A::new(c.x, c.y, c.z)),
        sdf::Value::Vec3d(c) => Some(Vec3A::new(c.x as f32, c.y as f32, c.z as f32)),
        _ => None,
    }
}

pub(super) fn custom_f32_array(prim: &Prim, name: &str) -> Option<Vec<f32>> {
    let v = prim
        .attribute(name)
        .get_at::<sdf::Value>(eval_time())
        .ok()??;
    match v {
        sdf::Value::FloatVec(v) => Some(v),
        sdf::Value::DoubleVec(v) => Some(v.into_iter().map(|d| d as f32).collect()),
        _ => None,
    }
}

pub(super) fn custom_i32_array(prim: &Prim, name: &str) -> Option<Vec<i32>> {
    let v = prim
        .attribute(name)
        .get_at::<sdf::Value>(eval_time())
        .ok()??;
    match v {
        sdf::Value::IntVec(v) => Some(v),
        _ => None,
    }
}

// -----------------------------------------------------------------------
// Attribute helpers
// -----------------------------------------------------------------------

pub(super) fn attr_f32(attr: &openusd::usd::Attribute) -> Option<f32> {
    match attr.get_at::<sdf::Value>(eval_time()).ok()?? {
        sdf::Value::Float(f) => Some(f),
        sdf::Value::Double(d) => Some(d as f32),
        _ => None,
    }
}

pub(super) fn attr_bool(attr: &openusd::usd::Attribute) -> Option<bool> {
    match attr.get_at::<sdf::Value>(eval_time()).ok()?? {
        sdf::Value::Bool(b) => Some(b),
        // Authoring tools sometimes write bools as ints.
        sdf::Value::Int(i) => Some(i != 0),
        _ => None,
    }
}

pub(super) fn attr_color3f(attr: &openusd::usd::Attribute) -> Option<[f32; 3]> {
    match attr.get_at::<sdf::Value>(eval_time()).ok()?? {
        // color3f is stored as Vec3f in sdf::Value
        sdf::Value::Vec3f(v) => Some([v.x, v.y, v.z]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdivision_edge_length_precedence_and_refusal() {
        assert_eq!(resolve_subdiv_edge_length(None, None), None, "uniform");
        assert_eq!(resolve_subdiv_edge_length(None, Some(4.0)), Some(4.0));
        assert_eq!(
            resolve_subdiv_edge_length(Some(2.0), Some(4.0)),
            Some(2.0),
            "the host wins"
        );
        assert_eq!(resolve_subdiv_edge_length(None, Some(0.0)), None);
        assert_eq!(resolve_subdiv_edge_length(None, Some(-1.0)), None);
        assert_eq!(resolve_subdiv_edge_length(None, Some(f32::INFINITY)), None);
        assert_eq!(
            resolve_subdiv_edge_length(Some(f32::NAN), Some(4.0)),
            Some(4.0),
            "a bad host value falls back to the stage's"
        );
    }

    #[test]
    fn the_adaptive_ceiling_is_the_level_setting_or_three() {
        if !crate::config().subdiv {
            return; // `CRUST_SUBDIV=0` forces every level to 0
        }
        assert_eq!(
            resolve_adaptive_max_level(None, None),
            DEFAULT_ADAPTIVE_MAX_LEVEL
        );
        assert_eq!(resolve_adaptive_max_level(Some(2), None), 2);
        assert_eq!(resolve_adaptive_max_level(None, Some(1)), 1);
        assert_eq!(
            resolve_adaptive_max_level(Some(0), Some(5)),
            0,
            "the host wins"
        );
        assert_eq!(resolve_adaptive_max_level(None, Some(40)), MAX_SUBDIV_LEVEL);
    }

    #[test]
    fn subdivision_level_precedence_and_clamp() {
        // Only meaningful with subdivision on (`CRUST_SUBDIV` unset).
        if !crate::config().subdiv {
            return;
        }
        assert_eq!(resolve_subdiv_level(None, None), DEFAULT_SUBDIV_LEVEL);
        assert_eq!(resolve_subdiv_level(None, Some(1)), 1, "the stage's level");
        assert_eq!(resolve_subdiv_level(Some(0), Some(3)), 0, "the host wins");
        assert_eq!(resolve_subdiv_level(Some(4), None), 4);
        assert_eq!(resolve_subdiv_level(None, Some(40)), MAX_SUBDIV_LEVEL);
        assert_eq!(resolve_subdiv_level(Some(u32::MAX), None), MAX_SUBDIV_LEVEL);
        assert_eq!(resolve_subdiv_level(None, Some(-3)), 0);
    }
}
