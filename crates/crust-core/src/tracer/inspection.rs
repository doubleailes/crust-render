//! The record store (design D5): per work unit, a bounded heap of the
//! brightest records and one aggregate per group key, merged in unit order
//! once the render returns, so the same command keeps the same records and
//! groups with any thread count.
//!
//! Nothing here allocates per record once a unit's buffers have grown: a
//! group key is the record's events and light, interned as a `[u16]`, and a
//! record is copied out of the fold only when it enters the heap.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

use glam::Vec3A;

use super::{NO_LIGHT, PathTerm};
use crate::region::PixelRect;

/// What the records are aggregated by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupBy {
    /// No groups: the brightest records only.
    None,
    /// The light that ends the path.
    Light,
    /// The expression, the light named by its tag.
    Expr,
    /// The expression and the light.
    #[default]
    ExprLight,
    /// The expression, each vertex's prim and the light: not available
    /// until a hit can be resolved to its prim ([`PRIMS_NOTE`](crate::inspect::PRIMS_NOTE)).
    ExprPrimsLight,
}

crate::names::named!(
    GroupBy,
    "grouping",
    [
        (
            GroupBy::None,
            "none",
            "No groups: the brightest records only"
        ),
        (GroupBy::Light, "light", "By the light that ends the path"),
        (
            GroupBy::Expr,
            "expr",
            "By the path's expression (lights told apart by their LPE tag)"
        ),
        (
            GroupBy::ExprLight,
            "expr+light",
            "By the expression and the light (default)"
        ),
        (
            GroupBy::ExprPrimsLight,
            "expr+prims+light",
            "By the expression, the prims it bounced on and the light (not yet available)"
        ),
    ]
);

/// What every unit of an inspection is configured with.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// Records kept, the brightest first.
    pub(crate) top: usize,
    pub(crate) group_by: GroupBy,
    /// The inspected pixels, in the tracer's raster space.
    pub(crate) rect: PixelRect,
    /// The frame's height, which flips a raster row into the image's.
    pub(crate) height: usize,
    /// Per light-list entry, its writable LPE tag's id, 0 for none: what an
    /// `expr` key tells lights apart by, as the expression's `<L.'tag'>`
    /// does.
    pub(crate) tags: Arc<[u32]>,
    pub(crate) luma: utils::Luma,
    /// The dominant share below which a record's label is ambiguous.
    pub(crate) ambiguous_below: f32,
}

/// A record's place in the ranking (D5): luminance descending, then image
/// row, column, sample and the record's index within its sample ascending
/// — a total order, so the kept records do not depend on which unit or
/// thread found them. Greater is brighter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Rank {
    pub(crate) luma: f32,
    pub(crate) y: u32,
    pub(crate) x: u32,
    pub(crate) sample: u32,
    pub(crate) index: u32,
}

impl Eq for Rank {}

impl Ord for Rank {
    fn cmp(&self, other: &Self) -> Ordering {
        self.luma
            .total_cmp(&other.luma)
            .then(other.y.cmp(&self.y))
            .then(other.x.cmp(&self.x))
            .then(other.sample.cmp(&self.sample))
            .then(other.index.cmp(&self.index))
    }
}

impl PartialOrd for Rank {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// One scattering vertex's events, each with its share of the vertex's
/// luminance.
pub(crate) type Split = Box<[(u16, f32)]>;

/// A record kept for the report, copied out of the fold.
#[derive(Debug, Clone)]
pub(crate) struct Kept {
    pub(crate) rank: Rank,
    /// Where the sample was taken, in image space: pixel `(x, y)` covers
    /// `[x, x + 1] × [y, y + 1]`.
    pub(crate) coordinates: [f32; 2],
    /// Its contribution to the sample's radiance, unexposed, before and
    /// after the indirect clamp.
    pub(crate) value: Vec3A,
    pub(crate) clamped: Vec3A,
    /// The events after `C` ([`PathTerm::events`]).
    pub(crate) events: Box<[u16]>,
    /// Per scattering event, in path order, its events' shares.
    pub(crate) splits: Box<[Split]>,
    pub(crate) light: u32,
    pub(crate) min_share: f32,
}

impl PartialEq for Kept {
    fn eq(&self, other: &Self) -> bool {
        self.rank == other.rank
    }
}

impl Eq for Kept {}

impl Ord for Kept {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank.cmp(&other.rank)
    }
}

impl PartialOrd for Kept {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A light-list index meaning: the group's records end on more than one
/// light.
pub(crate) const MIXED_LIGHTS: u32 = u32::MAX - 1;

/// One group's aggregate.
#[derive(Debug, Clone)]
pub(crate) struct GroupAcc {
    pub(crate) records: u64,
    /// Samples with at least one record in the group.
    pub(crate) hits: u64,
    /// The serial of the last sample counted in `hits` (0: none).
    last: u64,
    pub(crate) max_luma: f32,
    /// Σ over samples of `x · x_r` for the group's records (D6).
    pub(crate) second_moment: f64,
    /// Per pixel (its region index) that holds any of its records: Σ of
    /// filter weight × luminance, unclamped and clamped.
    pub(crate) pixels: Vec<(u32, f64, f64)>,
    /// The `pixels` entry the last record went to: a pixel's samples come
    /// in runs.
    cache: usize,
    /// The first record's events (under `light`, the last alone) and light,
    /// or [`MIXED_LIGHTS`] once another light joined; and that first light
    /// whatever joined (its tag is the group's under `expr`).
    pub(crate) events: Box<[u16]>,
    pub(crate) light: u32,
    pub(crate) first_light: u32,
}

/// One pixel's sums, mirroring the beauty's own accumulators.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PixelAcc {
    pub(crate) taken: u32,
    /// `Σ wᵢ` and `Σ wᵢ Lᵢ`, accumulated as `PixelState` accumulates them,
    /// so `beauty / weight` is the pass's pixel.
    pub(crate) weight: f32,
    pub(crate) beauty: Vec3A,
    /// `Σ wᵢ · Σ_r clamped_r`: what the records rebuild of `beauty`.
    pub(crate) records: Vec3A,
    /// `Σ |wᵢ| · max(|Lᵢ|, Σ_r |r|)`, largest channel: the size of the
    /// terms `beauty` sums, against which `records` is compared.
    pub(crate) scale: f32,
    /// Σ of filter weight × the records' luminance, unclamped, clamped,
    /// and of the ambiguously labelled records alone.
    pub(crate) luma: f64,
    pub(crate) luma_clamped: f64,
    pub(crate) luma_ambiguous: f64,
}

impl PixelAcc {
    fn add(&mut self, o: &PixelAcc) {
        self.taken += o.taken;
        self.weight += o.weight;
        self.beauty += o.beauty;
        self.records += o.records;
        self.scale += o.scale;
        self.luma += o.luma;
        self.luma_clamped += o.luma_clamped;
        self.luma_ambiguous += o.luma_ambiguous;
    }

    /// The pixel estimator's divisor: the beauty's (`PixelState::estimate`).
    pub(crate) fn divisor(&self) -> f64 {
        if self.weight > 0.0 {
            self.weight as f64
        } else {
            self.taken.max(1) as f64
        }
    }
}

/// The sample being folded.
#[derive(Debug, Default)]
struct Current {
    pixel: u32,
    x: u32,
    y: u32,
    sample: u32,
    coordinates: [f32; 2],
    weight: f32,
    /// Records so far: the next one's index.
    index: u32,
    /// Σ of the records' clamped values, and of their largest channels.
    sum: Vec3A,
    size: f32,
    /// Per record, its group (or `u32::MAX` without groups) and its share
    /// of the sample's second moment, `|w|·max(luma, 0)`.
    pending: Vec<(u32, f64)>,
}

/// A work unit's records.
#[derive(Debug)]
pub(crate) struct UnitStore {
    cfg: Arc<Config>,
    heap: BinaryHeap<Reverse<Kept>>,
    keys: HashMap<Box<[u16]>, u32>,
    groups: Vec<GroupAcc>,
    key: Vec<u16>,
    /// Per region pixel; only the unit's own are touched.
    pixels: Vec<PixelAcc>,
    cur: Current,
    /// Samples folded: the current sample's serial.
    samples: u64,
    records: u64,
    /// The largest relative difference between a sample's radiance and its
    /// records' sum.
    max_sample_error: f32,
}

impl UnitStore {
    pub(crate) fn new(cfg: Arc<Config>) -> UnitStore {
        UnitStore {
            pixels: vec![PixelAcc::default(); cfg.rect.area()],
            heap: BinaryHeap::with_capacity(cfg.top.min(1024) + 1),
            cfg,
            keys: HashMap::new(),
            groups: Vec::new(),
            key: Vec::new(),
            cur: Current::default(),
            samples: 0,
            records: 0,
            max_sample_error: 0.0,
        }
    }

    /// Opens camera sample `sample` of raster pixel `(i, j)`, taken at film
    /// offset `(fx, fy)` from the pixel's corner with filter weight `weight`.
    pub(crate) fn begin_sample(
        &mut self,
        i: usize,
        j: usize,
        sample: u32,
        fx: f32,
        fy: f32,
        weight: f32,
    ) {
        self.samples += 1;
        let height = self.cfg.height;
        let cur = &mut self.cur;
        cur.pixel = self.cfg.rect.index(i, j) as u32;
        cur.x = i as u32;
        cur.y = (height - 1 - j) as u32;
        cur.sample = sample;
        // Raster rows grow upwards: the sample sits `j + fy` from the bottom.
        cur.coordinates = [i as f32 + fx, height as f32 - (j as f32 + fy)];
        cur.weight = weight;
        cur.index = 0;
        cur.sum = Vec3A::ZERO;
        cur.size = 0.0;
        cur.pending.clear();
    }

    /// One record of the open sample.
    pub(crate) fn add(&mut self, t: &PathTerm) {
        let cfg = &*self.cfg;
        let cur = &mut self.cur;
        let index = cur.index;
        cur.index += 1;
        self.records += 1;
        cur.sum += t.clamped;
        cur.size += t.clamped.abs().max_element();
        let luma = cfg.luma.of(t.value);
        let luma_clamped = cfg.luma.of(t.clamped);
        let w = cur.weight as f64;
        let px = &mut self.pixels[cur.pixel as usize];
        px.luma += w * luma as f64;
        px.luma_clamped += w * luma_clamped as f64;
        if t.min_share < cfg.ambiguous_below {
            px.luma_ambiguous += w * luma as f64;
        }
        let x_r = (cur.weight.abs() * luma.max(0.0)) as f64;

        let group = if cfg.group_by == GroupBy::None {
            u32::MAX
        } else {
            let light = t.light;
            group_key(cfg, t.events, light, &mut self.key);
            let g = match self.keys.get(&self.key[..]) {
                Some(&g) => g,
                None => {
                    let g = self.groups.len() as u32;
                    self.keys.insert(self.key.clone().into_boxed_slice(), g);
                    self.groups.push(GroupAcc {
                        records: 0,
                        hits: 0,
                        last: 0,
                        max_luma: f32::NEG_INFINITY,
                        second_moment: 0.0,
                        pixels: Vec::new(),
                        cache: 0,
                        // Under `light`, the ending event alone: it tells
                        // a backdrop from an `O`.
                        events: if cfg.group_by == GroupBy::Light {
                            t.events[t.events.len().saturating_sub(1)..].into()
                        } else {
                            t.events.into()
                        },
                        light,
                        first_light: light,
                    });
                    g
                }
            };
            let acc = &mut self.groups[g as usize];
            acc.records += 1;
            if acc.last != self.samples {
                acc.last = self.samples;
                acc.hits += 1;
            }
            if luma > acc.max_luma || acc.records == 1 {
                acc.max_luma = luma;
            }
            if acc.light != light {
                acc.light = MIXED_LIGHTS;
            }
            let at = match acc.pixels.get(acc.cache) {
                Some(p) if p.0 == cur.pixel => acc.cache,
                _ => match acc.pixels.iter().position(|p| p.0 == cur.pixel) {
                    Some(at) => at,
                    None => {
                        acc.pixels.push((cur.pixel, 0.0, 0.0));
                        acc.pixels.len() - 1
                    }
                },
            };
            acc.cache = at;
            acc.pixels[at].1 += w * luma as f64;
            acc.pixels[at].2 += w * luma_clamped as f64;
            g
        };
        cur.pending.push((group, x_r));

        if cfg.top == 0 {
            return;
        }
        let rank = Rank {
            luma,
            y: cur.y,
            x: cur.x,
            sample: cur.sample,
            index,
        };
        if self.heap.len() >= cfg.top
            && self
                .heap
                .peek()
                .is_some_and(|Reverse(worst)| rank <= worst.rank)
        {
            return;
        }
        let splits = t
            .splits
            .iter()
            .filter(|(a, b)| a < b)
            .map(|&(a, b)| t.shares[a as usize..b as usize].into())
            .collect();
        if self.heap.len() >= cfg.top {
            self.heap.pop();
        }
        self.heap.push(Reverse(Kept {
            rank,
            coordinates: cur.coordinates,
            value: t.value,
            clamped: t.clamped,
            events: t.events.into(),
            splits,
            light: t.light,
            min_share: t.min_share,
        }));
    }

    /// Closes the open sample, whose radiance the beauty took as `radiance`
    /// (unweighted, unexposed).
    pub(crate) fn end_sample(&mut self, radiance: Vec3A) {
        let cur = &mut self.cur;
        let w = cur.weight;
        let px = &mut self.pixels[cur.pixel as usize];
        px.taken += 1;
        px.weight += w;
        px.beauty += radiance * w;
        px.records += cur.sum * w;
        let size = radiance.abs().max_element().max(cur.size);
        px.scale += w.abs() * size;
        let error = (radiance - cur.sum).abs().max_element();
        let relative = if error == 0.0 {
            0.0
        } else if size > 0.0 && !error.is_nan() {
            error / size
        } else {
            // A NaN on either side is the largest error there is.
            f32::INFINITY
        };
        self.max_sample_error = self.max_sample_error.max(relative);
        // The sample's second moment, `x²`, split by each record's share
        // `x_r / x` (records are taken as non-negative here).
        let x: f64 = cur.pending.iter().map(|p| p.1).sum();
        for &(g, x_r) in &cur.pending {
            if let Some(acc) = self.groups.get_mut(g as usize) {
                acc.second_moment += x * x_r;
            }
        }
    }
}

/// Every unit's records, merged in unit order.
#[derive(Debug, Default)]
pub(crate) struct Collected {
    /// The brightest records, brightest first.
    pub(crate) kept: Vec<Kept>,
    /// In the order the keys were first met, unit by unit.
    pub(crate) groups: Vec<GroupAcc>,
    /// Per region pixel (raster region index).
    pub(crate) pixels: Vec<PixelAcc>,
    pub(crate) samples: u64,
    pub(crate) records: u64,
    pub(crate) max_sample_error: f32,
    /// Each group's key ([`group_key`]): its index in `groups`.
    pub(crate) keys: HashMap<Box<[u16]>, u32>,
}

impl Collected {
    /// Merges `units` in their order. Each pixel belongs to one unit, so
    /// the per-pixel sums only move; the groups' sums gather units in order.
    pub(crate) fn merge(units: Vec<UnitStore>) -> Collected {
        let mut out = Collected::default();
        let keys = &mut out.keys;
        let mut top = 0;
        for unit in units {
            top = unit.cfg.top;
            if out.pixels.is_empty() {
                out.pixels = vec![PixelAcc::default(); unit.pixels.len()];
            }
            for (o, p) in out.pixels.iter_mut().zip(&unit.pixels) {
                if p.taken > 0 {
                    o.add(p);
                }
            }
            out.samples += unit.samples;
            out.records += unit.records;
            out.max_sample_error = out.max_sample_error.max(unit.max_sample_error);
            out.kept.extend(unit.heap.into_iter().map(|Reverse(k)| k));
            // Each unit's groups in the order it met them.
            let mut local: Vec<(Box<[u16]>, u32)> = unit.keys.into_iter().collect();
            local.sort_unstable_by_key(|(_, g)| *g);
            let mut groups: Vec<Option<GroupAcc>> = unit.groups.into_iter().map(Some).collect();
            for (key, g) in local {
                let acc = groups[g as usize].take().expect("each group once");
                match keys.get(&key) {
                    Some(&at) => {
                        let o = &mut out.groups[at as usize];
                        o.records += acc.records;
                        o.hits += acc.hits;
                        if acc.max_luma > o.max_luma {
                            o.max_luma = acc.max_luma;
                        }
                        o.second_moment += acc.second_moment;
                        o.pixels.extend(acc.pixels);
                        if o.light != acc.light {
                            o.light = MIXED_LIGHTS;
                        }
                    }
                    None => {
                        keys.insert(key, out.groups.len() as u32);
                        out.groups.push(acc);
                    }
                }
            }
        }
        out.kept.sort_unstable_by(|a, b| b.cmp(a));
        out.kept.truncate(top);
        out
    }
}

/// The key `cfg.group_by` files a record of `events` ending on `light`
/// under, into `key`: the events (none for `light`) and what tells their
/// ends apart — the light's index, or under `expr` its tag's id (lights
/// sharing a tag share the expression `<L.'tag'>`), or under `light` the
/// ending event too, so a backdrop's `L` and an `O` stay apart.
pub(crate) fn group_key(cfg: &Config, events: &[u16], light: u32, key: &mut Vec<u16>) {
    key.clear();
    let ends = match cfg.group_by {
        GroupBy::Light => {
            key.extend(events.last());
            light
        }
        GroupBy::Expr => {
            key.extend_from_slice(events);
            cfg.tags.get(light as usize).copied().unwrap_or(0)
        }
        _ => {
            key.extend_from_slice(events);
            light
        }
    };
    key.extend([(ends >> 16) as u16, ends as u16]);
}

/// Whether a record's light is a light-list entry.
pub(crate) fn is_light(light: u32) -> bool {
    light != NO_LIGHT && light != MIXED_LIGHTS
}
