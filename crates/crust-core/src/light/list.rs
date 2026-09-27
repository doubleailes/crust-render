//! [`LightList`] and NEE's light selection ([`LightSelection`]): uniform,
//! defensive power, or the learned per-cell table.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec3A;

use super::Light;
use crate::pdf::PdfSolidAngle;

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

/// Under [`LightSelection::Power`], the share of the finite lights' shadow
/// rays split evenly among them rather than by power (Hesterberg's
/// defensive importance sampling).
pub const DEFENSIVE_SHARE: f64 = 0.5;

/// The scene's lights, and how NEE picks one of them.
///
/// The pick's probability is half of the light strategy's MIS density (the
/// other half is the light's own `sample_li` pdf), so whatever
/// [`LightList::pick`] reports, [`LightList::find_by_geom`] and
/// [`LightList::iter`] report the same number for the same light: the bounce
/// side weights emission it found by chance with it, and the two sides must
/// describe one strategy or emission is double-counted.
pub struct LightList {
    /// The lights. Private, so that [`LightList::add`] is the only way in:
    /// it keeps the geometry index and the selection in step with this
    /// vector, and a light pushed past it would be sampled by NEE yet
    /// unattributed on the bounce side. Read it through
    /// [`LightList::lights`].
    pub(super) lights: Vec<Arc<dyn Light>>,
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
    /// The learned per-region selection, under [`LightSelection::Learned`].
    /// Consulted by every `*_at` method; `pmf` / `cdf` are what it falls back
    /// to outside trained cells.
    pub(super) cache: Option<std::sync::Arc<crate::light_cache::LightCache>>,
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
            cache: None,
        }
    }

    /// Adds a light source. The selection falls back to uniform until the
    /// next [`LightList::select_by`], since a table built over the old list
    /// would describe the wrong one.
    pub fn add(&mut self, light: Arc<dyn Light>) {
        if let Some(id) = light.geom_id() {
            self.by_geom.insert(id, self.lights.len());
        }
        if light.at_infinity() {
            self.infinite.push(self.lights.len() as u32);
        }
        self.lights.push(light);
        self.pmf.clear();
        self.cdf.clear();
        self.cache = None;
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
                l.power().map(|p| {
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
    /// probability `pmf` (from [`LightList::pick`], [`LightList::find_by_geom`]
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
    pub fn pick(&self, u: f32) -> Option<(&Arc<dyn Light>, f32)> {
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
        Some((&self.lights[index], self.pmf(index)))
    }

    /// Finds the light whose scene geometry has world id `geom_id`, with its
    /// selection probability. Used by the integrator to attribute a
    /// bounce-hit emissive surface to its light for MIS; emissive geometry
    /// with no light-list entry returns `None`.
    pub fn find_by_geom(&self, geom_id: u32) -> Option<(&Arc<dyn Light>, f32)> {
        let &index = self.by_geom.get(&geom_id)?;
        Some((&self.lights[index], self.pmf(index)))
    }

    /// [`LightList::pick`] for a vertex at `p`: under a learned selection, from
    /// the distribution of the cell holding `p`; otherwise exactly `pick`.
    #[inline]
    pub fn pick_at(&self, p: Vec3A, u: f32) -> Option<(&Arc<dyn Light>, f32)> {
        match self.cache.as_ref().and_then(|c| c.lookup(p)) {
            Some((pmf, cdf)) => {
                let index = cdf.partition_point(|&c| c <= u).min(self.lights.len() - 1);
                Some((&self.lights[index], pmf[index]))
            }
            None => self.pick(u),
        }
    }

    /// The probability [`LightList::pick_at`] at `p` picks light `index` —
    /// what the bounce side weights emission found from a vertex at `p` with.
    #[inline]
    pub fn pmf_at(&self, p: Vec3A, index: usize) -> f32 {
        match self.cache.as_ref().and_then(|c| c.lookup(p)) {
            Some((pmf, _)) => pmf[index],
            None => self.pmf(index),
        }
    }

    /// [`LightList::find_by_geom`] with the pick probability of a vertex at
    /// `p`: the bounce-side half of [`LightList::pick_at`].
    pub fn find_by_geom_at(&self, geom_id: u32, p: Vec3A) -> Option<(&Arc<dyn Light>, f32)> {
        let &index = self.by_geom.get(&geom_id)?;
        Some((&self.lights[index], self.pmf_at(p, index)))
    }

    /// [`LightList::iter`] with the pick probabilities of a vertex at `p`.
    pub fn iter_at(&self, p: Vec3A) -> impl Iterator<Item = (&Arc<dyn Light>, f32)> {
        let table = self.cache.as_ref().and_then(|c| c.lookup(p)).map(|t| t.0);
        self.lights.iter().enumerate().map(move |(index, light)| {
            (
                light,
                match table {
                    Some(pmf) => pmf[index],
                    None => self.pmf(index),
                },
            )
        })
    }

    /// [`LightList::iter_at`] restricted to the lights at infinity, in the
    /// same order: what an escaping ray needs. A finite light's
    /// [`Light::escaped`] is `None` by contract, so skipping it changes
    /// nothing but the number of virtual calls.
    pub fn infinite_at(&self, p: Vec3A) -> impl Iterator<Item = (&Arc<dyn Light>, f32)> {
        let table = if self.infinite.is_empty() {
            None
        } else {
            self.cache.as_ref().and_then(|c| c.lookup(p)).map(|t| t.0)
        };
        self.infinite.iter().map(move |&index| {
            let index = index as usize;
            (
                &self.lights[index],
                match table {
                    Some(pmf) => pmf[index],
                    None => self.pmf(index),
                },
            )
        })
    }

    /// Every light with its selection probability.
    pub fn iter(&self) -> impl Iterator<Item = (&Arc<dyn Light>, f32)> {
        self.lights
            .iter()
            .enumerate()
            .map(|(index, light)| (light, self.pmf(index)))
    }

    /// The lights, in the order they were added.
    pub fn lights(&self) -> &[Arc<dyn Light>] {
        &self.lights
    }

    /// Returns the number of lights in the `LightList`.
    /// Lights grouped by [`Light::kind`], most numerous first.
    pub fn kind_breakdown(&self) -> Vec<(&'static str, usize)> {
        let mut out: Vec<(&'static str, usize)> = Vec::new();
        for (light, _) in self.iter() {
            let kind = light.kind();
            match out.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, n)) => *n += 1,
                None => out.push((kind, 1)),
            }
        }
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        out
    }

    pub fn count(&self) -> usize {
        self.lights.len()
    }
}
