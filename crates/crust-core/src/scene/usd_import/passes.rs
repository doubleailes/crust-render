//! The active `UsdRenderPass`: its `matte` collection's geometry is held out,
//! and what its `cameraVisibility` collection leaves out is hidden from
//! camera rays.
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
//! traversal starts, so it is decided at dispatch: each chunk's queries are
//! built from that chunk's stage ([`ActivePass::begin_chunk`]), which has its
//! payloads loaded, so a `membershipExpression` predicate sees the prims it
//! asks about. The streaming import masks every chunk to the pass prim too
//! ([`ActivePass::path`]).
//!
//! Each answer is written straight into the world: a holdout flag
//! ([`WorldBuilder::set_holdout`]), and the camera bit cleared from the mask
//! (never set — a pass cannot show the camera what `crust:rayMask` or the
//! hidden-light rule hides). A light at infinity loses camera visibility from
//! its own escape mask. All of it lands before [`super::light_links`]
//! resolves, so a light linked to nothing is demoted on the pass's camera
//! visibility.
//!
//! `prune` and `renderVisibility` are not read: they change what the
//! traversal emits ([`ActivePass::read`] warns when they restrict anything).

use crate::warning;
use std::ops::Range;

use openusd::sdf;
use openusd::usd::{CollectionAPI, MembershipQuery, Prim, Stage};
use openusd_schemas::render::{COLLECTION_CAMERA_VISIBILITY, COLLECTION_MATTE, COLLECTION_PRUNE};
use tracing::debug;

use crate::light::{Light, LightList};
use crate::ray::{MASK_CAMERA, RayMask};
use crate::rt_world::WorldBuilder;

use super::light_links::{Opinions, inside_instance, opinions};
use super::prim_at;

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
}

/// One of the two collections the pass reads.
#[derive(Clone, Copy)]
enum Which {
    Matte,
    Camera,
}

impl Which {
    fn name(self) -> &'static str {
        match self {
            Which::Matte => COLLECTION_MATTE,
            Which::Camera => COLLECTION_CAMERA_VISIBILITY,
        }
    }

    /// Whether `o` makes the collection say anything beyond its fallback:
    /// `matte` includes nothing by default (`includeRoot` 0), and
    /// `cameraVisibility` everything (`includeRoot` 1, which `RenderPass`
    /// declares).
    fn restricts(self, o: &Opinions) -> bool {
        o.expression
            || match self {
                Which::Matte => !o.includes.is_empty() || o.include_root == Some(true),
                Which::Camera => !o.excludes.is_empty() || o.include_root == Some(false),
            }
    }
}

/// The active pass, through the traversal.
pub(super) struct ActivePass {
    path: sdf::Path,
    /// Whether `matte` / `cameraVisibility` restrict anything, as the index
    /// stage reads the pass: a collection that does not is never queried.
    reads: [bool; 2],
    /// The chunk being walked's queries; `None` for a collection that
    /// restricts nothing.
    queries: [Option<MembershipQuery>; 2],
    /// Per collection, whether a target inside an instance / a nested
    /// collection a chunk does not compose was warned about already.
    warned_instance: [bool; 2],
    warned_nested: [bool; 2],
    holdouts: usize,
    camera_hidden: usize,
}

impl ActivePass {
    /// The pass at `path` on the payload-free `index` stage, which
    /// [`super::settings::check_pass`] has found to be a `RenderPass`. Warns
    /// once per collection it does not read that restricts anything.
    pub(super) fn read(index: &Stage, path: &sdf::Path) -> Self {
        let prim = prim_at(index, path.clone());
        for (name, fallback_root) in [
            (COLLECTION_PRUNE, false),
            (COLLECTION_RENDER_VISIBILITY, true),
        ] {
            let o = opinions(&prim, name);
            let restricts = o.expression
                || if fallback_root {
                    !o.excludes.is_empty() || o.include_root == Some(false)
                } else {
                    !o.includes.is_empty() || o.include_root == Some(true)
                };
            if restricts {
                warning!(
                    RenderPassCollectionNotRead,
                    at = path,
                    "{path}: collection:{name} restricts what renders, which crust does not \
                     read — rendered as if it were unauthored"
                );
            }
        }
        let reads = [Which::Matte, Which::Camera].map(|w| w.restricts(&opinions(&prim, w.name())));
        debug!(
            "Render pass {path}: matte {}, cameraVisibility {}",
            if reads[0] {
                "read"
            } else {
                "holds out nothing"
            },
            if reads[1] { "read" } else { "hides nothing" }
        );
        ActivePass {
            path: path.clone(),
            reads,
            queries: [None, None],
            warned_instance: [false; 2],
            warned_nested: [false; 2],
            holdouts: 0,
            camera_hidden: 0,
        }
    }

    /// The pass prim, which every streamed chunk is masked to as well, so
    /// that [`ActivePass::begin_chunk`] reads its collections there.
    pub(super) fn path(&self) -> &sdf::Path {
        &self.path
    }

    /// Whether the pass changes anything about the geometry or the lights.
    fn active(&self) -> bool {
        self.reads.iter().any(|&r| r)
    }

    /// Builds the queries for the chunk `stage` is about to be walked from.
    pub(super) fn begin_chunk(&mut self, stage: &Stage) {
        self.queries = [None, None];
        if !self.active() {
            return;
        }
        let prim = prim_at(stage, self.path.clone());
        if !prim.is_valid().unwrap_or(false) {
            debug!(
                "Render pass {} is not composed on this chunk's stage",
                self.path
            );
            return;
        }
        for (k, which) in [Which::Matte, Which::Camera].into_iter().enumerate() {
            if self.reads[k] {
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

    /// Whether the prim at `path` is held out, and whether it is hidden from
    /// the camera, by the chunk's queries.
    fn judge(&self, path: &sdf::Path) -> (bool, bool) {
        let [matte, camera] = &self.queries;
        (
            matte.as_ref().is_some_and(|q| q.is_included(path)),
            camera.as_ref().is_some_and(|q| !q.is_included(path)),
        )
    }

    /// The prim at `path` emitted the geometries `geoms` and the volume
    /// regions `vols`: flags them as the pass says.
    pub(super) fn saw(
        &mut self,
        path: &sdf::Path,
        geoms: Range<usize>,
        vols: Range<usize>,
        world: &mut WorldBuilder,
    ) {
        if self.queries.iter().all(Option::is_none) || (geoms.is_empty() && vols.is_empty()) {
            return;
        }
        let (held, hidden) = self.judge(path);
        if !geoms.is_empty() {
            for g in geoms.start as u32..geoms.end as u32 {
                if held {
                    world.set_holdout(g, true);
                }
                if hidden {
                    let mask = world.mask(g);
                    world.set_mask(g, RayMask(mask.0 & !MASK_CAMERA.0));
                }
            }
            self.holdouts += held as usize;
            self.camera_hidden += hidden as usize;
        }
        if !vols.is_empty() && (held || hidden) {
            let (pass, why) = (&self.path, if held { "in" } else { "outside" });
            let name = if held { "matte" } else { "cameraVisibility" };
            warning!(
                RenderPassVolumeIgnored,
                at = path,
                "{path}: a volume region {why} collection:{name} of render pass {pass}, which \
                 crust does not apply to volumes — the camera sees it as without the pass"
            );
        }
    }

    /// The light at `path` was just added at `index` of `lights`. A light at
    /// infinity held out or hidden from the camera is invisible to escaping
    /// camera rays: zero colour and zero alpha is what both would give. A
    /// finite light's geometry was flagged by [`ActivePass::saw`].
    pub(super) fn light_added(&self, path: &sdf::Path, index: usize, lights: &mut LightList) {
        if self.queries.iter().all(Option::is_none) || !lights.light(index).at_infinity() {
            return;
        }
        let (held, hidden) = self.judge(path);
        if held || hidden {
            debug!(
                "{path}: hidden from the camera by render pass {}",
                self.path
            );
            lights.hide_from_camera(index);
        }
    }

    pub(super) fn summary(&self) -> PassSummary {
        PassSummary {
            path: self.path.to_string(),
            holdouts: self.holdouts,
            camera_hidden: self.camera_hidden,
        }
    }
}
