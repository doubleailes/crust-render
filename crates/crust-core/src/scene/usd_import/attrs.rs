//! Typed readers for authored attributes: the `crust:*` custom attributes, the
//! per-prim geometry flags (ray mask, motion, subdivision level), and
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

/// Ray mask for a light's *source geometry*. Industry default (Arnold,
/// RenderMan, Karma): the surface is invisible to camera rays — lights sit
/// in frame without showing up — while shadow and indirect rays still see
/// it, so occlusion and the bounce side of MIS are unchanged.
/// `crust:light:cameraVisible = true` opts the surface back in (classic
/// Cornell-box look); an authored `crust:rayMask` wins outright.
pub(super) fn light_ray_mask(prim: &Prim) -> RayMask {
    if let Some(m) = custom_i32(prim, "crust:rayMask") {
        return RayMask(m as u32);
    }
    let visible = custom_bool(prim, "crust:light:cameraVisible").unwrap_or(false);
    MASK_SHADOW | MASK_INDIRECT | if visible { MASK_CAMERA } else { RayMask::NONE }
}

/// `crust:motion:translate` — a world-space translation the prim moves
/// through over the shutter interval (transform motion blur).
pub(super) fn prim_motion_translate(prim: &Prim) -> Option<Vec3> {
    custom_color3(prim, "crust:motion:translate").map(|v| Vec3::new(v.x, v.y, v.z))
}

/// Hard cap on `crust:subdivisionLevel` — each level quadruples the face
/// count, so 6 turns one quad into 4096 and is already past what any of the
/// checked-in scenes could resolve.
const MAX_SUBDIV_LEVEL: i32 = 6;

/// `crust:subdivisionLevel` — uniform subdivision-surface refinement depth
/// (default 0 = render the base cage). Deliberately opt-in per prim rather
/// than triggered by `subdivisionScheme`: USD's fallback scheme is
/// `catmullClark`, so honouring the scheme alone would subdivide virtually
/// every mesh ever authored (all of the Moana island included), and USD has
/// no standard per-prim refinement level — Hydra treats refinement as a
/// render setting. The scheme still decides *how* to subdivide once a level
/// asks for it.
///
/// `CRUST_SUBDIV=0` forces 0 everywhere — the A/B switch that separates a
/// subdivision artifact from a material or lighting one, like
/// `CRUST_MESH_BAKE`.
pub(super) fn subdiv_level(prim: &Prim) -> u32 {
    if !crate::config().subdiv {
        return 0;
    }
    let level = custom_i32(prim, "crust:subdivisionLevel").unwrap_or(0);
    if level > MAX_SUBDIV_LEVEL {
        warn!(
            "Mesh at {}: crust:subdivisionLevel = {level} clamped to {MAX_SUBDIV_LEVEL}",
            prim.path()
        );
    }
    level.clamp(0, MAX_SUBDIV_LEVEL) as u32
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
