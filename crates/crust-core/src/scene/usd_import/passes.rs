//! `RenderPass` prims: which settings a pass renders with, and its layers
//! ([`crate::render_pass`]).
//!
//! A layer is a multiple-apply collection on the pass, `collection:layer:<name>`.
//! Its membership is openusd's own (`CollectionAPI::compute_membership_query`),
//! read once off the index stage, and judged, like a light link's, against the
//! stage path of every prim that emitted geometry or a volume
//! ([`LightLinks::per_receiver`]). The answer is one `u16` per `geom_id` in the
//! world and one per volume region: what a camera sample's first hit is owned
//! by. A native instance emits ids of its own, so two instances of one
//! prototype can be in two layers; a point instancer's instances share its
//! one entry, and a prim inside a prototype has no entry of its own.

use crate::warning;

use openusd::sdf;
use openusd::usd::{CollectionAPI, MembershipQuery, Prim, Stage};
use openusd_schemas::geom::PointInstancer;
use openusd_schemas::render::{Pass, PassSchema, Settings as UsdRenderSettings};
use tracing::debug;

use crate::render_pass::{DEFAULT_FILE_PATTERN, DEFAULT_LAYER, DEFAULT_LAYER_PATTERN, RenderPass};
use crate::rt_world::WorldBuilder;
use crate::volume::VolumeRegion;

use super::attrs::{decode_text, prim_value};
use super::light_links::{LightLinks, inside_instance, opinions};
use super::prim_at;
use super::settings::render_settings_path;

/// How a layer collection appears in a pass's composed `apiSchemas`.
const LAYER_SCHEMA: &str = "CollectionAPI:layer:";

/// The `RenderSettings` prim the pass at `pass` renders with: its
/// `renderSource`'s first target, else the stage's own settings prim. A
/// `pass` that is not a `RenderPass`, or a source that is not a
/// `RenderSettings`, is refused: either would render settings the pass
/// does not name.
pub(super) fn pass_settings(
    index: &Stage,
    pass: &sdf::Path,
) -> Result<Option<sdf::Path>, crate::Error> {
    let Some(prim) = Pass::get(index, pass.clone()).ok().flatten() else {
        return Err(crate::Error::NotARenderPass(pass.to_string()));
    };
    let source = prim
        .render_source_rel()
        .targets()
        .ok()
        .and_then(|t| t.into_iter().next());
    match source {
        None => Ok(render_settings_path(index)),
        Some(source) => match UsdRenderSettings::get(index, source.clone()).ok().flatten() {
            Some(_) => Ok(Some(source)),
            None => Err(crate::Error::PassSourceNotRenderSettings {
                pass: pass.to_string(),
                source: source.to_string(),
            }),
        },
    }
}

/// The layer names `prim` declares, in its composed `apiSchemas` order,
/// without [`DEFAULT_LAYER`]: a collection of that name is the implicit
/// layer's, not a layer of its own.
pub(super) fn declared_layers(prim: &Prim) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for schema in prim.api_schemas().unwrap_or_default() {
        if let Some(name) = schema.as_str().strip_prefix(LAYER_SCHEMA)
            && !name.is_empty()
            && name != DEFAULT_LAYER
            && !names.iter().any(|n| n == name)
        {
            names.push(name.to_owned());
        }
    }
    names
}

/// The pass at `pass`, already checked by [`pass_settings`], as the render
/// reads it. `settings` is what `pass_settings` returned.
pub(super) fn read_pass(
    index: &Stage,
    pass: &sdf::Path,
    settings: Option<&sdf::Path>,
) -> RenderPass {
    let prim = prim_at(index, pass.clone());
    let text = |name: &str| prim_value(&prim, name).and_then(decode_text);
    let mut layers = declared_layers(&prim);
    layers.push(DEFAULT_LAYER.to_owned());
    RenderPass {
        path: pass.to_string(),
        name: pass.name().unwrap_or_default().to_owned(),
        render_source: settings.map(ToString::to_string),
        layers,
        file_pattern: text("crust:filePattern").unwrap_or_else(|| DEFAULT_FILE_PATTERN.to_owned()),
        layer_pattern: text("crust:layerPattern")
            .unwrap_or_else(|| DEFAULT_LAYER_PATTERN.to_owned()),
    }
}

/// What one layer collection claims.
enum Claim {
    /// Nothing: no includes and no `includeRoot` (the collection fallback),
    /// or a collection that cannot be read.
    Nothing,
    /// Every prim: `includeRoot = 1` and nothing excluded.
    Everything,
    Some(MembershipQuery),
}

impl Claim {
    fn includes(&self, path: &sdf::Path) -> bool {
        match self {
            Claim::Nothing => false,
            Claim::Everything => true,
            Claim::Some(q) => q.is_path_included(path),
        }
    }
}

/// Reads the layer collection `layer:<name>` of the pass `prim` as what it
/// claims. `UsdCollectionAPI`'s own fallback applies: unlike a light link,
/// a layer that authors nothing claims nothing.
fn layer_claim(stage: &Stage, prim: &Prim, name: &str) -> Claim {
    let instance = format!("layer:{name}");
    let o = opinions(prim, &instance);
    if o.expression {
        warning!(
            PassLayerMembershipExpression,
            at = prim.path(),
            "{}: collection:{instance} authors membershipExpression, which crust does not \
             read — layer {name} claims nothing",
            prim.path()
        );
        return Claim::Nothing;
    }
    if o.includes.is_empty() && o.excludes.is_empty() {
        return if o.include_root == Some(true) {
            Claim::Everything
        } else {
            Claim::Nothing
        };
    }
    let query = match CollectionAPI::from_prim_unchecked(prim.clone(), &instance)
        .compute_membership_query()
    {
        Ok(q) => q,
        Err(e) => {
            warning!(
                PassLayerUnreadableCollection,
                at = prim.path(),
                "{}: collection:{instance} unreadable ({e}) — layer {name} claims nothing",
                prim.path()
            );
            return Claim::Nothing;
        }
    };
    for t in o.includes.iter().chain(&o.excludes) {
        if inside_instance(stage, t) || under_instancer(stage, t) {
            warning!(
                PassLayerTargetInInstance,
                at = prim.path(),
                "{}: collection:{instance} targets {t}, inside an instance or a point \
                 instancer — membership is judged on the instance or the instancer, so \
                 the target cannot be told apart from its siblings",
                prim.path()
            );
            break;
        }
    }
    Claim::Some(query)
}

/// Whether some strict ancestor of `path` is a `PointInstancer`: `path` is
/// one of its prototypes, drawn only through it.
fn under_instancer(stage: &Stage, path: &sdf::Path) -> bool {
    let mut p = path.parent();
    while let Some(a) = p {
        if a.is_abs_root() {
            return false;
        }
        if matches!(PointInstancer::get(stage, a.clone()), Ok(Some(_))) {
            return true;
        }
        p = a.parent();
    }
    false
}

/// Installs the render layers of `pass` into the world and the volume
/// regions: one layer index per `geom_id` and per region, judged against
/// every prim `links` saw. A prim two layers claim belongs to the earlier,
/// with a warning; a prim none claims belongs to [`DEFAULT_LAYER`], the
/// last. Nothing is installed when the pass declares no layer of its own:
/// every sample is then layer 0, `default`. Must run before the light links
/// are resolved, which consumes `links`.
pub(super) fn install_layers(
    index: &Stage,
    pass: &RenderPass,
    links: &LightLinks,
    world: &mut WorldBuilder,
    volumes: &mut [VolumeRegion],
) {
    let declared = &pass.layers[..pass.layers.len() - 1];
    if declared.is_empty() {
        return;
    }
    let path = sdf::path(&pass.path).expect("a checked render pass path");
    let prim = prim_at(index, path);
    let claims: Vec<Claim> = declared
        .iter()
        .map(|name| layer_claim(index, &prim, name))
        .collect();
    let default = declared.len() as u16;
    let mut counts = vec![0usize; pass.layers.len()];
    let (geoms, regions) = links.per_receiver(world.count(), volumes.len(), default, |p| {
        let mut claimed = claims.iter().enumerate().filter(|(_, c)| c.includes(p));
        let Some((first, _)) = claimed.next() else {
            counts[default as usize] += 1;
            return default;
        };
        if let Some((second, _)) = claimed.next() {
            warning!(
                PassLayerOverlap,
                at = p,
                "{p} is claimed by layers {} and {} of {}; it belongs to {}",
                declared[first],
                declared[second],
                pass.path,
                declared[first]
            );
        }
        counts[first] += 1;
        first as u16
    });
    for (name, n) in pass.layers.iter().zip(&counts) {
        debug!("Render pass {}: layer {name} holds {n} prim(s)", pass.path);
    }
    world.set_layers(geoms, default);
    for (region, layer) in volumes.iter_mut().zip(regions) {
        region.layer = layer;
    }
}

/// Warns about each of the pass's `renderVisibility` and `cameraVisibility`
/// collections that restricts membership: crust renders every object in
/// every pass, so either would change the image if it were read.
pub(super) fn warn_unread_collections(index: &Stage, pass: &RenderPass) {
    let path = sdf::path(&pass.path).expect("a checked render pass path");
    let prim = prim_at(index, path);
    for name in ["renderVisibility", "cameraVisibility"] {
        let o = opinions(&prim, name);
        if !o.includes.is_empty()
            || !o.excludes.is_empty()
            || o.include_root == Some(false)
            || o.expression
        {
            warning!(
                PassCollectionNotRead,
                at = prim.path(),
                "{}: collection:{name} restricts what the pass renders, which crust does \
                 not read — every object is rendered",
                pass.path
            );
        }
    }
}
