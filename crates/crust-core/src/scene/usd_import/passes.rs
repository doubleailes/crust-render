//! The active `UsdRenderPass`: its `matte` collection's geometry is held out,
//! what its `cameraVisibility` collection leaves out is hidden from camera
//! rays, and what its `renderVisibility` collection leaves out is hidden from
//! every ray, lights included.
//!
//! The host names the pass ([`crate::UsdImportOptions::pass`]); a stage that
//! defines passes renders with none unless one is named, as Hydra leaves the
//! active pass to the application. Its `renderSource` is read by
//! [`render_settings_path`](super::settings::render_settings_path), not here.
//!
//! Membership is openusd's (`CollectionAPI::membership_query`), judged where
//! light linking judges it ([`super::light_links`]): on the stage path of the
//! prim that brings the geometry in — the instance or instancer prim, never a
//! prototype's — so the two collection features cannot disagree about what
//! "this prim" is. Unlike a light link, the pass is known before the
//! traversal starts, so it is judged at dispatch: each chunk's queries are
//! built from that chunk's stage ([`ActivePass::begin_chunk`]), which has its
//! payloads loaded, so a `membershipExpression` predicate sees the prims it
//! asks about, and opinions the pass itself takes from a payload count. Which
//! collections restrict anything is decided there too, never on the
//! payload-free index stage. The streaming import masks every chunk to the
//! pass prim too ([`ActivePass::path`]).
//!
//! Each answer is recorded, not applied: three bits per prim that emits
//! geometry and per light ([`Membership`]). [`apply_pass`] applies them after
//! the traversal to a [`MaskTable`] and a light list — the one function that
//! decides a pass's visibility — and then resolves light linking over what
//! the pass keeps. `renderVisibility` comes first: a prim outside it gets a
//! zero mask, and is neither a holdout nor counted as one. Otherwise a
//! holdout flag is set and the camera bit cleared (never set — a pass cannot
//! show the camera what `crust:rayMask` or the hidden-light rule hides). A
//! light outside `renderVisibility` leaves the light list; a light at
//! infinity held out or hidden from the camera loses camera visibility from
//! its own escape mask. All of it lands before [`super::light_links`]
//! resolves, so a light linked to nothing is demoted on the pass's camera
//! visibility, and a light linked only to hidden geometry lights nothing.
//!
//! `prune` is not read: it changes what the traversal emits
//! ([`ActivePass::read`] warns when it restricts anything). Volume regions
//! are not held out or hidden (`render_pass.volume_ignored`).

use crate::{Warning, warning};
use std::collections::HashSet;
use std::ops::Range;

use openusd::sdf;
use openusd::usd::{CollectionAPI, MembershipQuery, Prim, Stage};
use openusd_schemas::render::{COLLECTION_CAMERA_VISIBILITY, COLLECTION_MATTE, COLLECTION_PRUNE};
use tracing::debug;

use crate::light::{Light, LightList};
use crate::ray::{MASK_CAMERA, RayMask};
use crate::rt_world::MaskTable;

use super::light_links::{LightLinks, Opinions, index_remap, inside_instance, opinions};
use super::{captured, prim_at};

/// `renderVisibility`, which `openusd-schemas` names no constant for.
const COLLECTION_RENDER_VISIBILITY: &str = "renderVisibility";

/// What the active pass did, for `crust check`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassSummary {
    /// The `RenderPass` prim.
    pub path: String,
    /// How many prims that emit geometry are holdouts.
    pub holdouts: usize,
    /// How many prims that emit geometry the pass hides from the camera.
    pub camera_hidden: usize,
    /// How many prims that emit geometry the pass hides from every ray.
    pub render_hidden: usize,
}

/// One of the three collections the pass reads.
#[derive(Clone, Copy)]
enum Which {
    Matte,
    Camera,
    Render,
}

const COLLECTIONS: [Which; 3] = [Which::Matte, Which::Camera, Which::Render];

impl Which {
    fn name(self) -> &'static str {
        match self {
            Which::Matte => COLLECTION_MATTE,
            Which::Camera => COLLECTION_CAMERA_VISIBILITY,
            Which::Render => COLLECTION_RENDER_VISIBILITY,
        }
    }

    /// Whether `o` makes the collection say anything beyond its fallback:
    /// `matte` includes nothing by default (`includeRoot` 0), and
    /// `cameraVisibility` and `renderVisibility` everything (`includeRoot` 1,
    /// which `RenderPass` declares).
    fn restricts(self, o: &Opinions) -> bool {
        o.expression
            || match self {
                Which::Matte => !o.includes.is_empty() || o.include_root == Some(true),
                Which::Camera | Which::Render => {
                    !o.excludes.is_empty() || o.include_root == Some(false)
                }
            }
    }
}

/// What the pass says of one prim, with `renderVisibility`'s precedence
/// applied: a render-hidden prim is neither held out nor camera-hidden.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Membership {
    /// In `matte`.
    held: bool,
    /// Outside `cameraVisibility`.
    camera_hidden: bool,
    /// Outside `renderVisibility`.
    render_hidden: bool,
}

impl Membership {
    fn any(self) -> bool {
        self.held || self.camera_hidden || self.render_hidden
    }
}

/// The active pass, through the traversal.
pub(super) struct ActivePass {
    path: sdf::Path,
    /// The chunk being walked's queries; `None` for a collection that
    /// restricts nothing there. Decided per chunk, not on the index stage:
    /// a pass can take its collections' opinions from a payload, which only
    /// a chunk's stage loads.
    queries: [Option<MembershipQuery>; 3],
    /// Per collection, whether a target inside an instance / a nested
    /// collection a chunk does not compose was warned about already.
    warned_instance: [bool; 3],
    warned_nested: [bool; 3],
    /// The geometry ranges the pass says anything of, per emitting prim.
    geoms: Vec<(Range<u32>, Membership)>,
    /// The lights the pass says anything of, by their index when added.
    lights: Vec<(usize, Membership)>,
    /// The prims whose geometry is hidden from every ray: no receiver of a
    /// light link.
    hidden: HashSet<sdf::Path>,
    summary: PassSummary,
    /// Where its warnings go: `None` to the import's, `Some` to its own
    /// record, for a pass of a group import.
    warnings: Option<Vec<Warning>>,
}

impl ActivePass {
    /// The pass at `path` on the payload-free `index` stage, which
    /// [`super::settings::check_pass`] has found to be a `RenderPass`. Warns
    /// when `prune`, which it does not read, restricts anything. With
    /// `own_warnings`, every warning the pass raises is kept on it
    /// ([`ActivePass::take_warnings`]) rather than the import's.
    pub(super) fn read(index: &Stage, path: &sdf::Path, own_warnings: bool) -> Self {
        let mut warnings = own_warnings.then(Vec::new);
        let prim = prim_at(index, path.clone());
        let o = opinions(&prim, COLLECTION_PRUNE);
        if o.expression || !o.includes.is_empty() || o.include_root == Some(true) {
            captured(&mut warnings, || {
                warning!(
                    RenderPassCollectionNotRead,
                    at = path,
                    "{path}: collection:{COLLECTION_PRUNE} restricts what renders, which crust \
                     does not read — rendered as if it were unauthored"
                )
            });
        }
        ActivePass {
            warnings,
            path: path.clone(),
            queries: [None, None, None],
            warned_instance: [false; 3],
            warned_nested: [false; 3],
            geoms: Vec::new(),
            lights: Vec::new(),
            hidden: HashSet::new(),
            summary: PassSummary {
                path: path.to_string(),
                holdouts: 0,
                camera_hidden: 0,
                render_hidden: 0,
            },
        }
    }

    /// The pass prim, which every streamed chunk is masked to as well, so
    /// that [`ActivePass::begin_chunk`] reads its collections there.
    pub(super) fn path(&self) -> &sdf::Path {
        &self.path
    }

    /// The warnings the pass kept on itself (see [`ActivePass::read`]).
    pub(super) fn take_warnings(&mut self) -> Vec<Warning> {
        self.warnings.take().unwrap_or_default()
    }

    /// Runs `f` on the pass, its warnings going where the pass's go.
    fn capture<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let mut into = self.warnings.take();
        let out = captured(&mut into, || f(self));
        self.warnings = into;
        out
    }

    /// Builds the queries for the chunk `stage` is about to be walked from,
    /// for each collection that restricts anything on that stage.
    pub(super) fn begin_chunk(&mut self, stage: &Stage) {
        self.capture(|pass| pass.begin_chunk_inner(stage));
    }

    fn begin_chunk_inner(&mut self, stage: &Stage) {
        self.queries = [None, None, None];
        let prim = prim_at(stage, self.path.clone());
        if !prim.is_valid().unwrap_or(false) {
            debug!(
                "Render pass {} is not composed on this chunk's stage",
                self.path
            );
            return;
        }
        for (k, which) in COLLECTIONS.into_iter().enumerate() {
            if which.restricts(&opinions(&prim, which.name())) {
                self.queries[k] = self.query(stage, &prim, which, k);
            }
        }
    }

    fn query(
        &mut self,
        stage: &Stage,
        prim: &Prim,
        which: Which,
        k: usize,
    ) -> Option<MembershipQuery> {
        let name = which.name();
        let path = &self.path;
        let o = opinions(prim, name);
        if !self.warned_instance[k]
            && let Some(t) = o
                .includes
                .iter()
                .chain(&o.excludes)
                .find(|t| inside_instance(stage, t))
        {
            warning!(
                RenderPassTargetInInstance,
                at = path,
                "{path}: collection:{name} targets {t}, inside an instance — membership is \
                 judged on the instance, so the target cannot be told apart from its siblings"
            );
            self.warned_instance[k] = true;
        }
        if !self.warned_nested[k]
            && let Some(t) = o.includes.iter().find(|t| {
                CollectionAPI::instance_at_path(t)
                    .is_some_and(|(owner, _)| !prim_at(stage, owner).is_valid().unwrap_or(false))
            })
        {
            warning!(
                RenderPassNestedNotComposed,
                at = path,
                "{path}: collection:{name} includes {t}, which a chunk of the streamed import \
                 does not compose — that nested collection contributes nothing there"
            );
            self.warned_nested[k] = true;
        }
        match CollectionAPI::from_prim_unchecked(prim.clone(), name).membership_query() {
            Ok(q) => Some(q),
            Err(e) => {
                debug!("{path}: collection:{name} unreadable ({e}) — read as its fallback");
                None
            }
        }
    }

    /// What the chunk's queries say of the prim at `path`.
    fn judge(&self, path: &sdf::Path) -> Membership {
        let [matte, camera, render] = &self.queries;
        let render_hidden = render.as_ref().is_some_and(|q| !q.is_included(path));
        Membership {
            held: !render_hidden && matte.as_ref().is_some_and(|q| q.is_included(path)),
            camera_hidden: !render_hidden && camera.as_ref().is_some_and(|q| !q.is_included(path)),
            render_hidden,
        }
    }

    /// The prim at `path` emitted the geometries `geoms` and the volume
    /// regions `vols`: records what the pass says of them.
    pub(super) fn saw(&mut self, path: &sdf::Path, geoms: Range<usize>, vols: Range<usize>) {
        if self.queries.iter().all(Option::is_none) || (geoms.is_empty() && vols.is_empty()) {
            return;
        }
        let m = self.judge(path);
        if !m.any() {
            return;
        }
        if !geoms.is_empty() {
            self.geoms.push((geoms.start as u32..geoms.end as u32, m));
            self.summary.holdouts += m.held as usize;
            self.summary.camera_hidden += m.camera_hidden as usize;
            self.summary.render_hidden += m.render_hidden as usize;
            if m.render_hidden {
                self.hidden.insert(path.clone());
            }
        }
        if !vols.is_empty() {
            let pass = &self.path;
            let (why, name, sees) = if m.render_hidden {
                ("outside", "renderVisibility", "every ray sees")
            } else if m.held {
                ("in", "matte", "the camera sees")
            } else {
                ("outside", "cameraVisibility", "the camera sees")
            };
            captured(&mut self.warnings, || {
                warning!(
                    RenderPassVolumeIgnored,
                    at = path,
                    "{path}: a volume region {why} collection:{name} of render pass {pass}, \
                     which crust does not apply to volumes — {sees} it as without the pass"
                )
            });
        }
    }

    /// The light at `path` was just added at `index` of the light list:
    /// records what the pass says of it.
    pub(super) fn light_added(&mut self, path: &sdf::Path, index: usize) {
        if self.queries.iter().all(Option::is_none) {
            return;
        }
        let m = self.judge(path);
        if m.any() {
            self.lights.push((index, m));
        }
    }

    pub(super) fn summary(&self) -> PassSummary {
        self.summary.clone()
    }
}

/// Applies `pass` (or none) to the traversal's base flags `table` and its
/// light list `lights`, then resolves `links` over what the pass keeps: the
/// one function that decides a pass's visibility (design D1 of
/// `render-visibility-and-all-passes`).
///
/// Returns where each light the list held went: its index after the lights
/// the pass turned off and those that illuminate nothing left the list, or
/// `None` for one of them.
pub(super) fn apply_pass(
    pass: Option<&ActivePass>,
    links: &LightLinks,
    table: &mut MaskTable,
    lights: &mut LightList,
) -> Vec<Option<usize>> {
    let before = lights.count();
    let Some(pass) = pass else {
        return links.resolve(lights, table, &index_remap(before, &[]), &|_| false);
    };
    for (geoms, m) in &pass.geoms {
        for g in geoms.clone() {
            if m.render_hidden {
                table.set_mask(g, RayMask::NONE);
                // No longer a light source either: nothing to collect there.
                table.set_transparent_emitter(g, false);
                continue;
            }
            if m.held {
                table.set_holdout(g, true);
            }
            if m.camera_hidden {
                let mask = table.mask(g);
                table.set_mask(g, RayMask(mask.0 & !MASK_CAMERA.0));
            }
        }
    }
    // A light at infinity held out or hidden from the camera is invisible to
    // escaping camera rays: zero colour and zero alpha is what both would
    // give. A finite light's geometry was flagged above. A light outside
    // `renderVisibility` is off, so it leaves the list (before the links are
    // set, as `LightList::remove` requires).
    let mut off: Vec<usize> = Vec::new();
    for &(index, m) in &pass.lights {
        if m.render_hidden {
            debug!("Light {index}: turned off by render pass {}", pass.path);
            off.push(index);
        } else if lights.light(index).at_infinity() {
            debug!(
                "Light {index}: hidden from the camera by render pass {}",
                pass.path
            );
            lights.hide_from_camera(index);
        }
    }
    off.sort_unstable_by(|a, b| b.cmp(a));
    for &index in &off {
        lights.remove(index);
    }
    let kept = index_remap(before, &off);
    links.resolve(lights, table, &kept, &|path| pass.hidden.contains(path))
}
