//! [`LightList`] and NEE's light selection ([`LightSelection`]): uniform,
//! defensive power, or the learned per-cell table.

use std::collections::HashMap;

use glam::Vec3A;

use super::{Light, LightKind};
use crate::pdf::PdfSolidAngle;
use crate::ray::{MASK_ALL, MASK_CAMERA, MASK_SHADOW, RayMask};

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
    ]
);

/// Under [`LightSelection::Power`], the share of the finite lights' shadow
/// rays split evenly among them rather than by power (Hesterberg's
/// defensive importance sampling).
pub const DEFENSIVE_SHARE: f64 = 0.5;

/// The class of a receiver that every light illuminates: a vertex with no
/// prim to judge (a volume-region scatter), or any vertex of a scene whose
/// links were never resolved.
pub const EVERY_CLASS: u16 = u16::MAX;

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
    /// Per light, whether its shadow set is restricted: such a light is
    /// sampled by NEE alone at non-delta vertices, because a bounce ray is
    /// stopped by the occluders its shadow rays ignore.
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
    /// The learned per-region selection, under [`LightSelection::Learned`].
    /// Consulted by every `*_at` method; `pmf` / `cdf` are what it falls back
    /// to where no trained cell is near.
    pub(super) cache: Option<std::sync::Arc<crate::light_cache::LightCache>>,
    /// Light and shadow linking, when any light authors a link.
    pub(super) links: Option<Box<LightLinks>>,
    /// Per light, its light-path-expression tag (`crust:light:lpeTag`): the
    /// label its `L` events carry, so `<L.'key'>` selects it. Kept in step
    /// with `lights` by `add_masked` and `remove`.
    pub(super) lpe_tags: Vec<Option<Box<str>>>,
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
            lpe_tags: Vec::new(),
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
    /// [`LightSelection::Learned`] builds the power table here; the learned
    /// part needs the scene and is added by the renderer
    /// ([`LightList::set_cache`]).
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
    }

    /// Installs a learned selection over the power one (see
    /// [`LightSelection::Learned`]). Must be built for this list's lights.
    pub(crate) fn set_cache(&mut self, cache: crate::light_cache::LightCache) {
        self.cache = Some(std::sync::Arc::new(cache));
    }

    /// Which strategy [`LightList::pick`] is using.
    pub fn selection(&self) -> LightSelection {
        if self.cache.is_some() {
            LightSelection::Learned
        } else if self.pmf.is_empty() {
            LightSelection::Uniform
        } else {
            LightSelection::Power
        }
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
    /// or [`LightList::iter`]) whose own `sample_li` density is `light_pdf`:
    /// their product. Both MIS halves route through here, so they cannot
    /// disagree on it. Under uniform selection it is the division
    /// `light_pdf / n` it always was, not a multiplication by `1/n`, which
    /// rounds differently when `n` is not a power of two — so the default
    /// renders bit-identically to the renderer before selection was a choice.
    pub fn density(&self, light_pdf: PdfSolidAngle, pmf: f32) -> PdfSolidAngle {
        PdfSolidAngle::from_measure(if self.pmf.is_empty() {
            light_pdf.get() / self.lights.len() as f32
        } else {
            light_pdf.get() * pmf
        })
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

    /// [`LightList::pick`] for a vertex at `p`, as an index into
    /// [`LightList::lights`]: under a learned selection, from the distribution
    /// of the cell holding `p`; otherwise exactly `pick_index`.
    ///
    /// Forced inline: it sits on every NEE pick, and LLVM's own threshold
    /// outlined it once `trace_path` grew by a few instructions elsewhere,
    /// which cost cornellbox 0.6% of its instructions (callgrind, 2 spp).
    #[inline(always)]
    pub fn pick_index_at(&self, p: Vec3A, u: f32) -> Option<(usize, f32)> {
        match self
            .cache
            .as_ref()
            .and_then(|c| c.blend_at(p).map(|b| (c, b)))
        {
            Some((cache, blend)) => Some(cache.pick(&blend, u)),
            None => self.pick_index(u),
        }
    }

    /// The probability [`LightList::pick_index_at`] at `p` picks light `index` —
    /// what the bounce side weights emission found from a vertex at `p` with.
    #[inline]
    pub fn pmf_at(&self, p: Vec3A, index: usize) -> f32 {
        match self
            .cache
            .as_ref()
            .and_then(|c| c.blend_at(p).map(|b| (c, b)))
        {
            Some((cache, blend)) => cache.pmf_at(&blend, index),
            None => self.pmf(index),
        }
    }

    /// The light whose scene geometry has world id `geom_id`, as an index into
    /// [`LightList::lights`], with the pick probability of a vertex at `p`: the
    /// bounce-side half of [`LightList::pick_index_at`]. Emissive geometry with
    /// no light-list entry returns `None`.
    pub fn find_index_by_geom_at(&self, geom_id: u32, p: Vec3A) -> Option<(usize, f32)> {
        let &index = self.by_geom.get(&geom_id)?;
        Some((index, self.pmf_at(p, index)))
    }

    /// The lights at infinity with the pick probabilities of a vertex at `p`,
    /// in list order: what an escaping ray needs. A finite light's
    /// [`Light::escaped`] is `None` by contract, so skipping it changes
    /// nothing but the number of virtual calls.
    pub fn infinite_at(&self, p: Vec3A) -> impl Iterator<Item = (&LightKind, f32)> {
        let blend = if self.infinite.is_empty() {
            None
        } else {
            self.cache
                .as_ref()
                .and_then(|c| c.blend_at(p).map(|b| (c, b)))
        };
        self.infinite.iter().map(move |&index| {
            let index = index as usize;
            (
                &self.lights[index],
                match &blend {
                    Some((cache, b)) => cache.pmf_at(b, index),
                    None => self.pmf(index),
                },
            )
        })
    }

    /// [`LightList::infinite_at`] restricted to the lights an escaping ray of
    /// category `mask` sees, with each light's index into [`LightList::lights`].
    pub fn infinite_indexed_seen_by(
        &self,
        p: Vec3A,
        mask: RayMask,
    ) -> impl Iterator<Item = (usize, &LightKind, f32)> {
        self.infinite_at(p)
            .zip(self.infinite.iter().zip(&self.infinite_masks))
            .filter(move |(_, (_, m))| m.sees(mask))
            .map(|((light, pmf), (&index, _))| (index as usize, light, pmf))
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
    pub fn set_links(&mut self, links: LightLinks) {
        let n = self.lights.len();
        assert!(
            links.illuminates.len() == n
                && links.shadow_masks.len() == n
                && links.nee_only.len() == n,
            "light links are indexed like the lights"
        );
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
    /// (its shadow set is restricted). False in a scene without shadow links.
    #[inline]
    pub fn nee_only(&self, index: usize) -> bool {
        self.links.as_ref().is_some_and(|l| l.nee_only[index])
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
