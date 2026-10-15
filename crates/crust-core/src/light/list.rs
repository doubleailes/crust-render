//! [`LightList`] and NEE's light selection ([`LightSelection`]): uniform,
//! defensive power, the light tree, the learned per-cell table, or the
//! neural visibility cache.

use std::collections::HashMap;

use glam::Vec3A;

use super::{Light, LightKind, LightTree, Receiver};
use crate::error::Structural;
// Defined beside the world's per-geometry classes, which default to it.
use crate::pdf::PdfSolidAngle;
use crate::ray::{MASK_ALL, MASK_CAMERA, MASK_SHADOW, RayMask};
pub use crate::rt_world::EVERY_CLASS;

/// How NEE chooses which light to sample at a vertex (`crust:lightSelection`,
/// `--light-selection`). Measured in `docs/light_sampling.md` §3.8.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LightSelection {
    /// One in N, whatever the lights emit: the renderer's behaviour before
    /// selection was a choice, reproduced bit for bit (see
    /// [`LightList::density`]), and the A/B for [`LightSelection::Power`].
    Uniform,
    /// The default: by power (Shirley et al. 1996; pbrt-v4's
    /// `PowerLightSampler`), made defensive. Lights at infinity keep their
    /// uniform share, since they have no comparable power ([`Light::power`]).
    /// The finite lights split the rest [`DEFENSIVE_SHARE`] evenly and the
    /// remainder in proportion to power, so no light falls below half its
    /// uniform share — power is blind to distance and visibility, and the
    /// even half is what bounds the cost where that blindness is wrong. It
    /// pays off when a few lights outshine many that light the same things
    /// (4.6× lower relMSE on a key among seven dim fills) and costs a few
    /// percent where each light owns its own region (6% on `veach_mis`).
    #[default]
    Power,
    /// Visibility-aware: per-region pick distributions learned by a short
    /// pre-pass before the render (see `light_cache.rs`), over the power
    /// selection wherever nothing was learned. What an interior lit through
    /// windows needs, where the most powerful lights are the ones hidden.
    Learned,
    /// Visibility-aware, continuously: the power table weighted by each
    /// light's visibility from the vertex, as a small network trained by the
    /// same pre-pass predicts it (see `light_visibility.rs`). An opt-in
    /// experiment against [`LightSelection::Learned`]'s grid
    /// (`docs/light_sampling.md`, "Neural visibility cache").
    Neural,
    /// By a light BVH (Conty Estevez & Kulla 2018; pbrt-v4's
    /// `BVHLightSampler`): the finite lights are walked from the shading
    /// point, each node's children chosen by an importance bound built from
    /// flux, distance, the lights' emission cones and the receiver's normal
    /// (see `tree.rs`). Mixed with [`TREE_DEFENSIVE_SHARE`] of the power
    /// table, so no light falls below that share of its power probability;
    /// lights at infinity keep their uniform share. Blind to visibility,
    /// which [`LightSelection::Learned`] sees.
    Tree,
}

crate::names::named!(
    LightSelection,
    "light selection",
    [
        (
            LightSelection::Power,
            "power",
            "By power, defensively: lights at infinity keep their uniform share (default)"
        ),
        (
            LightSelection::Uniform,
            "uniform",
            "One light in N, whatever it emits (the renderer before selection, bit for bit)"
        ),
        (
            LightSelection::Learned,
            "learned",
            "Visibility-aware: per-region pick distributions learned by a short pre-pass"
        ),
        (
            LightSelection::Tree,
            "tree",
            "By a light BVH: flux, distance, orientation and the receiver's horizon at each shading point, over a defensive share of power"
        ),
        (
            LightSelection::Neural,
            "neural",
            "Visibility-aware: power weighted by each light's visibility, as a small network trained by the same pre-pass predicts it (experimental)"
        ),
    ]
);

/// A position-dependent selection over the power table, consulted by every
/// `*_at` method of [`LightList`].
#[derive(Debug)]
pub(crate) enum Learned {
    /// [`LightSelection::Learned`]'s per-cell tables.
    Grid(super::learned::LightCache),
    /// [`LightSelection::Neural`]'s network.
    Neural(super::neural::VisibilityCache),
    /// [`LightSelection::Tree`]'s light BVH.
    Tree(TreeSelection),
}

/// [`LightSelection::Tree`]: the light BVH over the finite lights, and the
/// shares the non-defensive part of a pick splits into. That part (`q`)
/// gives each light at infinity `1 / live` and the tree's lights
/// `finite_share` times the tree's probability, where `live` counts the
/// lights at infinity and the tree's lights; the pick is
/// `TREE_DEFENSIVE_SHARE · power + (1 − TREE_DEFENSIVE_SHARE) · q`.
#[derive(Debug)]
pub(crate) struct TreeSelection {
    tree: LightTree,
    /// The number of lights at infinity plus the number in the tree.
    live: u32,
    /// The tree's lights' share of `q`: their count over `live`.
    finite_share: f32,
}

/// Under [`LightSelection::Tree`], the share of every pick made by the power
/// table rather than the tree: what keeps a light the tree's bound rates low
/// (a bump-mapped normal, a receiver inside a node's box) at no less than
/// this share of its power probability, so its NEE weight is at most
/// `1 / TREE_DEFENSIVE_SHARE` times what `power` gives it. Chosen by
/// measurement (`docs/light_sampling.md` §3.15).
pub const TREE_DEFENSIVE_SHARE: f32 = 0.1;

/// The largest `f32` below one, where a rescaled pick sample is clamped.
const ONE_MINUS_EPSILON: f32 = 1.0 - f32::EPSILON / 2.0;

/// Under [`LightSelection::Power`], the share of the finite lights' shadow
/// rays split evenly among them rather than by power (Hesterberg's
/// defensive importance sampling).
pub const DEFENSIVE_SHARE: f64 = 0.5;

/// Per-light light and shadow linking, indexed like [`LightList::lights`].
/// Built by the importer once every receiver is known
/// ([`LightList::set_links`]); absent when no light authors a link, which is
/// what keeps an unlinked scene's hot path to one `None` test.
#[derive(Clone, Debug, Default)]
pub struct LightLinks {
    /// Per light, the receiver classes it illuminates as a bitset
    /// (`class / 64`, `1 << class % 64`); `None` illuminates every class.
    pub illuminates: Vec<Option<Box<[u64]>>>,
    /// Per light, the mask its shadow rays carry: which occluder classes
    /// block it (see `shadow_link` encoding in the importer).
    pub shadow_masks: Vec<RayMask>,
    /// Per light, whether its shadow set is restricted: its shadow mask
    /// leaves some occluder class out. A bounce ray is stopped by occluders
    /// such a light's shadow rays ignore, so at a continuous vertex the
    /// bounce side estimates it through its *link twin* instead — a shadow
    /// ray along the bounce direction (`tracer/path.rs`) — or not at all.
    pub restricted: Vec<bool>,
    /// Per light, whether it is sampled by NEE alone at continuous vertices:
    /// a restricted dome, whose twin would cost a shadow ray on every bounce
    /// (lighting design record, "Shadow linking"), or every restricted light
    /// under `CRUST_LINK_TWIN=0`. Implies `restricted`. A restricted dome is
    /// NEE-only whatever this says: [`LightList::set_links`] sets it, since a
    /// dome has no link twin and its bounce side would otherwise be lost.
    pub nee_only: Vec<bool>,
}

/// The scene's lights, and how NEE picks one of them.
///
/// The pick's probability is half of the light strategy's MIS density (the
/// other half is the light's own `sample_li` pdf), so whatever
/// [`LightList::pick`] reports, [`LightList::find_index_by_geom_at`] and
/// [`LightList::iter`] report the same number for the same light: the bounce
/// side weights emission it found by chance with it, and the two sides must
/// describe one strategy or emission is double-counted.
pub struct LightList {
    /// The lights. Private, so that [`LightList::add`] is the only way in:
    /// it keeps the geometry index and the selection in step with this
    /// vector, and a light pushed past it would be sampled by NEE yet
    /// unattributed on the bounce side. Read it through
    /// [`LightList::lights`].
    pub(super) lights: Vec<LightKind>,
    /// Per-light selection probability, empty while the selection is
    /// uniform — [`LightList::select_by`] fills it.
    pub(super) pmf: Vec<f32>,
    /// Inclusive running sum of `pmf`, ending at exactly 1.
    pub(super) cdf: Vec<f32>,
    /// `geom_id → index into lights`, so a bounce hit finds its light in O(1)
    /// rather than by scanning the list on every emissive hit.
    pub(super) by_geom: HashMap<u32, usize>,
    /// Indices into `lights` of the lights at infinity ([`Light::at_infinity`]),
    /// in list order: the only ones an escaping ray can find, so
    /// [`LightList::infinite_at`] visits these rather than every light.
    pub(super) infinite: Vec<u32>,
    /// Per entry of `infinite`, the categories of *escaping* ray that see
    /// that light: [`MASK_ALL`] unless it is hidden from the camera. A light
    /// hidden from a ray is skipped only on the escape it would have been
    /// found by; its selection, NEE and every MIS weight are untouched,
    /// which is sound because camera rays have no NEE competitor.
    pub(super) infinite_masks: Vec<RayMask>,
    /// Lights at infinity that illuminate nothing but the camera sees — a
    /// backdrop. Outside `lights` on purpose: nothing can select them, so
    /// no pmf, [`LightList::density`], light cache or guide can mention
    /// them. Only an escaping camera ray reads them
    /// ([`LightList::backdrops`]).
    pub(super) backdrops: Vec<LightKind>,
    /// The learned selection, under [`LightSelection::Learned`] or
    /// [`LightSelection::Neural`]. Consulted by every `*_at` method; `pmf` /
    /// `cdf` are what it falls back to wherever it has nothing to say.
    pub(super) cache: Option<std::sync::Arc<Learned>>,
    /// Light and shadow linking, when any light authors a link.
    pub(super) links: Option<Box<LightLinks>>,
    /// The restricted lights that are not NEE-only, in list order: the ones
    /// a continuous vertex estimates through a link twin
    /// ([`LightList::twinned_lights`]). Empty without shadow links.
    pub(super) twinned: Vec<u32>,
    /// Per light, its light-path-expression tag (`crust:light:lpeTag`): the
    /// label its `L` events carry, so `<L.'key'>` selects it. Kept in step
    /// with `lights` by `add_masked` and `remove`.
    pub(super) lpe_tags: Vec<Option<Box<str>>>,
    /// Per light, the name it was imported under (its USD prim path), for
    /// messages and reports; kept in step with `lights` like `lpe_tags`.
    pub(super) names: Vec<Option<Box<str>>>,
    /// The working colour space's luminance weights ([`LightList::luma`]).
    pub(super) luma: utils::Luma,
}

impl Default for LightList {
    /// Creates a new, empty `LightList` as the default implementation.
    fn default() -> Self {
        Self::new()
    }
}

impl LightList {
    /// Creates a new, empty `LightList`, selecting uniformly until
    /// [`LightList::select_by`] says otherwise.
    pub fn new() -> Self {
        Self {
            lights: Vec::new(),
            pmf: Vec::new(),
            cdf: Vec::new(),
            by_geom: HashMap::new(),
            infinite: Vec::new(),
            infinite_masks: Vec::new(),
            backdrops: Vec::new(),
            cache: None,
            links: None,
            twinned: Vec::new(),
            lpe_tags: Vec::new(),
            names: Vec::new(),
            luma: utils::Luma::REC709,
        }
    }

    /// The luminance weights of the working colour space the scene's colours
    /// are in ([`crate::color::luma`]): what every heuristic that weighs a
    /// colour by one number uses — light power here, and from here the
    /// learned light cache, guiding's training signal and the renderer's
    /// adaptive-sampling and `variance` statistics. Rec.709's until
    /// [`LightList::set_luma`].
    pub fn luma(&self) -> utils::Luma {
        self.luma
    }

    /// Sets [`LightList::luma`]. The selection falls back to uniform until
    /// the next [`LightList::select_by`], as after [`LightList::add`], since a
    /// power table weighed by the old weights would describe the wrong one.
    pub fn set_luma(&mut self, luma: utils::Luma) {
        self.luma = luma;
        self.pmf.clear();
        self.cdf.clear();
        self.cache = None;
    }

    /// Adds a light source. The selection falls back to uniform until the
    /// next [`LightList::select_by`], since a table built over the old list
    /// would describe the wrong one.
    pub fn add(&mut self, light: impl Into<LightKind>) {
        self.add_masked(light, MASK_ALL);
    }

    /// [`LightList::add`] for a light at infinity seen only by the escaping
    /// rays in `escape_mask` — `MASK_ALL` without [`MASK_CAMERA`] hides it
    /// from the camera. A finite light's camera visibility lives on its
    /// geometry instead, so the mask is ignored for it.
    pub fn add_masked(&mut self, light: impl Into<LightKind>, escape_mask: RayMask) {
        debug_assert!(self.links.is_none(), "links are set after the last light");
        let light = light.into();
        if let Some(id) = light.geom_id() {
            self.by_geom.insert(id, self.lights.len());
        }
        if light.at_infinity() {
            self.infinite.push(self.lights.len() as u32);
            self.infinite_masks.push(escape_mask);
        }
        self.lights.push(light);
        self.lpe_tags.push(None);
        self.names.push(None);
        self.pmf.clear();
        self.cdf.clear();
        self.cache = None;
    }

    /// Refuses replacing the light at `index` with `light` when the list was
    /// built on something it says differently: another kind of light, one
    /// at or away from infinity, or another geometry ([`crate::live`]).
    /// Resolved links stay as they are: they are read from the lights'
    /// collections alone, which an in-place edit never changes, and keyed by
    /// index and `geom_id`, which a replacement keeps.
    pub fn check_replace(&self, index: usize, light: &LightKind) -> Result<(), Structural> {
        let Some(old) = self.lights.get(index) else {
            return Err(Structural(format!("the light list has no light {index}")));
        };
        if !same_light_shape(old, light) {
            return Err(Structural(format!(
                "light {index} would change kind, infinity or geometry"
            )));
        }
        Ok(())
    }

    /// Replaces the light at `index` with `light`, after
    /// [`LightList::check_replace`] passed. Its name, LPE tag, escape mask
    /// and geometry index stay. The selection falls back to uniform, as after
    /// [`LightList::add`]: the light's power is part of it, so it must be
    /// rebuilt ([`LightList::select_by`], the renderer's `reconfigure`).
    pub fn replace(&mut self, index: usize, light: LightKind) {
        debug_assert!(self.check_replace(index, &light).is_ok());
        self.lights[index] = light;
        self.pmf.clear();
        self.cdf.clear();
        self.cache = None;
    }

    /// Removes the light at `index` (in [`LightList::lights`] order) and
    /// returns it with the escape mask it was added with ([`MASK_ALL`] for a
    /// finite light). Every later light shifts down one place, and its
    /// geometry and infinite-light entries with it. The selection falls back
    /// to uniform, as after [`LightList::add`].
    ///
    /// For the importer, which can only tell a light that illuminates nothing
    /// once every receiver has been read.
    ///
    /// # Panics
    /// If `index` is out of range.
    pub fn remove(&mut self, index: usize) -> (LightKind, RayMask) {
        debug_assert!(self.links.is_none(), "links are set after the last removal");
        let light = self.lights.remove(index);
        self.lpe_tags.remove(index);
        self.names.remove(index);
        self.by_geom.retain(|_, i| *i != index);
        for i in self.by_geom.values_mut() {
            if *i > index {
                *i -= 1;
            }
        }
        let mut mask = MASK_ALL;
        if let Some(at) = self.infinite.iter().position(|&i| i as usize == index) {
            self.infinite.remove(at);
            mask = self.infinite_masks.remove(at);
        }
        for i in &mut self.infinite {
            if *i as usize > index {
                *i -= 1;
            }
        }
        self.pmf.clear();
        self.cdf.clear();
        self.cache = None;
        (light, mask)
    }

    /// Sets light `index`'s light-path-expression tag — see
    /// [`LightList::lpe_tag`]. An empty tag is no tag.
    pub fn set_lpe_tag(&mut self, index: usize, tag: Option<&str>) {
        self.lpe_tags[index] = tag.filter(|t| !t.is_empty()).map(Into::into);
    }

    /// Light `index`'s light-path-expression tag: the custom label of the
    /// `L` events it ends, which a light group `<L.'tag'>` selects.
    pub fn lpe_tag(&self, index: usize) -> Option<&str> {
        self.lpe_tags.get(index).and_then(|t| t.as_deref())
    }

    /// Names light `index` — the importer gives each light its prim path.
    pub fn set_name(&mut self, index: usize, name: &str) {
        self.names[index] = Some(name.into());
    }

    /// Light `index`'s name ([`LightList::set_name`]), if it was given one.
    pub fn name(&self, index: usize) -> Option<&str> {
        self.names.get(index).and_then(|n| n.as_deref())
    }

    /// The light-list entry whose geometry is `geom_id`, if any — whether a
    /// hit on emissive geometry is a light (`L`) or not (`O`).
    pub fn index_of_geom(&self, geom_id: u32) -> Option<usize> {
        self.by_geom.get(&geom_id).copied()
    }

    /// Builds the selection over the current lights (see [`LightSelection`]).
    ///
    /// A finite light whose power comes out non-finite or non-positive — a
    /// black one — gets probability zero, so NEE never spends a ray on it,
    /// and the bounce side, seeing the same zero, keeps its emission at full
    /// weight, so nothing is lost. With nothing left to pick from, the
    /// selection stays uniform.
    ///
    /// The table is inverted by its CDF rather than an alias table, and that
    /// is deliberate: the map from `u` to light stays monotone, so the
    /// stratified samples that pick light *k* are still one contiguous slice
    /// of the pick dimension, as under uniform picking.
    ///
    /// [`LightSelection::Learned`] and [`LightSelection::Neural`] build the
    /// power table here; the learned part needs the scene and is added by the
    /// renderer ([`LightList::set_cache`], [`LightList::set_visibility`]).
    pub fn select_by(&mut self, selection: LightSelection) {
        self.pmf.clear();
        self.cdf.clear();
        self.cache = None;
        if selection == LightSelection::Uniform || self.lights.is_empty() {
            return;
        }
        // `None` at infinity; `Some(0)` for a finite light that emits nothing.
        let powers: Vec<Option<f64>> = self
            .lights
            .iter()
            .map(|l| {
                l.power(self.luma).map(|p| {
                    let p = p as f64;
                    if p.is_finite() && p > 0.0 { p } else { 0.0 }
                })
            })
            .collect();
        let infinite = powers.iter().filter(|p| p.is_none()).count();
        let lit: Vec<f64> = powers
            .iter()
            .flatten()
            .copied()
            .filter(|&p| p > 0.0)
            .collect();
        let live = infinite + lit.len();
        if live == 0 {
            return;
        }
        let finite_share = lit.len() as f64 / live as f64;
        let lit_total: f64 = lit.iter().sum();
        let weights: Vec<f64> = powers
            .iter()
            .map(|p| match *p {
                None => 1.0 / live as f64,
                Some(p) if p > 0.0 => {
                    finite_share
                        * (DEFENSIVE_SHARE / lit.len() as f64
                            + (1.0 - DEFENSIVE_SHARE) * p / lit_total)
                }
                Some(_) => 0.0,
            })
            .collect();
        let total: f64 = weights.iter().sum();
        let mut running = 0.0f64;
        for (index, w) in weights.into_iter().enumerate() {
            running += w;
            self.pmf.push((w / total) as f32);
            self.cdf.push((running / total) as f32);
            tracing::debug!(
                "light {index} (geom {:?}): power {:?}, picked with probability {:.4}",
                self.lights[index].geom_id(),
                powers[index],
                w / total
            );
        }
        // The last light with any power ends the CDF at exactly one, so no
        // `u` below one can fall past it.
        if let Some(last) = self.pmf.iter().rposition(|&p| p > 0.0) {
            for c in &mut self.cdf[last..] {
                *c = 1.0;
            }
        }
        if selection == LightSelection::Tree {
            self.build_tree(&powers);
        }
    }

    /// Installs [`LightSelection::Tree`]'s light BVH over the power table
    /// [`LightList::select_by`] just built, over the lights that table gives
    /// a share (finite, with power). With at most one there is nothing to
    /// choose, and the power table stays, picking exactly as
    /// [`LightSelection::Power`] does.
    fn build_tree(&mut self, powers: &[Option<f64>]) {
        let bounded: Vec<(usize, super::LightBounds)> = powers
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_some_and(|p| p > 0.0))
            .filter_map(|(index, _)| Some((index, self.lights[index].bounds(self.luma)?)))
            .collect();
        let Some(tree) = LightTree::build(self.lights.len(), &bounded) else {
            tracing::debug!("light tree: at most one finite light, picking by power");
            return;
        };
        let live = (self.infinite.len() + bounded.len()) as u32;
        tracing::debug!(
            "light tree: {} lights, {} nodes, depth {}",
            bounded.len(),
            tree.node_count(),
            tree.depth()
        );
        self.cache = Some(std::sync::Arc::new(Learned::Tree(TreeSelection {
            tree,
            live,
            finite_share: bounded.len() as f32 / live as f32,
        })));
    }

    /// Installs a learned selection over the power one (see
    /// [`LightSelection::Learned`]). Must be built for this list's lights.
    pub(crate) fn set_cache(&mut self, cache: super::learned::LightCache) {
        self.cache = Some(std::sync::Arc::new(Learned::Grid(cache)));
    }

    /// Installs a neural visibility cache over the power selection (see
    /// [`LightSelection::Neural`]). Must be trained on this list's lights.
    pub(crate) fn set_visibility(&mut self, cache: super::neural::VisibilityCache) {
        debug_assert_eq!(cache.lights(), self.lights.len());
        self.cache = Some(std::sync::Arc::new(Learned::Neural(cache)));
    }

    /// The installed neural visibility cache, under
    /// [`LightSelection::Neural`]: for the probes that time or inspect it
    /// (`examples/nvc_bench.rs`).
    pub fn visibility_cache(&self) -> Option<&super::neural::VisibilityCache> {
        match self.cache.as_deref() {
            Some(Learned::Neural(c)) => Some(c),
            _ => None,
        }
    }

    /// Which strategy [`LightList::pick`] is using.
    pub fn selection(&self) -> LightSelection {
        match self.cache.as_deref() {
            Some(Learned::Grid(_)) => LightSelection::Learned,
            Some(Learned::Neural(_)) => LightSelection::Neural,
            Some(Learned::Tree(_)) => LightSelection::Tree,
            None if self.pmf.is_empty() => LightSelection::Uniform,
            None => LightSelection::Power,
        }
    }

    /// Whether the selection reads a receiver's normal and opacity, not just
    /// its position ([`LightSelection::Tree`]): the integrator asks the
    /// material whether it transmits only then.
    #[inline]
    pub fn reads_receiver_surface(&self) -> bool {
        matches!(self.cache.as_deref(), Some(Learned::Tree(_)))
    }

    /// The probability [`LightList::pick`] chooses light `index`.
    pub fn pmf(&self, index: usize) -> f32 {
        match self.pmf.get(index) {
            Some(&p) => p,
            None => 1.0 / self.lights.len() as f32,
        }
    }

    /// The light strategy's solid-angle density for a light chosen with
    /// probability `pmf` (from [`LightList::pick`], [`LightList::find_index_by_geom_at`]
    /// or [`LightList::iter`]) whose own `sample_li` density is `light_pdf`,
    /// at a vertex where NEE takes `samples` light samples: their product,
    /// times the count. Both MIS halves route through here, so they cannot
    /// disagree on it — the count is a parameter rather than a field so that
    /// every caller states the vertex's count (the bounce side carries the
    /// count of the vertex it left). Under uniform selection it is the division
    /// `light_pdf / n` it always was, not a multiplication by `1/n`, which
    /// rounds differently when `n` is not a power of two — so the default
    /// renders bit-identically to the renderer before selection was a choice;
    /// and one sample multiplies by exactly 1.
    ///
    /// With `samples` light samples and one bounce sample, Veach's
    /// multi-sample balance and power heuristics weigh each strategy by
    /// its sample count times its density, so the light side's effective
    /// density is `samples · light_pdf · pmf` on both halves: NEE averages
    /// its `samples` contributions, each weighted against the bounce with
    /// this density, and the bounce weighs a light it hits against it too.
    pub fn density(&self, light_pdf: PdfSolidAngle, pmf: f32, samples: u32) -> PdfSolidAngle {
        let one = if self.pmf.is_empty() {
            light_pdf.get() / self.lights.len() as f32
        } else {
            light_pdf.get() * pmf
        };
        PdfSolidAngle::from_measure(one * samples as f32)
    }

    /// Picks a light from one `[0, 1)` sample `u`, with the probability it
    /// was picked. `None` only for an empty list.
    pub fn pick(&self, u: f32) -> Option<(&LightKind, f32)> {
        self.pick_index(u)
            .map(|(index, pmf)| (&self.lights[index], pmf))
    }

    /// [`LightList::pick`] as an index into [`LightList::lights`].
    pub fn pick_index(&self, u: f32) -> Option<(usize, f32)> {
        let n = self.lights.len();
        if n == 0 {
            return None;
        }
        let index = if self.cdf.is_empty() {
            // Guard against `u == 1.0 - epsilon` rounding to `n`.
            ((u * n as f32) as usize).min(n - 1)
        } else {
            // The first light whose running sum exceeds `u`; a zero-power
            // light's slice of the CDF is empty, so it is never landed on.
            self.cdf.partition_point(|&c| c <= u).min(n - 1)
        };
        Some((index, self.pmf(index)))
    }

    /// [`LightList::pick`] for a receiver `r` (a position, or a position with
    /// a surface), as an index into [`LightList::lights`]: under a learned
    /// selection, from the distribution of the cell holding its position, or
    /// the network's there; under the tree, by walking it from `r`; otherwise
    /// exactly `pick_index`.
    ///
    /// Forced inline: it sits on every NEE pick, and LLVM's own threshold
    /// outlined it once `trace_path` grew by a few instructions elsewhere,
    /// which cost cornellbox 0.6% of its instructions (callgrind, 2 spp).
    /// The neural arm stays out of line, so the other selections' code is
    /// what it was.
    #[inline(always)]
    pub fn pick_index_at(&self, r: impl Into<Receiver>, u: f32) -> Option<(usize, f32)> {
        let r = r.into();
        match self.cache.as_deref() {
            None => self.pick_index(u),
            Some(Learned::Grid(c)) => match c.lookup(r.p) {
                Some((pmf, cdf)) => Some(self.pick_from(pmf, cdf, u)),
                None => self.pick_index(u),
            },
            Some(Learned::Neural(c)) => self.pick_index_neural(c, r.p, u),
            Some(Learned::Tree(t)) => self.pick_index_tree(t, &r, u),
        }
    }

    /// [`LightList::pick_index_at`]'s tree arm, out of line so the other
    /// selections' code is what it was. The first [`TREE_DEFENSIVE_SHARE`] of
    /// `u` picks by the power table; the rest picks from `q` (see
    /// [`TreeSelection`]): its first `n_inf / live` the lights at infinity,
    /// uniformly in list order, and the remainder the tree, walked from `r`.
    /// Each part is a monotone map of its slice of `u`. The probability
    /// returned is [`LightList::pmf_at`]'s, computed by the same functions.
    #[inline(never)]
    fn pick_index_tree(&self, t: &TreeSelection, r: &Receiver, u: f32) -> Option<(usize, f32)> {
        if u < TREE_DEFENSIVE_SHARE {
            let (index, _) = self.pick_index((u / TREE_DEFENSIVE_SHARE).min(ONE_MINUS_EPSILON))?;
            return Some((index, self.pmf_tree(t, r, index)));
        }
        let u = ((u - TREE_DEFENSIVE_SHARE) / (1.0 - TREE_DEFENSIVE_SHARE)).min(ONE_MINUS_EPSILON);
        let k = u * t.live as f32;
        let n_inf = self.infinite.len();
        if (k as usize) < n_inf {
            let index = self.infinite[k as usize] as usize;
            return Some((index, self.tree_mix(index, 1.0 / t.live as f32)));
        }
        let n_tree = t.live as usize - n_inf;
        let u = ((k - n_inf as f32) / n_tree as f32).min(ONE_MINUS_EPSILON);
        let (index, p_tree) = t.tree.sample(r, u)?;
        Some((index, self.tree_mix(index, t.finite_share * p_tree)))
    }

    /// The probability [`LightList::pick_index_tree`] picks light `index` at
    /// `r`.
    #[inline(never)]
    fn pmf_tree(&self, t: &TreeSelection, r: &Receiver, index: usize) -> f32 {
        let q = if t.tree.contains(index) {
            t.finite_share * t.tree.pmf(r, index)
        } else if self.lights[index].at_infinity() {
            1.0 / t.live as f32
        } else {
            0.0
        };
        self.tree_mix(index, q)
    }

    /// Light `index`'s pick probability under the tree, given its `q`: the
    /// one expression both MIS halves end in.
    #[inline(always)]
    fn tree_mix(&self, index: usize, q: f32) -> f32 {
        TREE_DEFENSIVE_SHARE * self.pmf[index] + (1.0 - TREE_DEFENSIVE_SHARE) * q
    }

    /// The first light whose running sum in `cdf` exceeds `u`, with its pmf.
    #[inline(always)]
    fn pick_from(&self, pmf: &[f32], cdf: &[f32], u: f32) -> (usize, f32) {
        let index = cdf.partition_point(|&c| c <= u).min(self.lights.len() - 1);
        (index, pmf[index])
    }

    /// [`LightList::pick_index_at`]'s neural arm. Cold, and out of line, so
    /// that it is one call in the code every other selection runs around
    /// each pick; under `neural` the inference it wraps costs hundreds of
    /// nanoseconds, against which the call is nothing.
    #[cold]
    #[inline(never)]
    fn pick_index_neural(
        &self,
        c: &super::neural::VisibilityCache,
        p: Vec3A,
        u: f32,
    ) -> Option<(usize, f32)> {
        c.with_table(p, |table| match table {
            Some((pmf, cdf)) => Some(self.pick_from(pmf, cdf, u)),
            None => self.pick_index(u),
        })
    }

    /// The probability [`LightList::pick_index_at`] at `r` picks light `index` —
    /// what the bounce side weights emission found from a vertex at `r` with.
    ///
    /// Forced inline, as `pick_index_at` is: the tree's arm grew it past
    /// LLVM's own threshold, and an out-of-line call per bounce-side lookup
    /// cost cornellbox 0.14% of its instructions under `power` (callgrind,
    /// 2 spp). The tree's and the network's arms stay out of line.
    #[inline(always)]
    pub fn pmf_at(&self, r: impl Into<Receiver>, index: usize) -> f32 {
        let r = r.into();
        match self.cache.as_deref() {
            None => self.pmf(index),
            Some(Learned::Grid(c)) => match c.lookup(r.p) {
                Some((pmf, _)) => pmf[index],
                None => self.pmf(index),
            },
            Some(Learned::Neural(c)) => self.pmf_at_neural(c, r.p, index),
            Some(Learned::Tree(t)) => self.pmf_tree(t, &r, index),
        }
    }

    /// [`LightList::pmf_at`]'s neural arm; cold for the same reason as
    /// [`LightList::pick_index_neural`].
    #[cold]
    #[inline(never)]
    fn pmf_at_neural(&self, c: &super::neural::VisibilityCache, p: Vec3A, index: usize) -> f32 {
        c.with_table(p, |table| match table {
            Some((pmf, _)) => pmf[index],
            None => self.pmf(index),
        })
    }

    /// The light whose scene geometry has world id `geom_id`, as an index into
    /// [`LightList::lights`], with the pick probability of a vertex at `r`: the
    /// bounce-side half of [`LightList::pick_index_at`]. Emissive geometry with
    /// no light-list entry returns `None`.
    pub fn find_index_by_geom_at(
        &self,
        geom_id: u32,
        r: impl Into<Receiver>,
    ) -> Option<(usize, f32)> {
        let &index = self.by_geom.get(&geom_id)?;
        Some((index, self.pmf_at(r, index)))
    }

    /// The lights at infinity with the pick probabilities of a vertex at `r`,
    /// in list order: what an escaping ray needs. A finite light's
    /// [`Light::escaped`] is `None` by contract, so skipping it changes
    /// nothing but the number of virtual calls.
    pub fn infinite_at(&self, r: impl Into<Receiver>) -> impl Iterator<Item = (&LightKind, f32)> {
        let r = r.into();
        // Each light's pmf through `pmf_at`, rather than a table looked up
        // once and carried: a table that could hold the network's pmf made
        // the iterator 270 bytes, paid for at every escaping ray (1.2% of the
        // default render's instructions, cornellbox at 2 spp). Under `power`
        // this is the read it always was; under `neural` the first light
        // fills the memo the others read.
        self.infinite.iter().map(move |&index| {
            let index = index as usize;
            (&self.lights[index], self.pmf_at(r, index))
        })
    }

    /// [`LightList::infinite_at`] restricted to the lights an escaping ray of
    /// category `mask` sees, with each light's index into [`LightList::lights`].
    pub fn infinite_indexed_seen_by(
        &self,
        r: impl Into<Receiver>,
        mask: RayMask,
    ) -> impl Iterator<Item = (usize, &LightKind, f32)> {
        let r = r.into();
        // Filtered first, then each survivor's pmf: no zip with
        // `infinite_at`'s iterator, which the compiler stopped inlining once
        // `pmf_at` grew a neural arm (0.9% of the default render's
        // instructions, cornellbox at 2 spp), and no pmf for a light the ray
        // cannot see.
        self.infinite
            .iter()
            .zip(&self.infinite_masks)
            .filter(move |(_, m)| m.sees(mask))
            .map(move |(&index, _)| {
                let index = index as usize;
                (index, &self.lights[index], self.pmf_at(r, index))
            })
    }

    /// The light at `index` of [`LightList::lights`].
    #[inline]
    pub fn light(&self, index: usize) -> &LightKind {
        &self.lights[index]
    }

    /// Installs light and shadow linking for the current lights. Indexed
    /// like [`LightList::lights`], so it must come after the last
    /// [`LightList::add`] and [`LightList::remove`].
    ///
    /// # Panics
    /// If a table's length is not the number of lights.
    pub fn set_links(&mut self, mut links: LightLinks) {
        let n = self.lights.len();
        assert!(
            links.illuminates.len() == n
                && links.shadow_masks.len() == n
                && links.restricted.len() == n
                && links.nee_only.len() == n,
            "light links are indexed like the lights"
        );
        debug_assert!(
            (0..n).all(|i| !links.nee_only[i] || links.restricted[i]),
            "NEE-only implies restricted"
        );
        // A dome finds nothing along a bounce direction
        // (`LightKind::found_along`), so a twinned dome would lose its bounce
        // side: a restricted dome is NEE-only, whoever built the links.
        for i in 0..n {
            if links.restricted[i] && self.lights[i].is_dome() {
                links.nee_only[i] = true;
            }
        }
        self.twinned = (0..n)
            .filter(|&i| links.restricted[i] && !links.nee_only[i])
            .map(|i| i as u32)
            .collect();
        self.links = Some(Box::new(links));
    }

    /// The linking tables, when any light authors a link.
    pub fn links(&self) -> Option<&LightLinks> {
        self.links.as_deref()
    }

    /// Whether light `index` illuminates a receiver of `class` (see
    /// [`EVERY_CLASS`]). Always true in a scene without light links.
    #[inline]
    pub fn illuminates(&self, index: usize, class: u16) -> bool {
        match &self.links {
            None => true,
            Some(_) if class == EVERY_CLASS => true,
            Some(l) => l.illuminates[index].as_ref().is_none_or(|bits| {
                let c = class as usize;
                bits.get(c / 64).is_some_and(|w| w & (1 << (c % 64)) != 0)
            }),
        }
    }

    /// The mask light `index`'s shadow rays carry: [`MASK_SHADOW`] in a
    /// scene without shadow links.
    #[inline]
    pub fn shadow_mask(&self, index: usize) -> RayMask {
        match &self.links {
            None => MASK_SHADOW,
            Some(l) => l.shadow_masks[index],
        }
    }

    /// Whether light `index` is sampled by NEE alone at non-delta vertices
    /// (a restricted dome; see [`LightLinks::nee_only`]). False in a scene
    /// without shadow links.
    #[inline]
    pub fn nee_only(&self, index: usize) -> bool {
        self.links.as_ref().is_some_and(|l| l.nee_only[index])
    }

    /// Whether light `index`'s shadow set is restricted
    /// ([`LightLinks::restricted`]). False in a scene without shadow links.
    #[inline]
    pub fn restricted(&self, index: usize) -> bool {
        self.links.as_ref().is_some_and(|l| l.restricted[index])
    }

    /// Whether light `index` is estimated on the bounce side by its link
    /// twin at a continuous vertex — restricted, and not NEE-only — so that
    /// the ordinary bounce must collect nothing from it there.
    #[inline]
    pub fn twinned(&self, index: usize) -> bool {
        self.links
            .as_ref()
            .is_some_and(|l| l.restricted[index] && !l.nee_only[index])
    }

    /// Whether the ordinary bounce collects nothing from light `index` at a
    /// continuous vertex: a twinned light under every strategy (its link
    /// twin owns that side), and a NEE-only one whenever the strategy samples
    /// lights (`samples_lights`), since then NEE owns it whole. One look at
    /// the link tables, for the integrator's bounce-side weights.
    #[inline]
    pub fn bounce_skips(&self, index: usize, samples_lights: bool) -> bool {
        match &self.links {
            None => false,
            Some(l) => l.restricted[index] && (!l.nee_only[index] || samples_lights),
        }
    }

    /// The lights [`LightList::twinned`] holds for, in list order: what a
    /// continuous vertex runs a link twin for. Empty without shadow links.
    #[inline]
    pub fn twinned_lights(&self) -> &[u32] {
        &self.twinned
    }

    /// Hides every light at infinity from the camera, backdrops included —
    /// the `domeLightCameraVisibility = false` render setting. Nothing else
    /// changes: which lights illuminate, and how they are selected, stays.
    pub fn hide_infinite_from_camera(&mut self) {
        for m in &mut self.infinite_masks {
            *m = RayMask(m.0 & !MASK_CAMERA.0);
        }
        self.backdrops.clear();
    }

    /// Adds a backdrop: a light at infinity that illuminates nothing and
    /// that escaping camera rays see in front of every other light at
    /// infinity. Not a selectable light — see the `backdrops` field.
    pub fn add_backdrop(&mut self, light: impl Into<LightKind>) {
        let light = light.into();
        debug_assert!(light.at_infinity(), "a backdrop is a light at infinity");
        self.backdrops.push(light);
    }

    /// The backdrops, in the order they were added.
    pub fn backdrops(&self) -> &[LightKind] {
        &self.backdrops
    }

    /// Whether a ray of category `mask` escaping the scene sees the
    /// backdrops (and only them): a camera ray, when there are any.
    #[inline]
    pub fn escapes_to_backdrop(&self, mask: RayMask) -> bool {
        !self.backdrops.is_empty() && mask.sees(MASK_CAMERA)
    }

    /// Every light with its selection probability.
    pub fn iter(&self) -> impl Iterator<Item = (&LightKind, f32)> {
        self.lights
            .iter()
            .enumerate()
            .map(|(index, light)| (light, self.pmf(index)))
    }

    /// The lights, in the order they were added.
    pub fn lights(&self) -> &[LightKind] {
        &self.lights
    }

    /// Lights grouped by [`Light::kind`], most numerous first.
    pub fn kind_breakdown(&self) -> Vec<(&'static str, usize)> {
        crate::stats::breakdown(self.lights.iter().map(|l| l.kind()))
    }

    /// Returns the number of lights in the `LightList`.
    pub fn count(&self) -> usize {
        self.lights.len()
    }
}

/// A light's identity in the list: what a replacement must keep.
pub(crate) fn same_light_shape(old: &LightKind, new: &LightKind) -> bool {
    std::mem::discriminant(old) == std::mem::discriminant(new)
        && old.at_infinity() == new.at_infinity()
        && old.geom_id() == new.geom_id()
}
