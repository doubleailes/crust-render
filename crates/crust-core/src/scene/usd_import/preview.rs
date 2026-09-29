//! `UsdPreviewSurface` (+ `UsdUVTexture`) → [`OpenPBR`] / [`crate::PreviewSurface`].

use std::sync::Arc;

use glam::Vec3A;
use openusd::sdf;
use openusd::usd::Stage;
use openusd_schemas::shade;
use openusd_schemas::shade::{
    Connectable, ProducerFilter, ReadPreviewSurface, Shader, ShadingAttribute,
};
use tracing::{debug, warn};

use crate::material::{Material, OpenPBR};

use super::ImportCaches;
use super::materials::{attribute_asset_path, load_uv_texture, material_ptex, shader_info_id};
use super::time::eval_time;

/// A `UsdPreviewSurface` material: its constants as an [`OpenPBR`], wrapped in
/// a [`crate::PreviewSurface`] when any input is driven by a `UsdUVTexture`.
///
/// An untextured surface — and one whose every texture connection turned out
/// unusable — stays the plain `OpenPBR` it always was, so it neither builds a
/// UV table nor pays a per-hit evaluation.
pub(super) fn preview_surface_material(
    stage: &Stage,
    mat_path: &sdf::Path,
    shader: &Shader,
    caches: &mut ImportCaches<'_>,
) -> Arc<dyn Material> {
    use crate::material::preview_surface::Target;

    let ps = shade::read_preview_surface(stage, mat_path).ok().flatten();
    let mut base = match &ps {
        Some(ps) => preview_surface_openpbr(ps),
        None => OpenPBR::diffuse(Vec3A::new(0.5, 0.5, 0.5)),
    };
    base.base_color_ptex = material_ptex(stage, mat_path, caches);
    let Some(ps) = ps else {
        return Arc::new(base);
    };

    // `read_preview_surface` reports `Texture` exactly when an input resolves
    // to a `UsdUVTexture` output, which is the question; the walk below then
    // reads the whole node rather than just its file.
    let textured = [
        (Target::DiffuseColor, ps.diffuse_color.texture().is_some()),
        (Target::EmissiveColor, ps.emissive_color.texture().is_some()),
        (Target::Metallic, ps.metallic.texture().is_some()),
        (Target::Roughness, ps.roughness.texture().is_some()),
        // Translucency only: a textured cutout mask is read apart, below.
        (
            Target::Opacity,
            ps.opacity.texture().is_some() && opacity_transmission(&ps),
        ),
        (Target::Ior, ps.ior.texture().is_some()),
        (Target::Clearcoat, ps.clearcoat.texture().is_some()),
        (
            Target::ClearcoatRoughness,
            ps.clearcoat_roughness.texture().is_some(),
        ),
    ];
    let mut inputs = Vec::new();
    let mut varnames: Vec<String> = Vec::new();
    let mut note = |v: Option<String>| {
        if let Some(v) = v
            && !varnames.contains(&v)
        {
            varnames.push(v);
        }
    };
    for (target, is_textured) in textured {
        if is_textured
            && let Some((input, varname)) =
                preview_uv_input(stage, mat_path, shader, target.input_name(), caches)
        {
            note(varname);
            inputs.push((target, input));
        }
    }
    let normal = if ps.normal.texture().is_some() {
        preview_uv_input(stage, mat_path, shader, "normal", caches).map(|(input, varname)| {
            note(varname);
            input
        })
    } else {
        None
    };
    // `opacityThreshold > 0` with a textured opacity: the mask is sampled
    // per hit. (A constant one is already thresholded into
    // `geometry_opacity`.)
    let cutout = if !opacity_transmission(&ps) && ps.opacity.texture().is_some() {
        preview_uv_input(stage, mat_path, shader, "opacity", caches).map(|(input, varname)| {
            note(varname);
            let threshold = ps.opacity_threshold.value().copied().unwrap_or(0.0);
            (input, threshold)
        })
    } else {
        None
    };

    // One chart per mesh: a network whose readers name two primvars shades
    // every texture from the first.
    if varnames.len() > 1 {
        warn!(
            "UsdPreviewSurface at {mat_path}: textures read primvars {varnames:?}; crust \
             carries one chart per mesh and reads '{}' for all of them",
            varnames[0]
        );
    }
    for (name, set) in [
        ("occlusion", ps.occlusion.is_set()),
        ("specularColor", ps.specular_color.is_set()),
    ] {
        if set {
            debug!("UsdPreviewSurface at {mat_path}: {name} is not read");
        }
    }
    if inputs.is_empty() && normal.is_none() && cutout.is_none() {
        return Arc::new(base);
    }
    let mut m = crate::PreviewSurface::new(mat_path.to_string(), base, inputs, normal)
        .with_uv_primvar(varnames.into_iter().next());
    if let Some((input, threshold)) = cutout {
        m = m.with_cutout(input, threshold);
    }
    debug!("Material {mat_path}: {m:?}");
    Arc::new(m)
}

/// Follows one `UsdPreviewSurface` input to the `UsdUVTexture` producing it
/// and reads that node whole: file, colour space, wrap modes, scale, bias,
/// fallback, and the primvar reader behind its `st`.
///
/// Every value is read through `value_producing_attributes(Any)`, so an input
/// wired to the Material's interface — how a published look exposes its
/// file paths — resolves the same as one authored on the node. `None` when the
/// input does not resolve to a texture output crust can use; the surface then
/// keeps that input's constant.
fn preview_uv_input(
    stage: &Stage,
    mat_path: &sdf::Path,
    shader: &Shader,
    name: &str,
    caches: &mut ImportCaches<'_>,
) -> Option<(crate::material::preview_surface::UvInput, Option<String>)> {
    use crate::material::preview_surface::{TexOutput, UvInput, Wrap};
    use shade::tokens as tk;

    let surface_input = shader.input(name);
    let produced = surface_input
        .value_producing_attributes(ProducerFilter::ShaderOutputsOnly)
        .ok()?;
    let source = produced.first()?;
    let output = match source {
        ShadingAttribute::Output(o) => TexOutput::from_name(o.base_name()),
        ShadingAttribute::Input(_) => None,
    };
    let Some(output) = output else {
        warn!(
            "UsdPreviewSurface at {mat_path}: {name} connects to {}, not a UsdUVTexture \
             r/g/b/a/rgb output — using its constant",
            source.path()
        );
        return None;
    };
    if matches!(output, TexOutput::A) {
        // The host samplers return opaque RGB, so the alpha channel reads 1.0
        // whatever the file holds — an approximation worth saying out loud.
        warn!(
            "UsdPreviewSurface at {mat_path}: {name} reads texture alpha ({}), which crust              does not decode — it reads 1.0 before scale/bias",
            source.path()
        );
    }
    let tex = Shader::get(stage, source.path().prim_path())
        .ok()
        .flatten()?;
    if shader_info_id(&tex).as_deref() != Some(tk::SHADER_ID_UV_TEXTURE) {
        return None;
    }

    // The value an input carries, connection followed.
    let value = |input: &shade::Input| -> Option<sdf::Value> {
        let produced = input.value_producing_attributes(ProducerFilter::Any).ok()?;
        produced
            .first()?
            .attribute()
            .get_at::<sdf::Value>(eval_time())
            .ok()
            .flatten()
    };
    let token = |input: &str| value(&tex.input(input)).and_then(|v| v.as_str().map(str::to_owned));
    let float4 = |input: &str| value(&tex.input(input)).and_then(|v| sdf_float4(&v));

    let file = tex
        .input(tk::TEX_FILE)
        .value_producing_attributes(ProducerFilter::Any)
        .ok()
        .and_then(|p| p.into_iter().next())
        .and_then(|a| attribute_asset_path(a.attribute(), caches.stage_path));
    let Some(file) = file else {
        warn!(
            "UsdUVTexture {}: no inputs:file — {name} keeps its constant",
            tex.path()
        );
        return None;
    };
    let space = crate::ColorSpace::from_usd(token(tk::TEX_SOURCE_COLOR_SPACE).as_deref());

    // Which chart the texture reads. crust carries one per mesh (see
    // `mesh_uvs`), so a reader naming another primvar is approximated by it.
    let st = tex.input(tk::TEX_ST);
    let mut varname = None;
    if let Some(reader) = st
        .value_producing_attributes(ProducerFilter::ShaderOutputsOnly)
        .ok()
        .and_then(|p| p.into_iter().next())
    {
        let reader = Shader::get(stage, reader.path().prim_path()).ok().flatten();
        let id = reader.as_ref().and_then(shader_info_id);
        match (&reader, id.as_deref()) {
            (Some(reader), Some(tk::SHADER_ID_PRIMVAR_READER_FLOAT2)) => {
                varname = value(&reader.input(tk::PVR_VARNAME))
                    .and_then(|v| v.as_str().map(str::to_owned));
            }
            _ => warn!(
                "UsdUVTexture {}: st is driven by {id:?}, which is not read — using the \
                 mesh chart unchanged",
                tex.path()
            ),
        }
    }

    let file_name = file.to_string_lossy();
    let tiled = file_name.contains("<UDIM>") || file_name.contains("<UVTILE>");
    // The node's own fallback, else the surface input's authored constant (so
    // a declined texture renders on the surface's constants), else that
    // input's `UsdPreviewSurface` schema default. The node set's own default,
    // opaque black, is the last resort only for an input the schema does not
    // know: a published look routinely connects an input without authoring a
    // constant, and black there is not neutral — ALab's wrench references a
    // roughness map the dataset does not ship, and roughness 0 turned it into
    // a mirror where the schema's 0.5 is an ordinary surface.
    let fallback = float4(tk::TEX_FALLBACK)
        .or_else(|| {
            surface_input
                .attribute()
                .get_at::<sdf::Value>(eval_time())
                .ok()
                .flatten()
                .and_then(|v| sdf_float4(&v))
        })
        .or_else(|| preview_surface_default(name))
        .unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let loaded = load_uv_texture(&file, space, caches).map(crate::TextureRef);
    if loaded.is_none() {
        // DEBUG: the host has already reported *why* at its own level (a
        // missing file is an ERROR there), and `CRUST_TEX=0` declines every
        // texture on purpose — a WARN here would repeat per texture.
        debug!(
            "UsdPreviewSurface at {mat_path}: {name} texture {} not loaded — shading with \
             {fallback:?}",
            file.display()
        );
    }
    let input = UvInput {
        tex: loaded,
        output,
        scale: float4(tk::TEX_SCALE).unwrap_or([1.0; 4]),
        bias: float4(tk::TEX_BIAS).unwrap_or([0.0; 4]),
        fallback,
        wrap: [
            Wrap::from_token(token(tk::TEX_WRAP_S).as_deref()),
            Wrap::from_token(token(tk::TEX_WRAP_T).as_deref()),
        ],
        tiled,
    };
    Some((input, varname))
}

/// A `UsdPreviewSurface` input's schema default, widened to four channels the
/// way [`sdf_float4`] widens an authored value — what the input reads when a
/// texture drives it, the texture fails, and nothing else was authored.
/// Values from the UsdPreviewSurface specification.
fn preview_surface_default(input: &str) -> Option<[f32; 4]> {
    let v = |x: f32| [x; 4];
    let c = |r: f32, g: f32, b: f32| [r, g, b, 1.0];
    Some(match input {
        "diffuseColor" => c(0.18, 0.18, 0.18),
        "emissiveColor" => c(0.0, 0.0, 0.0),
        "specularColor" => c(0.0, 0.0, 0.0),
        "normal" => c(0.0, 0.0, 1.0),
        "metallic" => v(0.0),
        "roughness" => v(0.5),
        "clearcoat" => v(0.0),
        "clearcoatRoughness" => v(0.01),
        "opacity" => v(1.0),
        "ior" => v(1.5),
        "occlusion" => v(1.0),
        _ => return None,
    })
}

/// A shading value widened to four channels, the shape `UsdUVTexture`'s
/// `scale`/`bias`/`fallback` have: a `float4` as authored, a colour with
/// alpha 1, a scalar in every channel.
fn sdf_float4(v: &sdf::Value) -> Option<[f32; 4]> {
    Some(match v {
        sdf::Value::Vec4f(v) => [v.x, v.y, v.z, v.w],
        sdf::Value::Vec4d(v) => [v.x as f32, v.y as f32, v.z as f32, v.w as f32],
        sdf::Value::Vec4h(v) => [v.x.to_f32(), v.y.to_f32(), v.z.to_f32(), v.w.to_f32()],
        sdf::Value::Vec3f(v) => [v.x, v.y, v.z, 1.0],
        sdf::Value::Vec3d(v) => [v.x as f32, v.y as f32, v.z as f32, 1.0],
        sdf::Value::Float(f) => [*f; 4],
        sdf::Value::Double(d) => [*d as f32; 4],
        _ => return None,
    })
}

/// A `UsdPreviewSurface`'s constant inputs as an [`OpenPBR`]. A
/// texture-connected input leaves its field at the default here;
/// [`preview_surface_material`] is what drives it.
fn preview_surface_openpbr(ps: &ReadPreviewSurface) -> OpenPBR {
    let mut o = OpenPBR::default();

    if let Some(rgb) = ps.diffuse_color.value() {
        o.base_color = Vec3A::new(rgb[0], rgb[1], rgb[2]);
    }
    if let Some(m) = ps.metallic.value() {
        o.base_metalness = *m;
    }
    if let Some(r) = ps.roughness.value() {
        o.specular_roughness = *r;
    }
    if let Some(op) = ps.opacity.value() {
        if opacity_transmission(ps) {
            o.transmission_weight = 1.0 - op.clamp(0.0, 1.0);
        } else {
            // The spec's cutout: kept whole at or above the threshold.
            let threshold = ps.opacity_threshold.value().copied().unwrap_or(0.0);
            o.geometry_opacity = if *op >= threshold { 1.0 } else { 0.0 };
        }
    }
    if let Some(rgb) = ps.emissive_color.value() {
        o.emission_color = Vec3A::new(rgb[0], rgb[1], rgb[2]);
        let max = rgb[0].max(rgb[1]).max(rgb[2]);
        if max > 0.0 {
            o.emission_luminance = 1.0;
        }
    }
    if let Some(ior) = ps.ior.value() {
        o.specular_ior = *ior;
    }
    if let Some(c) = ps.clearcoat.value() {
        o.coat_weight = *c;
    }
    if let Some(cr) = ps.clearcoat_roughness.value() {
        o.coat_roughness = *cr;
    }

    o
}

/// Whether a `UsdPreviewSurface`'s `opacity` means translucency, which crust
/// maps to refraction (`transmission_weight = 1 − opacity` at `ior`), rather
/// than a cutout mask.
///
/// The spec gives `opacity` two modes. Under the default `opacityThreshold`
/// of 0 a surface below 1 is translucent, and its `ior` is "the index of
/// refraction to be used for translucent objects" — a dielectric, which is how
/// ALab authors all of its glass (opacity ≈ 0, ior ≈ 1.49). Above 0 it is a
/// mask that keeps or discards each point whole, which is the host cutout
/// `geometry_opacity` names and does not refract at all.
fn opacity_transmission(ps: &ReadPreviewSurface) -> bool {
    ps.opacity_threshold.value().is_none_or(|t| *t <= 0.0)
}
