//! Material binding and dispatch: `MaterialBindingAPI` resolution, the
//! per-stage material cache, and the decoders for `crust:openpbr`,
//! `PxrDisneyBsdf`, MaterialX references and asset paths.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use glam::Vec3A;
use openusd::sdf;
use openusd::usd::{Prim, Stage};
use openusd_schemas::shade::{
    Connectable, Material as UsdMaterial, MaterialBindingAPI, ProducerFilter, Shader,
    TerminalSource,
};
use tracing::{debug, warn};

use crate::material::{DispRemap, Displacement, DisplacementValue, Material, OpenPBR};

use super::attrs::custom_f32;
use super::preview::{preview_displacement, preview_surface_material};
use super::time::eval_time;
use super::{ImportCaches, prim_at};

/// Memoizes resolved materials by binding path (and shares one default),
/// so prims bound to the same USD material get pointer-identical Arcs —
/// which is what lets `MeshKey` recognize shared mesh geometry.
#[derive(Default)]
pub(super) struct MaterialCache {
    pub(super) by_path: HashMap<(u32, String), BoundMaterial>,
    pub(super) default: Option<Arc<dyn Material>>,
    /// Resolved `.ptx` path -> the opened texture, or `None` if it could not
    /// be opened. Keyed by filesystem path, so it needs no epoch scoping.
    pub(super) ptex: HashMap<(String, crate::ColorSpace), Option<Arc<dyn crate::PtexTexture>>>,
    /// `(resolved path, colour space)` -> the opened UV texture. Keyed by
    /// filesystem path for the same reason as `ptex`, and by colour space
    /// because the same file can legitimately be read both ways (a packed ORM
    /// map is raw; the albedo beside it is display-encoded) and the decode
    /// happens once, at load.
    pub(super) textures: HashMap<(String, crate::ColorSpace), Option<Arc<dyn crate::Texture2D>>>,
    /// Which stage the prototype-scoped entries belong to; see
    /// [`MaterialCache::key`]. Kept in step with [`ImportCaches::epoch`].
    pub(super) epoch: u32,
    /// The displacement of the `.mtlx` material the last resolution loaded,
    /// handed from [`load_mtlx_material`] to [`resolve_bound`], which takes it
    /// straight after: the document is compiled once, for both.
    mtlx_displacement: Option<Arc<crate::materialx::MtlxDisplacement>>,
}

impl MaterialCache {
    pub(super) fn default_material(&mut self) -> Arc<dyn Material> {
        self.default.get_or_insert_with(default_material).clone()
    }

    /// Cache key for a bound material path.
    ///
    /// Authored scene paths are stable across stages, so they key on the
    /// path alone and a material shared by several subtrees resolves to
    /// one `Arc` — which is what lets the mesh cache deduplicate across
    /// them. Paths *inside* a prototype are not stable: prototypes are
    /// numbered per composition, so under the streaming importer every
    /// stage has its own `/__Prototype_0`, and keying those on the path
    /// alone hands one chunk's material to the next chunk's geometry.
    /// Those are therefore scoped by epoch.
    ///
    /// This bit the streaming importer for real: on the Moana island it
    /// silently merged distinct meshes onto shared materials, losing
    /// 5 835 258 triangles. No single element reproduces it — it needs
    /// two chunks that both carry prototype-internal materials.
    pub(super) fn key(&self, path: &str) -> (u32, String) {
        let epoch = if path.starts_with("/__Prototype") {
            self.epoch
        } else {
            0
        };
        (epoch, path.to_string())
    }
}

/// A prim's resolved material and the displacement it defines, if any.
///
/// Resolved together and cached together, per `(epoch, path)`: displacement
/// is a property of the material, but it is consumed once at import rather
/// than per hit, so it lives beside the [`Material`] instead of inside it.
/// `displacement` is always `None` under `CRUST_DISPLACE=0`, and every
/// downstream step keys off its presence — so the off side is the code path
/// that existed before displacement, not an approximation of it.
#[derive(Clone)]
pub(super) struct BoundMaterial {
    pub(super) material: Arc<dyn Material>,
    pub(super) displacement: Option<Arc<Displacement>>,
}

/// The binding purpose a final render resolves: USD's `full`, falling back to
/// the all-purpose binding (`compute_bound_material` does the fallback).
/// `preview` is for interactive proxies and is never consulted.
const RENDER_BINDING_PURPOSE: &str = "full";

/// The material bound to `prim` for a final render, resolved the way
/// `UsdShadeMaterialBindingAPI::ComputeBoundMaterial` does.
///
/// Two things this has to get right that a direct lookup on the prim does not:
///
/// - **Inheritance.** A binding on an ancestor applies to every prim beneath
///   it, and production assets bind on the group: ALab binds each asset's
///   `GEO` scope, never its meshes. openusd-schemas' `compute_bound_material`
///   walks the ancestors (binding strength and collection bindings included),
///   but only from a prim that *carries* `MaterialBindingAPI`, and
///   `MaterialBindingAPI::get` returns `None` for any other — which is every
///   mesh under a bound group. So the walk starts at the nearest ancestor
///   that has the API. Skipping the API-less prims in between loses nothing:
///   USD disregards a binding on a prim without the API applied.
/// - **Purpose.** ALab authors `material:binding:full` and
///   `material:binding:preview` and almost never the all-purpose relationship,
///   so asking for `""` alone found nothing on 7 256 prims.
fn bound_material(stage: &Stage, prim: &Prim) -> Option<sdf::Path> {
    let mut path = Some(prim.path().clone());
    while let Some(p) = path {
        if p.is_abs_root() {
            return None;
        }
        if let Ok(Some(api)) = MaterialBindingAPI::get(stage, p.clone()) {
            return api
                .compute_bound_material(RENDER_BINDING_PURPOSE)
                .ok()
                .flatten();
        }
        path = p.parent();
    }
    None
}

/// The material bound to `prim` — [`resolve_bound`] for a prim that cannot
/// be displaced (spheres, curves).
pub(super) fn resolve_material(
    stage: &Stage,
    prim: &Prim,
    caches: &mut ImportCaches<'_>,
) -> Arc<dyn Material> {
    resolve_bound(stage, prim, caches).material
}

/// The material bound to `prim`, with its displacement.
pub(super) fn resolve_bound(
    stage: &Stage,
    prim: &Prim,
    caches: &mut ImportCaches<'_>,
) -> BoundMaterial {
    let mat_path = bound_material(stage, prim);

    let Some(mat_path) = mat_path else {
        debug!(
            "{} has no material binding — using default grey OpenPBR",
            prim.path()
        );
        return BoundMaterial {
            material: caches.materials.default_material(),
            displacement: None,
        };
    };

    let key = caches.materials.key(mat_path.as_str());
    if let Some(hit) = caches.materials.by_path.get(&key) {
        return hit.clone();
    }
    // Per *distinct* material, not per binding: a stage binding one material
    // to 10 000 prims logs this once. The key carries the cache epoch, which
    // is what keeps one streamed chunk's `/__Prototype_N` apart from the
    // next's — see `MaterialCache::key`.
    debug!("Resolving material {mat_path} (epoch {})", key.0);
    caches.materials.mtlx_displacement = None;
    let material = resolve_material_uncached(stage, &mat_path, caches);
    let mtlx = caches.materials.mtlx_displacement.take();
    let displacement = if crate::config().displace {
        resolve_displacement(stage, &mat_path, mtlx, caches).map(Arc::new)
    } else {
        None
    };
    if let Some(d) = &displacement {
        debug!("Material {mat_path}: {d:?}");
    }
    let resolved = BoundMaterial {
        material,
        displacement,
    };
    caches.materials.by_path.insert(key, resolved.clone());
    resolved
}

/// The scalar displacement a material defines, read from whichever of the
/// three authoring paths it uses: RenderMan's `PxrDisplace`, a MaterialX
/// `displacementshader`, or `UsdPreviewSurface.inputs:displacement`. `None`
/// when it defines none — the common case, which costs one child scan.
fn resolve_displacement(
    stage: &Stage,
    mat_path: &sdf::Path,
    mtlx: Option<Arc<crate::materialx::MtlxDisplacement>>,
    caches: &mut ImportCaches<'_>,
) -> Option<Displacement> {
    let found = if let Some(shader) = child_shader(stage, mat_path, "PxrDisplace") {
        pxr_displacement(stage, mat_path, &shader, caches)
    } else if let Some(field) = mtlx {
        Some(Displacement::new(DisplacementValue::Field {
            field,
            scale: 1.0,
        }))
    } else {
        preview_displacement_of(stage, mat_path, caches)
    }?;
    // An authored bound on the material; a mesh prim's own overrides it.
    let bound = custom_f32(&prim_at(stage, mat_path.clone()), "crust:displacementBound");
    Some(found.with_authored_bound(bound))
}

/// The material's child `Shader` with this `info:id`, if any.
fn child_shader(stage: &Stage, mat_path: &sdf::Path, id: &str) -> Option<Shader> {
    let children = prim_at(stage, mat_path.clone()).children().ok()?;
    let child = children.iter().find(|c| {
        matches!(
            c.attribute("info:id").get_at::<sdf::Value>(eval_time()),
            Ok(Some(sdf::Value::Token(t))) if t.as_str() == id
        )
    })?;
    Shader::get(stage, child.path().clone()).ok().flatten()
}

/// The value a shader input carries, its connection to the Material's
/// interface followed — how the island authors every parameter.
fn input_value(shader: &Shader, name: &str) -> Option<sdf::Value> {
    shader
        .input(name)
        .value_producing_attributes(ProducerFilter::Any)
        .ok()?
        .into_iter()
        .find_map(|a| {
            a.attribute()
                .get_at::<sdf::Value>(eval_time())
                .ok()
                .flatten()
        })
}

fn input_f32(shader: &Shader, name: &str) -> Option<f32> {
    match input_value(shader, name)? {
        sdf::Value::Float(f) => Some(f),
        sdf::Value::Double(d) => Some(d as f32),
        sdf::Value::Int(i) => Some(i as f32),
        _ => None,
    }
}

/// The shader whose output drives `shader.inputs:<name>`, if one does.
fn upstream_shader(stage: &Stage, shader: &Shader, name: &str) -> Option<Shader> {
    let produced = shader
        .input(name)
        .value_producing_attributes(ProducerFilter::ShaderOutputsOnly)
        .ok()?;
    let source = produced.first()?;
    Shader::get(stage, source.path().prim_path()).ok().flatten()
}

/// RenderMan's `PxrDisplace`, read the way `PxrDisneyBsdf` is: off the
/// Material's interface where the network is wired to it.
///
/// The offset is `dispAmount · T(map)`: `dispAmount` authored on the shader or
/// connected to the interface (`inputs:dispScale` on the island);
/// `dispScalar` either a value or the chains the island authors:
/// `PxrPtexture → [PxrDispTransform] →`, its Ptex read raw from the
/// texture's `filename` (connected to `inputs:displacementMap`) and remapped
/// by the transform ([`DispRemap`]); or, as isDunesA's `soil` does, a
/// `PxrBlend` multiply (`operation = 18`) of two `PxrPtexture`s in place of
/// the one, their red channels multiplied. Any other network driving
/// `dispScalar` (another blend operation, a UV `PxrTexture`) is refused with a
/// warning, and so are `dispVector` / `modelDispVector`.
fn pxr_displacement(
    stage: &Stage,
    mat_path: &sdf::Path,
    displace: &Shader,
    caches: &mut ImportCaches<'_>,
) -> Option<Displacement> {
    for vector in ["dispVector", "modelDispVector"] {
        let input = displace.input(vector);
        let connected = input
            .value_producing_attributes(ProducerFilter::ShaderOutputsOnly)
            .ok()
            .is_some_and(|p| !p.is_empty());
        if connected {
            warn!(
                "Material {mat_path}: PxrDisplace.{vector} is vector displacement, which \
                 crust does not apply — ignored"
            );
        }
    }
    let amount = input_f32(displace, "dispAmount").filter(|a| a.is_finite() && *a != 0.0)?;

    let Some(mut source) = upstream_shader(stage, displace, "dispScalar") else {
        // A constant scalar: a uniform offset.
        let s = input_f32(displace, "dispScalar").unwrap_or(0.0);
        let c = amount * s;
        return (c != 0.0 && c.is_finite())
            .then(|| Displacement::new(DisplacementValue::Constant(c)));
    };
    let mut remap = DispRemap::None;
    if shader_info_id(&source).as_deref() == Some("PxrDispTransform") {
        // RenderMan's defaults for anything unauthored.
        let mode = input_f32(&source, "dispRemapMode").unwrap_or(0.0) as i32;
        let center = input_f32(&source, "dispCenter").unwrap_or(0.5);
        let depth = input_f32(&source, "dispDepth").unwrap_or(1.0);
        let height = input_f32(&source, "dispHeight").unwrap_or(1.0);
        remap = match mode {
            1 => DispRemap::Centered { center },
            2 => DispRemap::DepthHeight {
                center,
                depth,
                height,
            },
            _ => DispRemap::None,
        };
        source = upstream_shader(stage, &source, "dispScalar")?;
    }
    let refuse = |what: &Shader| {
        warn!(
            "Material {mat_path}: PxrDisplace.dispScalar is driven by {} ({:?}), which \
             crust does not evaluate — the surface is not displaced",
            what.path(),
            shader_info_id(what)
        );
    };
    let sources = match shader_info_id(&source).as_deref() {
        Some("PxrPtexture") => vec![source],
        // RenderMan's `PxrBlend` operation 18 is multiply — the island also
        // uses it to mask its bump colour. `resultR` of the blend is the
        // product of the two inputs' red channels.
        Some("PxrBlend") if input_f32(&source, "operation").map(|o| o as i32) == Some(18) => {
            let top = upstream_shader(stage, &source, "topRGB");
            let bottom = upstream_shader(stage, &source, "bottomRGB");
            match (top, bottom) {
                (Some(t), Some(b))
                    if [&t, &b]
                        .iter()
                        .all(|s| shader_info_id(s).as_deref() == Some("PxrPtexture")) =>
                {
                    vec![t, b]
                }
                _ => {
                    refuse(&source);
                    return None;
                }
            }
        }
        _ => {
            refuse(&source);
            return None;
        }
    };
    let mut maps = Vec::with_capacity(sources.len());
    for tex in &sources {
        let file = tex
            .input("filename")
            .value_producing_attributes(ProducerFilter::Any)
            .ok()?
            .into_iter()
            .find_map(|a| attribute_asset_path(a.attribute(), caches.stage_path))?;
        maps.push(crate::PtexRef(load_ptex(
            &file,
            crate::ColorSpace::Raw,
            caches,
        )?));
    }
    Some(Displacement::new(DisplacementValue::Ptex {
        maps,
        remap,
        scale: amount,
    }))
}

/// The preview surface's `inputs:displacement`, when the material shades
/// with a `UsdPreviewSurface`.
fn preview_displacement_of(
    stage: &Stage,
    mat_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
) -> Option<Displacement> {
    let mat = UsdMaterial::get(stage, mat_path.clone()).ok().flatten()?;
    let resolved = mat
        .compute_surface_source(SURFACE_RENDER_CONTEXTS)
        .ok()
        .flatten()?;
    let shader = resolved.sources().iter().find_map(TerminalSource::shader)?;
    if shader_info_id(shader).as_deref() != Some("UsdPreviewSurface") {
        return None;
    }
    preview_displacement(stage, mat_path, shader, caches)
}

/// Render contexts `compute_surface_source` is asked for, strongest first.
///
/// openusd 0.6 took no argument: it tried the universal terminal, then every
/// authored context in alphabetical order. 0.7 takes the preference list
/// explicitly and appends the universal context *last* unless it is named, so
/// the empty string leads here to keep a plain `outputs:surface` winning over
/// any render-specific one. `glslfx` is the preview context and the only
/// namespaced one crust can decode; an `ri` surface is a PxrDisneyBsdf, which
/// `has_shader_id` catches before this call is ever reached.
const SURFACE_RENDER_CONTEXTS: &[&str] = &["", "glslfx"];

fn resolve_material_uncached(
    stage: &Stage,
    mat_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
) -> Arc<dyn Material> {
    // A material whose whole definition is a reference into a `.mtlx` composes
    // to a prim with a `Material` type name and *nothing inside it*, because
    // openusd ships no MaterialX file-format plugin. So every schema query
    // below fails and the surface falls back to grey — which is exactly what
    // the MaterialX Teapot and Lion did before this existed.
    //
    // Consulted lazily, at each point where the USD path gives up, rather than
    // first: finding it means walking the prim's composition graph, and a
    // stage of ordinary USD materials should not pay for that.
    macro_rules! try_mtlx {
        () => {
            if let Some((file, node)) = mtlx_reference(stage, mat_path)
                && let Some(m) = load_mtlx_material(&file, &node, caches)
            {
                debug!(
                    "Material {mat_path} resolved through the MaterialX reference {}</{node}>",
                    file.display()
                );
                return m;
            }
        };
    }

    let mat = match UsdMaterial::get(stage, mat_path.clone()) {
        Ok(Some(m)) => m,
        _ => {
            try_mtlx!();
            warn!(
                "Material at {} not resolvable — using default grey OpenPBR",
                mat_path
            );
            return default_material();
        }
    };

    // Checked before asking for the surface source, because that answer cannot
    // be trusted to name the network that matters. A material carrying several
    // render-context outputs — the Moana island authors `outputs:ri:surface`
    // (PxrDisneyBsdf), `outputs:glslfx:surface` (UsdPreviewSurface) and
    // `outputs:ri:displacement` on every prim — resolves through
    // `compute_surface_source` to the *preview* shader, whose inputs are all
    // `.connect`ed to the material's interface rather than authored as values.
    // Decoding that gives a material with every parameter at its default: the
    // island rendered uniformly pale and glossy instead of matte dark rock.
    if has_shader_id(stage, mat_path, "PxrDisneyBsdf") {
        debug!(
            "Material {mat_path}: PxrDisneyBsdf child shader — decoding it off the Material prim"
        );
        return Arc::new(disney_to_openpbr(stage, mat_path, caches));
    }

    // openusd 0.7 hands back the whole resolved terminal — every source
    // driving it, in connection order — where 0.6 returned the one shader.
    // The first source whose endpoint is a `Shader`-typed prim is that shader;
    // a source can point at an untyped prim, which is USD's invalid-shader
    // result and not something to decode.
    let resolved = mat
        .compute_surface_source(SURFACE_RENDER_CONTEXTS)
        .ok()
        .flatten();
    let Some(shader) = resolved
        .as_ref()
        .and_then(|terminal| terminal.sources().iter().find_map(TerminalSource::shader))
    else {
        // The MaterialX case reaches here when the material prim itself
        // composed (a wrapper layer `over`s it, so the prim exists) but
        // its shader network lives in the unreadable `.mtlx`.
        try_mtlx!();
        warn!(
            "Material {} has no surface shader — using default grey OpenPBR",
            mat_path
        );
        return default_material();
    };

    let shader_id = shader_info_id(shader);
    debug!("Material {mat_path}: surface shader id = {shader_id:?}");
    match shader_id.as_deref() {
        Some("crust:openpbr") => decode_crust_openpbr(shader),
        Some("UsdPreviewSurface") => {
            // The preview surface may still be the Ptex-driven one — the Moana
            // island wires its `diffuseColor` to a Ptex node — so consult the
            // material's own interface input either way.
            preview_surface_material(stage, mat_path, shader, caches)
        }
        Some("PxrDisneyBsdf") => Arc::new(disney_to_openpbr(stage, mat_path, caches)),
        Some(other) => {
            warn!(
                "Unrecognized shader id '{}' at {} — using default grey OpenPBR",
                other, mat_path
            );
            default_material()
        }
        None => {
            warn!(
                "Shader at {} has no info:id — using default grey OpenPBR",
                mat_path
            );
            default_material()
        }
    }
}

fn default_material() -> Arc<dyn Material> {
    Arc::new(OpenPBR::diffuse(Vec3A::new(0.5, 0.5, 0.5)))
}

pub(super) fn shader_info_id(shader: &Shader) -> Option<String> {
    // `Shader::id()` is the higher-level accessor and does the correct
    // `get::<String>()` (which extracts from both String and Token variants).
    if let Ok(Some(id)) = shader.id() {
        return Some(id);
    }
    // Fallback for older openusd revisions or shaders that author info:id
    // via a raw attribute rather than the schema helper.
    shader
        .attribute("info:id")
        .get_at::<sdf::Value>(eval_time())
        .ok()
        .flatten()
        .and_then(|v| match v {
            // `Token` carries an interned `tf::Token`, `String` a plain
            // `String`, so the two arms cannot bind the same name.
            sdf::Value::Token(t) => Some(t.as_str().to_owned()),
            sdf::Value::String(t) => Some(t),
            _ => None,
        })
}

/// Whether the material has a child `Shader` prim with this `info:id`.
///
/// Cheaper and more reliable than resolving a render-context output for the
/// question actually being asked — "does this material shade with X" — since a
/// material may declare several context outputs and USD gives no ordering
/// between them without a configured render context.
fn has_shader_id(stage: &Stage, mat_path: &sdf::Path, id: &str) -> bool {
    let Ok(children) = prim_at(stage, mat_path.clone()).children() else {
        return false;
    };
    children.iter().any(
        |c| match c.attribute("info:id").get_at::<sdf::Value>(eval_time()) {
            Ok(Some(sdf::Value::Token(t))) => t.as_str() == id,
            Ok(Some(sdf::Value::String(t))) => t == id,
            _ => false,
        },
    )
}

/// Maps RenderMan's `PxrDisneyBsdf` onto [`OpenPBR`].
///
/// The Moana island — the reason this exists — shades every surface with it,
/// so without this arm the whole dataset resolves to flat grey. Both models
/// descend from Burley's, which makes most of the mapping direct; the
/// parameters are read off the **Material** prim rather than the shader, since
/// that is where the island authors them (the shader's inputs are all
/// `.connect`ed to the material's interface inputs, and following those
/// connections would buy nothing here).
///
/// Not mapped, because OpenPBR has no equivalent lobe: `subsurface*`,
/// `diffuseTransmission`, `specularTint`, `scatter*`. Those surfaces render as
/// opaque dielectrics.
///
/// `sheen` is deliberately **not** mapped onto `fuzz_weight`, despite both
/// being "the retroreflective one". Disney's sheen is a small term *added* at
/// grazing angles; OpenPBR's fuzz is a Charlie layer *mixed over* everything
/// beneath it, so at the island's authored `sheen = 1` the fuzz lobe replaced
/// the base entirely — the lava rocks lost their Ptex detail and rendered as
/// smooth blue-grey plastic. A weight is not a weight just because it shares a
/// name, and there is no honest scalar between the two.
fn disney_to_openpbr(
    stage: &Stage,
    mat_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
) -> OpenPBR {
    let prim = prim_at(stage, mat_path.clone());
    // Called with the whole attribute name, `inputs:` included: a literal, so
    // reading an input allocates no name.
    let f = |n: &str| custom_f32(&prim, n);
    let c = |n: &str| custom_vec3(&prim, n);

    let mut o = OpenPBR::default();

    // `inputs:baseColor` reaches the BSDF through a `PxrColorCorrect` with
    // gamma 1/2.2, i.e. the authored value is display-encoded and the shader
    // decodes it to linear. Do the same, or every surface renders washed out.
    if let Some(rgb) = c("inputs:baseColor") {
        o.base_color = srgb_to_linear(rgb);
    }
    if let Some(v) = f("inputs:metallic") {
        o.base_metalness = v;
    }
    if let Some(v) = f("inputs:roughness") {
        o.specular_roughness = v;
    }
    if let Some(v) = f("inputs:ior") {
        o.specular_ior = v;
    }
    if let Some(v) = f("inputs:anisotropic") {
        o.specular_roughness_anisotropy = v;
    }
    if let Some(v) = f("inputs:clearcoat") {
        o.coat_weight = v;
    }
    // Gloss is the complement of roughness.
    if let Some(v) = f("inputs:clearcoatGloss") {
        o.coat_roughness = (1.0 - v).clamp(0.0, 1.0);
    }
    // Either name turns up across the island's materials.
    if let Some(v) = f("inputs:specularTransmission").or_else(|| f("inputs:refractionGain")) {
        o.transmission_weight = v;
    }
    // A cutout (`Material::opacity`), not transmission.
    if let Some(v) = f("inputs:alpha") {
        o.geometry_opacity = v;
    }
    if let Some(v) = f("inputs:thinSurface") {
        o.geometry_thin_walled = v != 0.0;
    }

    o.base_color_ptex = material_ptex(stage, mat_path, caches);
    debug!(
        "PxrDisneyBsdf {mat_path}: baseColor={:?} (authored {:?}) roughness={} metalness={} \
         fuzz={} coat={} ior={} ptex={}",
        o.base_color,
        c("inputs:baseColor"),
        o.specular_roughness,
        o.base_metalness,
        o.fuzz_weight,
        o.coat_weight,
        o.specular_ior,
        o.base_color_ptex.is_some()
    );
    o
}

/// The `.mtlx` file and material node a `Material` prim references, if any.
///
/// Read off the *layer specs* rather than the composed prim, because there is
/// nothing composed to read: without a MaterialX file-format plugin the
/// reference resolves to no layer at all, so the arc survives only as authored
/// metadata. The first `.mtlx` found wins.
fn mtlx_reference(stage: &Stage, mat_path: &sdf::Path) -> Option<(std::path::PathBuf, String)> {
    // The composed stage path is not where the reference is *authored*: a shot
    // layer referencing `teapot.usda` sees the material at
    // `/World/Teapot/Looks/TeapotCeramic`, while the arc is authored at
    // `/Teapot/Looks/TeapotCeramic` inside the asset layer. Only the prim's
    // composition graph knows both, so the candidate spec paths come from its
    // nodes rather than from the stage path.
    let mut paths = vec![mat_path.clone()];
    if let Ok(graph) = prim_at(stage, mat_path.clone()).prim_index().graph() {
        for node in graph.all_nodes() {
            let p = node.path().clone();
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
    }

    for id in stage.layer_identifiers() {
        let Some(layer) = stage.layer(&id) else {
            continue;
        };
        // Every candidate path against every layer. The cross product is a
        // handful of lookups — a material's graph has a few nodes and a stage
        // a few layers — and it sidesteps having to map a node's `LayerId`
        // back to an identifier, which openusd does not expose.
        for path in &paths {
            let Ok(Some(spec)) = layer.prim(path.clone()) else {
                continue;
            };
            let Ok(Some(sdf::Value::ReferenceListOp(list))) = spec.field("references") else {
                continue;
            };
            // Every list-op bucket, because which one a reference lands in
            // depends on whether it was authored as `prepend`, `append` or a
            // bare assignment — and all three mean "this material is that
            // document".
            let items = list
                .explicit_items
                .iter()
                .chain(list.prepended_items.iter())
                .chain(list.appended_items.iter())
                .chain(list.added_items.iter());
            for r in items {
                if !r.asset_path.to_ascii_lowercase().ends_with(".mtlx") {
                    continue;
                }
                // Anchored against the *authoring layer's* directory, the same
                // rule `asset_value_path` follows for textures and for the
                // same reason: `Looks/teapot_ceramic_ldX.mtlx` is relative to
                // `teapot.usda`, which need not be the stage root.
                let base = std::path::Path::new(&id).parent()?.to_path_buf();
                let file = base.join(&r.asset_path);
                let node = crate::materialx::material_node_of(r.prim_path.as_str())
                    .unwrap_or_default()
                    .to_string();
                return Some((file, node));
            }
        }
    }
    None
}

/// Parses a `.mtlx` and turns the named material node into a [`Material`].
///
/// Asset paths inside the document are relative to the document itself (the
/// MaterialX rule), not to the USD layer that referenced it, so texture
/// resolution anchors on `file`'s own directory.
fn load_mtlx_material(
    file: &std::path::Path,
    node: &str,
    caches: &mut ImportCaches<'_>,
) -> Option<Arc<dyn Material>> {
    let dir = file
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();
    // `RefCell` because the loader closure is called from inside the compiler
    // while `caches` would otherwise be mutably borrowed by the outer call.
    let cell = std::cell::RefCell::new(&mut *caches);
    let loader = |asset: &str, space: Option<&str>| -> Option<crate::TextureRef> {
        let mut c = cell.borrow_mut();
        load_uv_texture(
            &dir.join(asset),
            crate::ColorSpace::from_mtlx(space),
            &mut c,
        )
        .map(crate::TextureRef)
    };
    // Billed as (total) minus (what the texture loads already billed): the
    // loader closure runs *inside* this call and adds its own decode time to
    // the same accumulator, so timing the whole thing on top of that would
    // count every texture twice — which showed up as a "Load assets" phase
    // reported at 178% of the parse phase that contains it.
    let started = Instant::now();
    let before = cell.borrow().asset_time;
    let loaded = crate::materialx::load(file, (!node.is_empty()).then_some(node), &loader);
    let nested = cell.borrow().asset_time - before;
    // `cell` is not used past this point, which ends its borrow of `caches`.
    caches.asset_time += started.elapsed().saturating_sub(nested);

    match loaded {
        Ok(l) => {
            if !l.unsupported.is_empty() {
                warn!(
                    "MaterialX {}: no operator for node type(s) {} — those inputs \
                     fall back to their defaults",
                    file.display(),
                    l.unsupported.join(", ")
                );
            }
            if !l.reported.is_empty() {
                warn!(
                    "MaterialX {}: not represented — {}",
                    file.display(),
                    l.reported.join("; ")
                );
            }
            debug!(
                "MaterialX {} -> {} ({} textures resolved{})",
                file.display(),
                l.summary,
                l.textures,
                if l.displacement.is_some() {
                    ", displaced"
                } else {
                    ""
                }
            );
            caches.materials.mtlx_displacement = l.displacement;
            Some(l.material)
        }
        Err(e) => {
            warn!(
                "MaterialX {} not usable ({e}) — falling back",
                file.display()
            );
            None
        }
    }
}

/// Opens a UV texture through the host, memoized by resolved path and colour
/// space. The caller maps its own vocabulary onto the space — MaterialX's
/// `colorspace` through [`crate::ColorSpace::from_mtlx`], UsdUVTexture's
/// `sourceColorSpace` through [`crate::ColorSpace::from_usd`] — since the two
/// disagree on what an absent attribute means.
pub(super) fn load_uv_texture(
    path: &std::path::Path,
    space: crate::ColorSpace,
    caches: &mut ImportCaches<'_>,
) -> Option<Arc<dyn crate::Texture2D>> {
    let key = (path.to_string_lossy().into_owned(), space);
    if let Some(hit) = caches.materials.textures.get(&key) {
        return hit.clone();
    }
    let started = Instant::now();
    let loaded = caches.assets.load_texture(path, space);
    let elapsed = started.elapsed();
    caches.asset_time += elapsed;
    if loaded.is_none() {
        debug!(
            "Texture {} ({space:?}) not loadable by the host",
            path.display()
        );
    }
    caches.materials.textures.insert(key, loaded.clone());
    loaded
}

/// The per-face colour texture a material binds, if any.
///
/// `inputs:surfaceMap` is the interface input both of the island's Ptex shader
/// paths read — `PxrPtexture.filename` for RenderMan and
/// `HwPtexTexture_1.file` for the GL preview both `.connect` to it — so
/// reading it directly gets the file without walking either network.
pub(super) fn material_ptex(
    stage: &Stage,
    mat_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
) -> Option<crate::PtexRef> {
    let prim = prim_at(stage, mat_path.clone());
    let value = prim
        .attribute("inputs:surfaceMap")
        .get_at::<sdf::Value>(eval_time())
        .ok()
        .flatten()?;
    let path = asset_value_path(&value, caches.stage_path)?;

    load_ptex(&path, crate::ColorSpace::Gamma22, caches).map(crate::PtexRef)
}

/// Opens a Ptex file through the host, once per `(resolved path, space)`.
///
/// Keyed on the resolved filesystem path, which — unlike a prototype-scoped
/// scene path — is stable across the streaming importer's stages, so one
/// texture is opened once however many materials or chunks reference it.
/// The space is in the key because the decode happens at open: a file read
/// both as colour and as displacement is two textures, which is correct and
/// rare. Negative results are cached too: a 600 MB file that failed to open
/// should not be retried per material.
pub(super) fn load_ptex(
    path: &std::path::Path,
    space: crate::ColorSpace,
    caches: &mut ImportCaches<'_>,
) -> Option<Arc<dyn crate::PtexTexture>> {
    let key = (path.to_string_lossy().into_owned(), space);
    if let Some(hit) = caches.materials.ptex.get(&key) {
        return hit.clone();
    }
    let started = Instant::now();
    let loaded = caches.assets.load_ptex(path, space);
    caches.asset_time += started.elapsed();
    caches.materials.ptex.insert(key, loaded.clone());
    loaded
}

/// An `asset`-valued attribute as a filesystem path.
///
/// openusd anchors default-sourced asset paths against the layer that authored
/// them and reports the result in `resolved_path` — which is what makes a
/// production stage's `../../../textures/foo.ptx` work at all, since the layer
/// authoring it is nested several directories below the root. The authored
/// string is only a fallback, anchored against the root layer.
pub(super) fn asset_value_path(
    value: &sdf::Value,
    stage_path: &Path,
) -> Option<std::path::PathBuf> {
    let (authored, resolved) = match value {
        sdf::Value::AssetPath(p) => (p.as_str().to_string(), p.resolved_path()),
        sdf::Value::String(p) => (p.clone(), None),
        _ => return None,
    };
    if let Some(r) = resolved
        && !r.is_empty()
    {
        return Some(std::path::PathBuf::from(r));
    }
    if authored.is_empty() {
        return None;
    }
    let candidate = std::path::Path::new(&authored);
    if candidate.is_absolute() {
        return Some(candidate.to_path_buf());
    }
    Some(
        stage_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(candidate),
    )
}

/// [`asset_value_path`] for an attribute, anchoring an **unresolved** relative
/// path against the layer that authored it rather than against the root layer.
///
/// openusd anchors every asset value against its authoring layer, but reports
/// the anchored path only when it names a file that exists — and a
/// `<UDIM>`-tokened texture path never does, since it names a set. So such a
/// value arrives with no `resolved_path` at all, and anchoring it against the
/// root layer was wrong for any texture authored in a sublayer or reference:
/// ALab's look layers sit five directories below `entry.usda` and author
/// `@../../texture/…<UDIM>.exr@`, and 1 718 texture sets failed to load. The
/// strongest spec in the attribute's property stack is the layer whose opinion
/// supplies the value, which is exactly what USD anchors against.
pub(super) fn attribute_asset_path(
    attr: &openusd::usd::Attribute,
    stage_path: &Path,
) -> Option<std::path::PathBuf> {
    let value = attr.get_at::<sdf::Value>(eval_time()).ok().flatten()?;
    let resolved = matches!(&value, sdf::Value::AssetPath(p)
        if p.resolved_path().is_some_and(|r| !r.is_empty()));
    let authored = value.as_str().map(str::to_owned);
    if !resolved
        && let Some(authored) = authored.filter(|a| !a.is_empty() && Path::new(a).is_relative())
        && let Some(layer_dir) = attr
            .property_stack()
            .ok()
            .and_then(|stack| stack.into_iter().next())
            .and_then(|site| Path::new(&site.layer).parent().map(Path::to_path_buf))
            .filter(|d| !d.as_os_str().is_empty())
    {
        return Some(layer_dir.join(authored));
    }
    asset_value_path(&value, stage_path)
}

/// sRGB transfer function, decoding a display-referred colour to linear.
///
/// The plain 2.2 power law rather than the piecewise sRGB curve: it is what
/// the `PxrColorCorrect` gamma node in the island's materials actually
/// applies, and matching the reference render matters more here than matching
/// the standard.
fn srgb_to_linear(c: Vec3A) -> Vec3A {
    Vec3A::new(
        c.x.max(0.0).powf(2.2),
        c.y.max(0.0).powf(2.2),
        c.z.max(0.0).powf(2.2),
    )
}

fn custom_vec3(prim: &Prim, name: &str) -> Option<Vec3A> {
    let v = prim
        .attribute(name)
        .get_at::<sdf::Value>(eval_time())
        .ok()??;
    match v {
        sdf::Value::Vec3f(p) => Some(Vec3A::new(p.x, p.y, p.z)),
        sdf::Value::Vec3d(p) => Some(Vec3A::new(p.x as f32, p.y as f32, p.z as f32)),
        _ => None,
    }
}

/// Decode a `crust:openpbr` shader into the OpenPBR material. Every input
/// name is camelCase mirror of the Rust snake_case, e.g. `base_color` →
/// `inputs:baseColor`, `subsurface_radius_scale` → `inputs:subsurfaceRadiusScale`.
fn decode_crust_openpbr(shader: &Shader) -> Arc<dyn Material> {
    let mut o = OpenPBR::default();

    let f = |n: &str, d: f32| shader_input_f32(shader, n).unwrap_or(d);
    let c = |n: &str, d: Vec3A| shader_input_vec3(shader, n).unwrap_or(d);
    let b = |n: &str, d: bool| shader_input_bool(shader, n).unwrap_or(d);

    // Base
    o.base_weight = f("inputs:baseWeight", o.base_weight);
    o.base_color = c("inputs:baseColor", o.base_color);
    o.base_diffuse_roughness = f("inputs:baseDiffuseRoughness", o.base_diffuse_roughness);
    o.base_metalness = f("inputs:baseMetalness", o.base_metalness);

    // Specular
    o.specular_weight = f("inputs:specularWeight", o.specular_weight);
    o.specular_color = c("inputs:specularColor", o.specular_color);
    o.specular_roughness = f("inputs:specularRoughness", o.specular_roughness);
    o.specular_ior = f("inputs:specularIor", o.specular_ior);
    o.specular_roughness_anisotropy = f(
        "inputs:specularRoughnessAnisotropy",
        o.specular_roughness_anisotropy,
    );

    // Transmission
    o.transmission_weight = f("inputs:transmissionWeight", o.transmission_weight);
    o.transmission_color = c("inputs:transmissionColor", o.transmission_color);
    o.transmission_depth = f("inputs:transmissionDepth", o.transmission_depth);
    o.transmission_scatter = c("inputs:transmissionScatter", o.transmission_scatter);
    o.transmission_scatter_anisotropy = f(
        "inputs:transmissionScatterAnisotropy",
        o.transmission_scatter_anisotropy,
    );
    o.transmission_dispersion_scale = f(
        "inputs:transmissionDispersionScale",
        o.transmission_dispersion_scale,
    );
    o.transmission_dispersion_abbe_number = f(
        "inputs:transmissionDispersionAbbeNumber",
        o.transmission_dispersion_abbe_number,
    );

    // Subsurface
    o.subsurface_weight = f("inputs:subsurfaceWeight", o.subsurface_weight);
    o.subsurface_color = c("inputs:subsurfaceColor", o.subsurface_color);
    o.subsurface_radius = f("inputs:subsurfaceRadius", o.subsurface_radius);
    o.subsurface_radius_scale = c("inputs:subsurfaceRadiusScale", o.subsurface_radius_scale);
    o.subsurface_scatter_anisotropy = f(
        "inputs:subsurfaceScatterAnisotropy",
        o.subsurface_scatter_anisotropy,
    );

    // Fuzz
    o.fuzz_weight = f("inputs:fuzzWeight", o.fuzz_weight);
    o.fuzz_color = c("inputs:fuzzColor", o.fuzz_color);
    o.fuzz_roughness = f("inputs:fuzzRoughness", o.fuzz_roughness);

    // Coat
    o.coat_weight = f("inputs:coatWeight", o.coat_weight);
    o.coat_color = c("inputs:coatColor", o.coat_color);
    o.coat_roughness = f("inputs:coatRoughness", o.coat_roughness);
    o.coat_roughness_anisotropy = f(
        "inputs:coatRoughnessAnisotropy",
        o.coat_roughness_anisotropy,
    );
    o.coat_ior = f("inputs:coatIor", o.coat_ior);
    o.coat_darkening = f("inputs:coatDarkening", o.coat_darkening);

    // Thin film
    o.thin_film_weight = f("inputs:thinFilmWeight", o.thin_film_weight);
    o.thin_film_thickness = f("inputs:thinFilmThickness", o.thin_film_thickness);
    o.thin_film_ior = f("inputs:thinFilmIor", o.thin_film_ior);

    // Emission
    o.emission_luminance = f("inputs:emissionLuminance", o.emission_luminance);
    o.emission_color = c("inputs:emissionColor", o.emission_color);

    // Geometry
    o.geometry_opacity = f("inputs:geometryOpacity", o.geometry_opacity);
    o.geometry_thin_walled = b("inputs:geometryThinWalled", o.geometry_thin_walled);

    Arc::new(o)
}

/// A shader input by its whole attribute name (`inputs:roughness`) — the
/// callers pass literals, so no name is built per read.
fn shader_input_f32(shader: &Shader, attr_name: &str) -> Option<f32> {
    let v = shader
        .attribute(attr_name)
        .get_at::<sdf::Value>(eval_time())
        .ok()??;
    match v {
        sdf::Value::Float(f) => Some(f),
        sdf::Value::Double(d) => Some(d as f32),
        _ => None,
    }
}

fn shader_input_bool(shader: &Shader, attr_name: &str) -> Option<bool> {
    let v = shader
        .attribute(attr_name)
        .get_at::<sdf::Value>(eval_time())
        .ok()??;
    match v {
        sdf::Value::Bool(b) => Some(b),
        _ => None,
    }
}

fn shader_input_vec3(shader: &Shader, attr_name: &str) -> Option<Vec3A> {
    let v = shader
        .attribute(attr_name)
        .get_at::<sdf::Value>(eval_time())
        .ok()??;
    match v {
        sdf::Value::Vec3f(p) => Some(Vec3A::new(p.x, p.y, p.z)),
        // USD encodes color3f as an sdf::Value::Vec3f — no dedicated variant.
        _ => None,
    }
}
