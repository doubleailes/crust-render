//! Typed readers for authored attributes: the `crust:*` custom attributes, the
//! per-prim geometry flags (ray mask, motion), the subdivision level, and
//! the value decoders every attribute read of the importer goes through
//! ([`value_at`] + `decode_*`). Every read resolves at [`eval_time`].

use glam::{Vec3, Vec3A};
use openusd::gf::Vec3f;
use openusd::sdf;
use openusd::usd::{Attribute, Prim};
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

// -----------------------------------------------------------------------
// Value decoding
// -----------------------------------------------------------------------
//
// Every typed read in the importer is `value_at` plus one `decode_*`. The
// decoders accept every encoding the importer has met for a type (a float
// authored as `double`, a bool as an `int`, a colour as `double3`) and
// narrow to `f32` with a plain `as`, so a value already read one way reads
// to the same bits through any of them.

/// `attr`'s value at [`eval_time`], or `None` when it is unauthored, blocked
/// or unreadable. The one place an attribute is read at the evaluation time;
/// openusd's own xformable composition is asked at
/// [`xform_time`](super::time::xform_time) instead.
pub(super) fn value_at(attr: &Attribute) -> Option<sdf::Value> {
    attr.get_at::<sdf::Value>(eval_time()).ok().flatten()
}

/// An `int`.
pub(super) fn decode_i32(v: &sdf::Value) -> Option<i32> {
    match v {
        sdf::Value::Int(i) => Some(*i),
        _ => None,
    }
}

/// A `float`, `double` or `half`, as `f32`.
pub(super) fn decode_f32(v: &sdf::Value) -> Option<f32> {
    match v {
        sdf::Value::Float(f) => Some(*f),
        sdf::Value::Double(d) => Some(*d as f32),
        sdf::Value::Half(h) => Some(h.to_f32()),
        _ => None,
    }
}

/// A `bool`, or an `int` (non-zero is true): authoring tools sometimes write
/// bools as ints.
pub(super) fn decode_bool(v: &sdf::Value) -> Option<bool> {
    match v {
        sdf::Value::Bool(b) => Some(*b),
        sdf::Value::Int(i) => Some(*i != 0),
        _ => None,
    }
}

/// A `token` or a `string`. Unlike `sdf::Value::as_str`, not an `asset`.
pub(super) fn decode_token(v: &sdf::Value) -> Option<&str> {
    match v {
        sdf::Value::Token(t) => Some(t.as_str()),
        sdf::Value::String(s) => Some(s),
        _ => None,
    }
}

/// A three-component vector (`float3`, `double3`, `half3` and the roles
/// spelled with them: `color3f`, `point3f`, `vector3d`, …), as `f32`.
pub(super) fn decode_vec3(v: &sdf::Value) -> Option<Vec3> {
    match v {
        sdf::Value::Vec3f(c) => Some(Vec3::new(c.x, c.y, c.z)),
        sdf::Value::Vec3d(c) => Some(Vec3::new(c.x as f32, c.y as f32, c.z as f32)),
        sdf::Value::Vec3h(c) => Some(Vec3::new(c.x.to_f32(), c.y.to_f32(), c.z.to_f32())),
        _ => None,
    }
}

/// A `float[]` or `double[]`, as `f32`. Takes the value so a `float[]` moves
/// out without a copy.
pub(super) fn decode_f32s(v: sdf::Value) -> Option<Vec<f32>> {
    match v {
        sdf::Value::FloatVec(v) => Some(v),
        sdf::Value::DoubleVec(v) => Some(v.into_iter().map(|d| d as f32).collect()),
        _ => None,
    }
}

/// An `int[]`.
pub(super) fn decode_i32s(v: sdf::Value) -> Option<Vec<i32>> {
    match v {
        sdf::Value::IntVec(v) => Some(v),
        _ => None,
    }
}

/// A `float3[]` (`point3f[]`, `vector3f[]`, …), as authored.
pub(super) fn decode_vec3fs(v: sdf::Value) -> Option<Vec<Vec3f>> {
    match v {
        sdf::Value::Vec3fVec(v) => Some(v),
        _ => None,
    }
}

pub(super) fn attr_f32(attr: &Attribute) -> Option<f32> {
    decode_f32(&value_at(attr)?)
}

pub(super) fn attr_bool(attr: &Attribute) -> Option<bool> {
    decode_bool(&value_at(attr)?)
}

pub(super) fn attr_vec3(attr: &Attribute) -> Option<Vec3> {
    decode_vec3(&value_at(attr)?)
}

pub(super) fn attr_token(attr: &Attribute) -> Option<String> {
    decode_token(&value_at(attr)?).map(str::to_owned)
}

// The same reads for a prim's attribute by name — the `crust:*` custom
// attributes and the schema attributes read off a prim directly.

pub(super) fn custom_i32(prim: &Prim, name: &str) -> Option<i32> {
    decode_i32(&value_at(&prim.attribute(name))?)
}

pub(super) fn custom_f32(prim: &Prim, name: &str) -> Option<f32> {
    attr_f32(&prim.attribute(name))
}

pub(super) fn custom_bool(prim: &Prim, name: &str) -> Option<bool> {
    attr_bool(&prim.attribute(name))
}

pub(super) fn custom_token(prim: &Prim, name: &str) -> Option<String> {
    attr_token(&prim.attribute(name))
}

pub(super) fn custom_color3(prim: &Prim, name: &str) -> Option<Vec3A> {
    attr_vec3(&prim.attribute(name)).map(Vec3A::from)
}

pub(super) fn custom_f32_array(prim: &Prim, name: &str) -> Option<Vec<f32>> {
    decode_f32s(value_at(&prim.attribute(name))?)
}

pub(super) fn custom_i32_array(prim: &Prim, name: &str) -> Option<Vec<i32>> {
    decode_i32s(value_at(&prim.attribute(name))?)
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
