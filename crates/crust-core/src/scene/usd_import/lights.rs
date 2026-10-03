//! UsdLux lights → [`LightList`] entries (and their emissive scene geometry).

use std::path::Path;
use std::sync::Arc;

use crust_rt::Geometry;
use glam::{Affine3A, Mat3A, Mat4 as GMat4, Vec3, Vec3A};
use openusd::usd::{Prim, Stage};
use openusd_schemas::lux::{
    CylinderLight, DiskLight, DistantLight as UsdDistantLight, DomeLight, Light as UsdLight,
    RectLight, ShapingAPI, SphereLight,
};
use tracing::{debug, warn};

use crate::light::{
    AffineShape, AreaLight, AreaShape, DistantLight as CoreDistantLight,
    DomeLight as CoreDomeLight, LightList, LightShape, RectShape, SphereShape, UnitShape,
};
use crate::lux::{IesShaping, Shaping, distant_illuminance, distant_size_factor};
use crate::material::Emissive;

use super::attrs::{
    attr_bool, attr_f32, attr_token, attr_vec3, custom_bool, custom_color3, custom_f32,
    infinite_light_escape_mask, light_ray_mask, value_at,
};
use super::materials::asset_value_path;
use super::{ImportCaches, ImportCtx};

/// The `LightAPI` quantities every UsdLux light shares.
struct LuxParams {
    /// `L_Color` in the spec's notation, in nits:
    /// `intensity · 2^exposure · color`, times the colour temperature's
    /// blackbody when `enableColorTemperature` is on. Before `normalize`.
    pub(super) emission: Vec3A,
    /// `inputs:normalize` — divide by the light's size (see each light).
    pub(super) normalize: bool,
}

/// Reads the shared `LightAPI` inputs. Warns about the ones crust reads but
/// cannot honour, since each makes the image differ from what was authored:
/// `diffuse` / `specular` are per-lobe multipliers, and crust's light
/// transport does not split a light's contribution by lobe.
fn lux_params(prim: &Prim, light: &impl UsdLight) -> LuxParams {
    // A non-finite value would reach both MIS halves as NaN radiance, so it
    // falls back to the schema default like the shaping inputs do.
    let finite = |name: &str, v: Option<f32>, fallback: f32| match v {
        Some(x) if !x.is_finite() => {
            warn!(
                "{}: inputs:{name} = {x} is not finite — using its fallback {fallback}",
                prim.path()
            );
            fallback
        }
        v => v.unwrap_or(fallback),
    };
    let intensity = finite("intensity", attr_f32(&light.intensity_attr()), 1.0);
    let exposure = finite("exposure", attr_f32(&light.exposure_attr()), 0.0);
    let color = match attr_vec3(&light.color_attr()).map(|c| c.to_array()) {
        Some(c) if c.iter().any(|x| !x.is_finite()) => {
            warn!(
                "{}: inputs:color = {c:?} is not finite — using its fallback (1, 1, 1)",
                prim.path()
            );
            [1.0; 3]
        }
        c => c.unwrap_or([1.0; 3]),
    };
    let gain = intensity * 2f32.powf(exposure);
    let mut emission = Vec3A::new(color[0] * gain, color[1] * gain, color[2] * gain);

    if attr_bool(&light.enable_color_temperature_attr()).unwrap_or(false) {
        let kelvin = attr_f32(&light.color_temperature_attr()).unwrap_or(6500.0);
        // The blackbody leaves the Rec.709 gamut below ~1900 K; clamp the
        // product, not the factor, so a negative channel cannot flip sign
        // against a negative `color`.
        emission = (emission * crate::blackbody_rgb(kelvin)).max(Vec3A::ZERO);
    }

    for (name, attr) in [
        ("diffuse", light.diffuse_attr()),
        ("specular", light.specular_attr()),
    ] {
        if let Some(v) = attr_f32(&attr)
            && v != 1.0
        {
            warn!(
                "{}: inputs:{name} = {v} is a per-lobe multiplier crust does not \
                 support — ignored (the light contributes at 1.0)",
                prim.path()
            );
        }
    }

    LuxParams {
        emission,
        normalize: attr_bool(&light.normalize_attr()).unwrap_or(false),
    }
}

/// The light's `ShapingAPI`, or `None` when nothing about it shapes.
///
/// Read off the prim directly rather than through `ShapingAPI::get`, since
/// shaping attributes authored without the API applied are common in
/// exported stages and hdEmbree honours them. What the API's presence
/// changes is the *fallback* of an unauthored input: with `ShapingAPI`
/// applied the schema's `cone:angle = 90` is in force (Hydra hands the
/// delegate the schema fallback), without it there is no such attribute and
/// the cone is open (hdEmbree's own default, 180°). openusd does not report
/// schema fallbacks for applied API schemas, so that rule is applied here.
fn lux_shaping(
    stage: &Stage,
    prim: &Prim,
    light_to_world: Mat3A,
    caches: &mut ImportCaches,
) -> Option<Shaping> {
    let applied = ShapingAPI::get(stage, prim.path().clone())
        .ok()
        .flatten()
        .is_some();
    // A non-finite authored value survives every formula below and turns the
    // light's radiance — both MIS halves of it — into NaN. Such a value is
    // refused and replaced by the input's fallback, with a warning.
    let finite = |name: &str, v: Option<f32>| match v {
        Some(x) if !x.is_finite() => {
            warn!(
                "{}: {name} = {x} is not finite — using its fallback",
                prim.path()
            );
            None
        }
        v => v,
    };
    let mut shaping = Shaping::new(light_to_world);
    shaping.focus = finite(
        "inputs:shaping:focus",
        custom_f32(prim, "inputs:shaping:focus"),
    )
    .unwrap_or(0.0);
    // The schema's fallback is black, not white ("The default tint is
    // black", usdLux/schema.usda): unauthored, focus darkens off-axis
    // emission rather than leaving it neutral, exactly as in hdEmbree.
    shaping.focus_tint = match custom_color3(prim, "inputs:shaping:focusTint") {
        Some(c) if !c.is_finite() => {
            warn!(
                "{}: inputs:shaping:focusTint = {c} is not finite — using its fallback",
                prim.path()
            );
            Vec3A::ZERO
        }
        c => c.unwrap_or(Vec3A::ZERO),
    };
    shaping.cone_angle_deg = finite(
        "inputs:shaping:cone:angle",
        custom_f32(prim, "inputs:shaping:cone:angle"),
    )
    .unwrap_or(if applied {
        Shaping::SCHEMA_CONE_ANGLE_DEG
    } else {
        180.0
    });
    shaping.cone_softness = finite(
        "inputs:shaping:cone:softness",
        custom_f32(prim, "inputs:shaping:cone:softness"),
    )
    .unwrap_or(0.0);

    let ies_file = value_at(&prim.attribute("inputs:shaping:ies:file"))
        .and_then(|v| asset_value_path(&v, caches.stage_path));
    if let Some(path) = ies_file {
        let profile = caches.load_cached(
            |c| &mut c.ies,
            path,
            |assets, path| assets.load_ies(path),
            |path, loaded| {
                if loaded.is_none() {
                    warn!(
                        "{}: could not load IES profile {} — the light renders \
                         without it",
                        prim.path(),
                        path.display()
                    );
                }
            },
        );
        shaping.ies = profile.map(|profile| IesShaping {
            profile,
            angle_scale: finite(
                "inputs:shaping:ies:angleScale",
                custom_f32(prim, "inputs:shaping:ies:angleScale"),
            )
            .unwrap_or(0.0),
            normalize: custom_bool(prim, "inputs:shaping:ies:normalize").unwrap_or(false),
        });
    }

    (!shaping.is_neutral()).then_some(shaping)
}

/// The linear part of a matrix, for direction-only uses.
fn linear_part(m: GMat4) -> Mat3A {
    Mat3A::from_mat4(m)
}

/// Uniform scale of `m` along the named local axes, if it is a similarity
/// there — equal lengths, mutually perpendicular — and snapped to exactly 1
/// when within rounding of it, so an unscaled light keeps its authored
/// radius to the bit.
fn similarity_scale(m: Mat3A, axes: &[usize]) -> Option<f32> {
    let cols = [m.x_axis, m.y_axis, m.z_axis];
    let s = cols[axes[0]].length();
    if s.is_nan() || s <= 0.0 {
        return None;
    }
    if axes
        .iter()
        .any(|&a| (cols[a].length() - s).abs() > 1e-5 * s)
    {
        return None;
    }
    for (i, &a) in axes.iter().enumerate() {
        if axes[i + 1..]
            .iter()
            .any(|&b| !perpendicular(cols[a], cols[b]))
        {
            return None;
        }
    }
    Some(if (s - 1.0).abs() < 1e-6 { 1.0 } else { s })
}

fn perpendicular(a: Vec3A, b: Vec3A) -> bool {
    a.dot(b).abs() <= 1e-5 * a.length() * b.length()
}

/// A sphere, disk or cylinder light's shape — the three UsdLux lights defined
/// on a round [`UnitShape`] — at its authored size.
struct RoundShape {
    unit: UnitShape,
    radius: f32,
    /// The cylinder's `inputs:length`; unused by the other two.
    length: f32,
}

/// Attaches a sphere, disk or cylinder light of the given [`RoundShape`]
/// under the prim transform `world_xf`.
///
/// Where `world_xf` is a similarity over the axes the shape is round in, the
/// geometry is the kernel's world-space analytic primitive, as a sphere light
/// always was, with the authored radius times that scale. Otherwise — a
/// non-uniform scale squashing it into an ellipsoid, an ellipse or an
/// elliptical tube — it is the unit primitive placed by an instance, which
/// the kernel intersects exactly under any affine map. Either way the light
/// samples the same [`AffineShape`] (or, for the round sphere, the historical
/// [`SphereShape`], so existing scenes render exactly as before), and
/// `normalize` divides by its world-space area.
fn emit_round_light(
    ctx: &mut ImportCtx,
    prim: &Prim,
    shape: RoundShape,
    world_xf: GMat4,
    params: LuxParams,
    shaping: Option<Shaping>,
) {
    let RoundShape {
        unit,
        radius,
        length,
    } = shape;
    // A negative or zero radius (or length) would pass the affine check —
    // a negative scale is a reflection, still invertible — while the kernel
    // refuses the matching primitive, leaving a light NEE samples on a
    // surface no ray can hit. Refuse it here, once, for both.
    let valid = |x: f32| x.is_finite() && x > 0.0;
    if !valid(radius) || (unit == UnitShape::Cylinder && !valid(length)) {
        warn!(
            "{}: {:?} light radius {radius}{} must be finite and positive — skipped",
            prim.path(),
            unit,
            if unit == UnitShape::Cylinder {
                format!(", length {length}")
            } else {
                String::new()
            }
        );
        return;
    }
    let local = match unit {
        UnitShape::Sphere => Vec3::splat(radius),
        UnitShape::Disk => Vec3::new(radius, radius, 1.0),
        UnitShape::Cylinder => Vec3::new(length, radius, radius),
    };
    let l2w = Affine3A::from_mat4(world_xf * GMat4::from_scale(local));
    let Some(affine) = AffineShape::new(unit, l2w) else {
        warn!(
            "{}: light transform collapses its shape (zero size or scale) — skipped",
            prim.path()
        );
        return;
    };
    let area = affine.area();
    let radiance = if params.normalize {
        params.emission / area.max(1e-30)
    } else {
        params.emission
    };
    let material = Arc::new(Emissive::light(radiance, shaping));
    let m = linear_part(world_xf);
    let origin = l2w.translation;

    let direct = match unit {
        UnitShape::Sphere => similarity_scale(m, &[0, 1, 2]).map(|s| Geometry::Sphere {
            center: origin,
            radius: radius * s,
        }),
        UnitShape::Disk => similarity_scale(m, &[0, 1]).map(|s| Geometry::Disk {
            center: origin,
            // Local −Z under the normal transform: the emitting side, which
            // the kernel reports as the disk's front.
            normal: m.inverse().transpose() * -Vec3A::Z,
            radius: radius * s,
        }),
        UnitShape::Cylinder => similarity_scale(m, &[1, 2])
            .filter(|_| perpendicular(m.x_axis, m.y_axis) && perpendicular(m.x_axis, m.z_axis))
            .map(|s| Geometry::Cylinder {
                p0: l2w.transform_point3a(Vec3A::new(-0.5, 0.0, 0.0)),
                p1: l2w.transform_point3a(Vec3A::new(0.5, 0.0, 0.0)),
                radius: radius * s,
            }),
    };
    let analytic = direct.is_some();
    let round_sphere = match &direct {
        Some(Geometry::Sphere { center, radius }) => Some(SphereShape {
            center: *center,
            radius: *radius,
        }),
        _ => None,
    };
    let geometry = direct.unwrap_or_else(|| {
        let mut b = crust_rt::SceneBuilder::new();
        b.attach(match unit {
            UnitShape::Sphere => Geometry::Sphere {
                center: Vec3A::ZERO,
                radius: 1.0,
            },
            UnitShape::Disk => Geometry::Disk {
                center: Vec3A::ZERO,
                normal: -Vec3A::Z,
                radius: 1.0,
            },
            UnitShape::Cylinder => Geometry::Cylinder {
                p0: Vec3A::new(-0.5, 0.0, 0.0),
                p1: Vec3A::new(0.5, 0.0, 0.0),
                radius: 1.0,
            },
        });
        Geometry::Instance {
            scene: Arc::new(b.commit_with(crate::commit_options())),
            transform: l2w,
            transform_end: None,
        }
    });
    let geom_id = ctx
        .world
        .attach_masked(geometry, material.clone(), light_ray_mask(prim));

    let shape: AreaShape = match round_sphere {
        Some(sphere) => sphere.into(),
        None => affine.into(),
    };
    ctx.lights.add(AreaLight::new(shape, material, geom_id));
    debug!(
        "{:?} light {}: area={} normalize={} radiance={:?} ({})",
        unit,
        prim.path(),
        area,
        params.normalize,
        radiance,
        if analytic { "analytic" } else { "instanced" }
    );
}

/// `UsdLuxSphereLight`: radius `inputs:radius` (0.5), scaled by the prim
/// transform — a non-uniform scale makes an ellipsoid. `treatAsPoint` is a
/// hint for renderers without area lights and is ignored, as the schema
/// permits.
pub(super) fn emit_sphere_light(
    stage: &Stage,
    ctx: &mut ImportCtx,
    prim: &Prim,
    light: &SphereLight,
    world_xf: GMat4,
) {
    let radius = attr_f32(&light.radius_attr()).unwrap_or(0.5);
    let params = lux_params(prim, light);
    let shaping = lux_shaping(stage, prim, linear_part(world_xf), &mut ctx.caches);
    let shape = RoundShape {
        unit: UnitShape::Sphere,
        radius,
        length: 0.0,
    };
    emit_round_light(ctx, prim, shape, world_xf, params, shaping);
}

/// `UsdLuxDiskLight`: a disk of `inputs:radius` (0.5) in the local XY
/// plane, emitting from one side, along local −Z.
pub(super) fn emit_disk_light(
    stage: &Stage,
    ctx: &mut ImportCtx,
    prim: &Prim,
    light: &DiskLight,
    world_xf: GMat4,
) {
    let radius = attr_f32(&light.radius_attr()).unwrap_or(0.5);
    let params = lux_params(prim, light);
    let shaping = lux_shaping(stage, prim, linear_part(world_xf), &mut ctx.caches);
    let shape = RoundShape {
        unit: UnitShape::Disk,
        radius,
        length: 0.0,
    };
    emit_round_light(ctx, prim, shape, world_xf, params, shaping);
}

/// `UsdLuxCylinderLight`: a tube of `inputs:radius` (0.5) and
/// `inputs:length` (1) along local X, centred on the origin, emitting
/// outward from its side and not from its end caps. `treatAsLine` is
/// ignored, as `treatAsPoint` is.
pub(super) fn emit_cylinder_light(
    stage: &Stage,
    ctx: &mut ImportCtx,
    prim: &Prim,
    light: &CylinderLight,
    world_xf: GMat4,
) {
    let radius = attr_f32(&light.radius_attr()).unwrap_or(0.5);
    let length = attr_f32(&light.length_attr()).unwrap_or(1.0);
    let params = lux_params(prim, light);
    let shaping = lux_shaping(stage, prim, linear_part(world_xf), &mut ctx.caches);
    let shape = RoundShape {
        unit: UnitShape::Cylinder,
        radius,
        length,
    };
    emit_round_light(ctx, prim, shape, world_xf, params, shaping);
}

/// `RectLight`'s `inputs:texture:file`, decoded by the host. Cached by
/// resolved path: a rig commonly reuses one card texture on many lights.
fn rect_light_texture(prim: &Prim, caches: &mut ImportCaches) -> Option<Arc<crate::LightTexture>> {
    let value = value_at(&prim.attribute("inputs:texture:file"))?;
    let path = asset_value_path(&value, caches.stage_path)?;
    caches.load_cached(
        |c| &mut c.light_textures,
        path,
        |assets, path| assets.load_light_texture(path),
        |path, loaded| match loaded {
            Some(t) => debug!(
                "RectLight {}: texture {} ({}x{})",
                prim.path(),
                path.display(),
                t.width(),
                t.height()
            ),
            None => warn!(
                "RectLight at {}: could not load inputs:texture:file {} — the light \
                 emits its uniform colour",
                prim.path(),
                path.display()
            ),
        },
    )
}

/// `UsdLuxRectLight`: a `width × height` rectangle (1 × 1) in the local XY
/// plane, centred on the origin, emitting from one side, along local −Z, and
/// multiplied by `inputs:texture:file` when one is authored (image top row
/// at the light's +Y edge, left column at −X).
pub(super) fn emit_rect_light(
    stage: &Stage,
    ctx: &mut ImportCtx,
    prim: &Prim,
    light: &RectLight,
    world_xf: GMat4,
) {
    let width = attr_f32(&light.width_attr()).unwrap_or(1.0);
    let height = attr_f32(&light.height_attr()).unwrap_or(1.0);
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        warn!(
            "RectLight at {}: width {width} × height {height} must be finite and \
             positive — skipped",
            prim.path()
        );
        return;
    }
    let params = lux_params(prim, light);
    let shaping = lux_shaping(stage, prim, linear_part(world_xf), &mut ctx.caches);
    let texture = rect_light_texture(prim, &mut ctx.caches);

    let corner = world_xf.transform_point3(Vec3::new(-0.5 * width, -0.5 * height, 0.0));
    let origin = Vec3A::new(corner.x, corner.y, corner.z);
    let eu = world_xf.transform_vector3(Vec3::new(width, 0.0, 0.0));
    let ev = world_xf.transform_vector3(Vec3::new(0.0, height, 0.0));
    let edge_u = Vec3A::new(eu.x, eu.y, eu.z);
    let edge_v = Vec3A::new(ev.x, ev.y, ev.z);

    // The emitting normal is local −Z under the *normal* transform, which is
    // ±(edge_u × edge_v): the plain transformed −Z is only perpendicular to
    // the rectangle while the transform is a similarity, and under a
    // sheared or non-uniformly scaled rotation it is not. The transformed
    // axis is kept when it is (every ordinary light), so those render as
    // they always have.
    let nz = world_xf.transform_vector3(Vec3::NEG_Z);
    let nz = Vec3A::new(nz.x, nz.y, nz.z);
    let cross = edge_u.cross(edge_v);
    let det = linear_part(world_xf).determinant();
    let exact = -cross * det.signum();
    let normal = if nz.dot(edge_u).abs() <= 1e-5 * nz.length() * edge_u.length()
        && nz.dot(edge_v).abs() <= 1e-5 * nz.length() * edge_v.length()
    {
        nz
    } else {
        exact
    };

    let area = cross.length();
    let radiance = if params.normalize {
        params.emission / area.max(1e-30)
    } else {
        params.emission
    };

    // The geometry (one mesh: two triangles spanning the rectangle) and the
    // AreaLight share one surface; bounce hits are attributed to the light by
    // the geometry id. The triangles are wound so their geometric normal is
    // the *emitting* one, because a light's surface is one-sided and
    // `front_face` is how a bounce hit knows which side it arrived on.
    let mut emitter = Emissive::light(radiance, shaping);
    if let Some(image) = texture
        && let Some(map) = crate::RectTexture::new(image, origin, edge_u, edge_v)
    {
        emitter = emitter.with_texture(map);
    }
    let material = Arc::new(emitter);
    let (c00, c10, c11, c01) = (
        origin,
        origin + edge_u,
        origin + edge_u + edge_v,
        origin + edge_v,
    );
    let indices = if cross.dot(normal) > 0.0 {
        vec![[0, 1, 2], [0, 2, 3]]
    } else {
        vec![[0, 2, 1], [0, 3, 2]]
    };
    let geom_id = ctx.world.attach_masked(
        Geometry::TriangleMesh {
            vertices: vec![
                c00.to_array(),
                c10.to_array(),
                c11.to_array(),
                c01.to_array(),
            ],
            indices,
            normals: None,
        },
        material.clone(),
        light_ray_mask(prim),
    );
    ctx.lights.add(AreaLight::new(
        RectShape::new(origin, edge_u, edge_v, normal),
        material,
        geom_id,
    ));
    debug!(
        "RectLight: origin={:?} edge_u={:?} edge_v={:?} normalize={} radiance={:?}",
        origin, edge_u, edge_v, params.normalize, radiance
    );
}

/// Imports a `UsdLuxDistantLight`. The light points down its local -Z, so
/// the world direction it travels toward is that axis under the prim's
/// transform. `inputs:angle` is the source's angular *diameter* in degrees
/// (default 0.53 — the sun's).
///
/// Units follow the spec. `intensity × color × 2^exposure` is the source's
/// **luminance** in nits; with `inputs:normalize` it is divided by
/// [`distant_size_factor`] (`π·sin²θmax`), which makes it the
/// **illuminance** in lux on a surface facing the light. A zero angle is a
/// delta light, whose `intensity` both the spec (`sizeFactor = 1`) and
/// hdEmbree deliver as that illuminance.
///
/// crust widens a cone narrower than `MIN_DISTANT_ANGLE_DEG` (see
/// [`DistantLight`](crate::light::DistantLight)) rather than carrying a
/// delta light, and the widening preserves the *illuminance* the
/// authored cone would have delivered — so it moves the penumbra and nothing
/// else. The light has no scene geometry, so it is light-list-only: bounce
/// rays find it by escaping along a direction inside its cone.
pub(super) fn emit_distant_light(
    lights: &mut LightList,
    prim: &Prim,
    light: &UsdDistantLight,
    world_xf: GMat4,
) {
    let direction = world_xf.transform_vector3(Vec3::NEG_Z);
    if direction.length_squared() < 1e-12 {
        warn!("DistantLight has a degenerate orientation — skipped");
        return;
    }
    let angle = attr_f32(&light.angle_attr()).unwrap_or(0.53).max(0.0);
    let params = lux_params(prim, light);
    let direction = Vec3A::new(direction.x, direction.y, direction.z);

    // Everything becomes the illuminance the *authored* cone delivers to a
    // facing surface, computed in f64, and `DistantLight::new` spreads it
    // over the cone crust actually samples. That one quantity covers all
    // four cases exactly: nits (`L·π·sin²θ`), lux (`normalize`), a delta
    // light (its intensity *is* lux), and a cone narrower than the widening
    // floor — or so narrow its f32 cosine rounds to 1 — whose illuminance
    // the widened cone keeps.
    let illuminance = if angle == 0.0 {
        params.emission
    } else {
        let per_nit = distant_illuminance(1.0, angle);
        let luminance = if params.normalize {
            params.emission / distant_size_factor(angle)
        } else {
            params.emission
        };
        luminance * per_nit
    };
    let light = CoreDistantLight::new(direction, illuminance, angle);
    let mask = infinite_light_escape_mask(prim);
    debug!(
        "DistantLight {}: direction={:?} angle={}° normalize={} camera-visible={}",
        prim.path(),
        direction,
        angle,
        params.normalize,
        mask.sees(crate::ray::MASK_CAMERA)
    );
    lights.add_masked(light, mask);
}

/// Imports a `UsdLuxDomeLight` as an infinite environment.
///
/// `inputs:texture:file` is resolved against the USD layer's directory and
/// handed to the host's [`AssetLoader`](crate::scene::AssetLoader) —
/// crust-core decodes nothing itself, and decodes one file once however many
/// domes name it. Without a file, or when the host declines, the dome is its
/// uniform `intensity × color × 2^exposure` (× the colour temperature's
/// blackbody, when enabled).
///
/// Only `latlong` is supported; `inputs:texture:format` values that mean
/// anything else warn and fall back to the uniform colour rather than
/// silently mapping the image wrongly. The prim's rotation orients the sky.
pub(super) fn emit_dome_light(
    lights: &mut LightList,
    prim: &Prim,
    light: &DomeLight,
    world_xf: GMat4,
    caches: &mut ImportCaches,
) {
    // `normalize` does not apply to a dome (its sizeFactor is 1).
    let tint = lux_params(prim, light).emission;

    let format = attr_token(&light.texture_format_attr());
    let map = match dome_texture_path(light, caches.stage_path) {
        Some(texture) => match format.as_deref() {
            // `automatic` infers from the image; for the equirectangular
            // images a dome light normally carries that means latlong.
            None | Some("latlong") | Some("automatic") => caches.load_cached(
                |c| &mut c.environments,
                texture,
                |assets, texture| assets.load_environment(texture).map(Arc::new),
                |texture, loaded| {
                    if loaded.is_none() {
                        warn!(
                            "DomeLight at {}: could not load {} — falling back to \
                             the uniform colour",
                            prim.path(),
                            texture.display()
                        );
                    }
                },
            ),
            Some(other) => {
                warn!(
                    "DomeLight at {}: texture:format \"{other}\" is not supported \
                     (only latlong) — falling back to the uniform colour",
                    prim.path()
                );
                None
            }
        },
        None => None,
    };

    // Only the rotation orients the sky; a dome is at infinity, so its
    // translation and scale are meaningless.
    let m = world_xf.to_cols_array_2d();
    let rotation = Mat3A::from_cols(
        Vec3A::new(m[0][0], m[0][1], m[0][2]).normalize_or(Vec3A::X),
        Vec3A::new(m[1][0], m[1][1], m[1][2]).normalize_or(Vec3A::Y),
        Vec3A::new(m[2][0], m[2][1], m[2][2]).normalize_or(Vec3A::Z),
    );

    let mask = infinite_light_escape_mask(prim);
    debug!(
        "Imported DomeLight at {} (tint={:?}, {}, camera-visible={})",
        prim.path(),
        tint,
        match &map {
            Some(m) => format!("{}x{} environment map", m.width(), m.height()),
            None => "uniform".to_string(),
        },
        mask.sees(crate::ray::MASK_CAMERA)
    );
    lights.add_masked(CoreDomeLight::new(tint, map, rotation), mask);
}

/// The dome's `inputs:texture:file` as a filesystem path.
///
/// Goes through [`asset_value_path`], which prefers openusd's `resolved_path()`
/// — anchored against the layer that *authored* the path, not the root layer.
/// That distinction only shows up once a stage has depth: the Moana island's
/// lights author `../textures/islandsun.exr` relative to `usd/island.usda`, so
/// a root layer sitting anywhere else would otherwise resolve it against the
/// wrong directory and silently fall back to the dome's uniform colour.
fn dome_texture_path(light: &DomeLight, stage_path: &Path) -> Option<std::path::PathBuf> {
    let value = value_at(&light.texture_file_attr())?;
    asset_value_path(&value, stage_path)
}
