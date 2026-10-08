//! Material binding and dispatch: `MaterialBindingAPI` resolution, the
//! per-stage material cache, and the decoders for `crust:openpbr`,
//! `PxrDisneyBsdf` and MaterialX references.

use std::collections::HashMap;
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

use crate::color::Space;
use crate::material::{DispRemap, Displacement, DisplacementValue, Material, OpenPBR};

use super::assets::{asset_path, load_ptex, load_uv_texture};
use super::attrs::{
    attr_bool, attr_f32, attr_own_color_space, attr_vec3, custom_color3, custom_f32, custom_token,
    decode_number, decode_text, in_working, value_at,
};
use super::preview::{preview_displacement, preview_surface_material};
use super::{ImportCaches, prim_at};

/// Memoizes resolved materials by binding path (and shares one default),
/// so prims bound to the same USD material get pointer-identical Arcs —
/// which is what lets `MeshKey` recognize shared mesh geometry.
#[derive(Default)]
pub(super) struct MaterialCache {
    pub(super) by_path: HashMap<(u32, String), CachedMaterial>,
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
    /// handed from [`load_mtlx_material`] to [`cached_material`], which keeps
    /// it with the material: the document is compiled once, for both.
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
/// Cached together per `(epoch, path)` ([`CachedMaterial`]), the displacement
/// resolved the first time a mesh asks: displacement is a property of the
/// material, but it is consumed once at import rather
/// than per hit, so it lives beside the [`Material`] instead of inside it.
/// `displacement` is always `None` under `CRUST_DISPLACE=0`, and every
/// downstream step keys off its presence — so the off side is the code path
/// that existed before displacement, not an approximation of it.
#[derive(Clone)]
pub(super) struct BoundMaterial {
    pub(super) material: Arc<dyn Material>,
    pub(super) displacement: Option<Arc<Displacement>>,
}

/// One cached material, its displacement resolved on first demand.
///
/// Lazily, because only a mesh consumes a displacement: a material bound
/// only to spheres or curves never opens its displacement maps. A `.mtlx`
/// material's displacement program is compiled with the document, so it is
/// kept here until then.
pub(super) struct CachedMaterial {
    material: Arc<dyn Material>,
    mtlx: Option<Arc<crate::materialx::MtlxDisplacement>>,
    /// `None` until a mesh asked; then the answer, which may be no
    /// displacement.
    displacement: Option<Option<Arc<Displacement>>>,
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

/// The material bound to `prim`, for a prim that cannot be displaced
/// (spheres, curves): its displacement is not resolved.
pub(super) fn resolve_material(
    stage: &Stage,
    prim: &Prim,
    caches: &mut ImportCaches<'_>,
) -> Arc<dyn Material> {
    match cached_material(stage, prim, caches) {
        Some((key, _)) => caches.materials.by_path[&key].material.clone(),
        None => caches.materials.default_material(),
    }
}

/// The material bound to `prim`, with its displacement.
pub(super) fn resolve_bound(
    stage: &Stage,
    prim: &Prim,
    caches: &mut ImportCaches<'_>,
) -> BoundMaterial {
    let Some((key, mat_path)) = cached_material(stage, prim, caches) else {
        return BoundMaterial {
            material: caches.materials.default_material(),
            displacement: None,
        };
    };
    let entry = &caches.materials.by_path[&key];
    let material = entry.material.clone();
    if let Some(displacement) = &entry.displacement {
        return BoundMaterial {
            material,
            displacement: displacement.clone(),
        };
    }
    let mtlx = entry.mtlx.clone();
    let displacement = if crate::config().displace {
        resolve_displacement(stage, &mat_path, mtlx, caches).map(Arc::new)
    } else {
        None
    };
    if let Some(d) = &displacement {
        debug!("Material {mat_path}: {d:?}");
    }
    if let Some(entry) = caches.materials.by_path.get_mut(&key) {
        entry.displacement = Some(displacement.clone());
    }
    BoundMaterial {
        material,
        displacement,
    }
}

/// Resolves and caches the material bound to `prim`, returning its cache key
/// and path; `None` for an unbound prim, which takes the default.
fn cached_material(
    stage: &Stage,
    prim: &Prim,
    caches: &mut ImportCaches<'_>,
) -> Option<((u32, String), sdf::Path)> {
    let Some(mat_path) = bound_material(stage, prim) else {
        debug!(
            "{} has no material binding — using default grey OpenPBR",
            prim.path()
        );
        return None;
    };
    let key = caches.materials.key(mat_path.as_str());
    if caches.materials.by_path.contains_key(&key) {
        return Some((key, mat_path));
    }
    // Per *distinct* material, not per binding: a stage binding one material
    // to 10 000 prims logs this once. The key carries the cache epoch, which
    // is what keeps one streamed chunk's `/__Prototype_N` apart from the
    // next's — see `MaterialCache::key`.
    debug!("Resolving material {mat_path} (epoch {})", key.0);
    caches.materials.mtlx_displacement = None;
    let material = resolve_material_uncached(stage, &mat_path, caches);
    let mtlx = caches.materials.mtlx_displacement.take();
    caches.materials.by_path.insert(
        key.clone(),
        CachedMaterial {
            material,
            mtlx,
            displacement: None,
        },
    );
    Some((key, mat_path))
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
    // A token or a string, as `has_shader_id` accepts.
    let child = children
        .iter()
        .find(|c| custom_token(c, "info:id").as_deref() == Some(id))?;
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
        .find_map(|a| value_at(a.attribute()))
}

fn input_f32(shader: &Shader, name: &str) -> Option<f32> {
    input_value(shader, name).and_then(decode_number)
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
            .find_map(|a| asset_path(a.attribute(), caches.stage_path))?;
        maps.push(crate::PtexRef(load_ptex(
            &file,
            crate::ColorSpace::RAW,
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
///
/// `mtlx` comes last: an inline MaterialX network (`ND_*` shaders, see
/// [`super::mtlx_network`]) is used when nothing the universal or preview
/// context names is decodable, so a stage that already rendered through its
/// preview surface renders exactly as before.
const SURFACE_RENDER_CONTEXTS: &[&str] = &["", "glslfx", "mtlx"];

/// Render contexts for the `volume` terminal. Only a MaterialX network can
/// drive one (`ND_volume`, a VDF), so the universal terminal is read too, and
/// for a shader that is not MaterialX the terminal is ignored.
const VOLUME_RENDER_CONTEXTS: &[&str] = &["mtlx", ""];

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

    // A `volume` terminal makes the material Typhoon's surface-volume
    // material: the volume is the medium inside the bound geometry, under the
    // MaterialX surface of the same network when there is one, and a
    // transparent medium boundary when there is none.
    if let Some(volume) = terminal_shader(mat.compute_volume_source(VOLUME_RENDER_CONTEXTS))
        .filter(|s| shader_info_id(s).is_some_and(|id| super::mtlx_network::is_mtlx_id(&id)))
    {
        // Each context is checked for MaterialX on its own: an `mtlx`
        // surface that is not a MaterialX shader must not hide a universal
        // one that is.
        let is_mtlx =
            |s: &Shader| shader_info_id(s).is_some_and(|id| super::mtlx_network::is_mtlx_id(&id));
        let surface = terminal_shader(mat.compute_surface_source(&["mtlx"]))
            .filter(is_mtlx)
            .or_else(|| terminal_shader(mat.compute_surface_source(&[""])).filter(is_mtlx));
        if surface.is_none()
            && let Some(other) =
                terminal_shader(mat.compute_surface_source(SURFACE_RENDER_CONTEXTS))
        {
            warn!(
                "Material {mat_path}: the volume terminal is MaterialX but the surface ({}) \
                 is not — the volume is ignored",
                shader_info_id(&other).unwrap_or_default()
            );
        } else if let Some(m) =
            inline_mtlx_material(stage, mat_path, surface.as_ref(), Some(&volume), caches)
        {
            return m;
        }
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
        Some("crust:openpbr") => decode_crust_openpbr(shader, caches.working, caches.luma),
        Some("UsdPreviewSurface") => {
            // The preview surface may still be the Ptex-driven one — the Moana
            // island wires its `diffuseColor` to a Ptex node — so consult the
            // material's own interface input either way.
            preview_surface_material(stage, mat_path, shader, caches)
        }
        Some("PxrDisneyBsdf") => Arc::new(disney_to_openpbr(stage, mat_path, caches)),
        Some(id) if super::mtlx_network::is_mtlx_id(id) => {
            inline_mtlx_material(stage, mat_path, Some(shader), None, caches)
                .unwrap_or_else(default_material)
        }
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

/// The first `Shader`-typed source driving a resolved terminal.
fn terminal_shader(
    resolved: Result<
        Option<openusd_schemas::shade::ResolvedTerminal>,
        openusd_schemas::SchemaError,
    >,
) -> Option<Shader> {
    resolved
        .ok()
        .flatten()
        .and_then(|t| t.sources().iter().find_map(TerminalSource::shader).cloned())
}

/// Builds a material from an inline MaterialX network: the shaders driving
/// the `surface` and `volume` terminals, translated into one document
/// ([`super::mtlx_network`]) and compiled as a `.mtlx`'s would be. Textures
/// arrive as absolute paths, so they resolve against nothing further.
fn inline_mtlx_material(
    stage: &Stage,
    mat_path: &sdf::Path,
    surface: Option<&Shader>,
    volume: Option<&Shader>,
    caches: &mut ImportCaches<'_>,
) -> Option<Arc<dyn Material>> {
    let net = super::mtlx_network::translate(
        stage,
        caches.stage_path,
        surface.map(|s| &**s),
        volume.map(|s| &**s),
    );
    if !net.reported.is_empty() {
        warn!(
            "Material {mat_path}: MaterialX network not fully translated — {}",
            net.reported.join("; ")
        );
    }
    let surface = net.surface.as_deref().and_then(|n| net.doc.find("", n));
    let volume = net.volume.as_deref().and_then(|n| net.doc.find("", n));
    if surface.is_none() && volume.is_none() {
        return None;
    }
    let label = mat_path.as_str().to_string();
    compile_mtlx(&label, std::path::Path::new("/"), caches, |host, luma| {
        crate::materialx::from_compiled(
            crust_mtlx::compile_terminals(&net.doc, surface, volume, host),
            luma,
        )
    })
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
    value_at(&shader.attribute("info:id")).and_then(decode_text)
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
    children
        .iter()
        .any(|c| custom_token(c, "info:id").as_deref() == Some(id))
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
    let c = |n: &str| custom_color3(&prim, n);

    let mut o = OpenPBR {
        luma: caches.luma,
        ..OpenPBR::default()
    };

    // `inputs:baseColor` reaches the BSDF through a `PxrColorCorrect` with
    // gamma 1/2.2, i.e. the authored value is display-encoded and the shader
    // decodes it to linear. Do the same, or every surface renders washed out.
    // `g22_rec709` — the plain power law, not the piecewise sRGB curve: it is
    // what that node applies, and matching the island's reference render
    // matters more here than matching the standard. A `colorSpace` metadatum
    // on the input itself names another space instead; a scope's
    // `colorSpace:name` does not, since it describes linear colour values and
    // this one is display-encoded by convention.
    if let Some(rgb) = c("inputs:baseColor") {
        let source =
            attr_own_color_space(&prim.attribute("inputs:baseColor")).unwrap_or(Space::G22_REC709);
        o.base_color = crate::color::convert(rgb, source, caches.working);
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
                // rule `asset_path` follows for textures and for the
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
    let dir = file.parent().unwrap_or(std::path::Path::new("."));
    let label = file.display().to_string();
    compile_mtlx(&label, dir, caches, |host, luma| {
        crate::materialx::load_in(file, (!node.is_empty()).then_some(node), host, luma)
    })
}

/// Compiles a MaterialX material through `compile`, handing it a host whose
/// texture loader resolves `image` files against `dir` and whose colour
/// conversion targets the working space, with the working space's luminance
/// weights, and reports what the compiler could not represent. `label` names the material in the log: the
/// `.mtlx` file, or the USD material of an inline network.
fn compile_mtlx(
    label: &str,
    dir: &std::path::Path,
    caches: &mut ImportCaches<'_>,
    compile: impl FnOnce(
        &crust_mtlx::Host<'_>,
        utils::Luma,
    ) -> Result<crate::materialx::Loaded, crate::materialx::MtlxError>,
) -> Option<Arc<dyn Material>> {
    // `RefCell` because the loader closure is called from inside the compiler
    // while `caches` would otherwise be mutably borrowed by the outer call.
    let (working, luma) = (caches.working, caches.luma);
    let cell = std::cell::RefCell::new(&mut *caches);
    let loader = |asset: &str, space: Option<&str>| -> Option<crate::TextureRef> {
        let mut c = cell.borrow_mut();
        load_uv_texture(
            &dir.join(asset),
            crate::ColorSpace::from_mtlx(space, working),
            &mut c,
        )
        .map(crate::TextureRef)
    };
    // A literal colour with an effective `colorspace` is converted into the
    // working space once, at compile time; one with none is already in it.
    let convert = |space: &str, rgb: [f32; 3]| -> [f32; 3] {
        crate::ColorSpace::from_mtlx(Some(space), working)
            .resolved()
            .map_or(rgb, |s| s.decode_rgb(Vec3A::from_array(rgb)).to_array())
    };
    let host = crust_mtlx::Host {
        load_texture: &loader,
        convert_color: &convert,
    };
    // Billed as (total) minus (what the texture loads already billed): the
    // loader closure runs *inside* this call and adds its own decode time to
    // the same accumulator, so timing the whole thing on top of that would
    // count every texture twice — which showed up as a "Load assets" phase
    // reported at 178% of the parse phase that contains it.
    let started = Instant::now();
    let before = cell.borrow().asset_time;
    let loaded = compile(&host, luma);
    let nested = cell.borrow().asset_time - before;
    // `cell` is not used past this point, which ends its borrow of `caches`.
    caches.asset_time += started.elapsed().saturating_sub(nested);

    match loaded {
        Ok(l) => {
            if !l.unsupported.is_empty() {
                warn!(
                    "MaterialX {label}: no operator for node type(s) {} — those inputs \
                     fall back to their defaults",
                    l.unsupported.join(", ")
                );
            }
            if !l.reported.is_empty() {
                warn!(
                    "MaterialX {label}: not represented — {}",
                    l.reported.join("; ")
                );
            }
            debug!(
                "MaterialX {label} -> {} ({} textures resolved{})",
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
            warn!("MaterialX {label} not usable ({e}) — falling back");
            None
        }
    }
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
    let path = asset_path(&prim.attribute("inputs:surfaceMap"), caches.stage_path)?;

    let space = crate::ColorSpace::new(Space::G22_REC709, caches.working);
    load_ptex(&path, space, caches).map(crate::PtexRef)
}

/// Decode a `crust:openpbr` shader into the OpenPBR material. Every input
/// name is camelCase mirror of the Rust snake_case, e.g. `base_color` →
/// `inputs:baseColor`, `subsurface_radius_scale` → `inputs:subsurfaceRadiusScale`.
fn decode_crust_openpbr(shader: &Shader, working: Space, luma: utils::Luma) -> Arc<dyn Material> {
    let mut o = OpenPBR {
        luma,
        ..OpenPBR::default()
    };

    // Inputs by their whole attribute name (`inputs:roughness`): the names are
    // literals, so none is built per read.
    let f = |n: &str, d: f32| attr_f32(&shader.attribute(n)).unwrap_or(d);
    // Every colour is authored in the working space unless its `colorSpace`
    // metadatum names another (`in_working`); `v` reads a vector that is not
    // a colour and is never converted.
    let c = |n: &str, d: Vec3A| {
        attr_vec3(&shader.attribute(n)).map_or(d, |v| in_working(&shader.attribute(n), v, working))
    };
    let v = |n: &str, d: Vec3A| attr_vec3(&shader.attribute(n)).unwrap_or(d);
    let b = |n: &str, d: bool| attr_bool(&shader.attribute(n)).unwrap_or(d);

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
    o.subsurface_radius_scale = v("inputs:subsurfaceRadiusScale", o.subsurface_radius_scale);
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
