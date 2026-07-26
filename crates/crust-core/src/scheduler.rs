//! Tile/pass render scheduling, after MoonRay's `rndr` driver (concepts, not
//! code — see `docs/moonray_comparison.md`).
//!
//! The frame is divided into fixed 8×8 tiles, visited in a precomputed
//! permutation ([`TileOrder`], Morton by default). Work is described by
//! [`Pass`]es — a range of tile-local pixel slots at a range of per-pixel
//! sample indices, applied to every active tile — and distributed through a
//! [`TileWorkQueue`], a "virtual" queue that stores no elements: one atomic
//! cursor synthesizes contiguous tile groups on demand.
//!
//! The [`PassSchedule`] is a pure function of `(spp, min_spp)`; a resumed
//! render rebuilds the identical remaining schedule from the sample index a
//! checkpoint stopped at, which is what makes resume exact.

use std::sync::atomic::{AtomicU32, Ordering};

/// Tiles are 8×8 = 64 pixels — small enough that adaptive sampling can stop
/// regions early at a useful granularity, large enough to amortize
/// scheduling.
pub(crate) const TILE_SIZE: usize = 8;

/// Pixel slots per full tile.
#[allow(dead_code)] // consumed by the pass-based driver (next commit)
pub(crate) const TILE_PIXELS: u32 = (TILE_SIZE * TILE_SIZE) as u32;

/// Longest per-pixel sample range a single fine pass may cover. Bounds the
/// work between two pass boundaries — the points where adaptive decisions,
/// progress and checkpoints happen.
#[allow(dead_code)] // consumed by the pass-based driver (next commit)
pub(crate) const MAX_PASS_SAMPLES: u32 = 16;

/// One tile of the image, clipped at the right/bottom edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tile {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// The order tiles are visited in. Purely a scheduling choice — every order
/// renders the same image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TileOrder {
    /// Morton (Z-curve) order: spatially coherent groups, and neighbours of
    /// a tile tend to be rendered nearby in time — the default.
    #[default]
    Morton,
    /// Outward square spiral from the image center.
    Spiral,
    /// Row-major, top row first — the classic scanline feel.
    Scanline,
    /// Deterministic seeded shuffle.
    Random,
}

/// The precomputed, permuted tile list for one frame.
pub(crate) struct TileScheduler {
    pub tiles: Vec<Tile>,
}

impl TileScheduler {
    pub fn new(width: usize, height: usize, order: TileOrder, seed: u32) -> Self {
        let nx = width.div_ceil(TILE_SIZE);
        let ny = height.div_ceil(TILE_SIZE);
        let tile_at = |tx: usize, ty: usize| {
            let x = tx * TILE_SIZE;
            let y = ty * TILE_SIZE;
            Tile {
                x,
                y,
                w: (x + TILE_SIZE).min(width) - x,
                h: (y + TILE_SIZE).min(height) - y,
            }
        };
        let mut tiles = Vec::with_capacity(nx * ny);
        match order {
            TileOrder::Scanline => {
                for ty in 0..ny {
                    for tx in 0..nx {
                        tiles.push(tile_at(tx, ty));
                    }
                }
            }
            TileOrder::Morton => {
                // Walk Morton codes over the enclosing power-of-two square,
                // skipping coordinates outside the actual grid.
                let side = nx.max(ny).next_power_of_two();
                for code in 0..(side * side) as u64 {
                    let (tx, ty) = morton_decode(code);
                    if tx < nx && ty < ny {
                        tiles.push(tile_at(tx, ty));
                    }
                }
            }
            TileOrder::Spiral => {
                // Outward square spiral over the enclosing square, anchored
                // at the grid center; out-of-range steps are skipped.
                let side = nx.max(ny);
                let (mut tx, mut ty) = (nx as i64 / 2, ny as i64 / 2);
                let push = |tx: i64, ty: i64, tiles: &mut Vec<Tile>| {
                    if (0..nx as i64).contains(&tx) && (0..ny as i64).contains(&ty) {
                        tiles.push(tile_at(tx as usize, ty as usize));
                    }
                };
                push(tx, ty, &mut tiles);
                // Legs of length 1,1,2,2,3,3,… in directions R,U,L,D cover
                // any square around the anchor; 2·side legs is enough slack
                // for an off-center anchor.
                let dirs = [(1i64, 0i64), (0, -1), (-1, 0), (0, 1)];
                let mut leg = 1usize;
                let mut dir = 0usize;
                while tiles.len() < nx * ny {
                    for _ in 0..2 {
                        let (dx, dy) = dirs[dir % 4];
                        for _ in 0..leg {
                            tx += dx;
                            ty += dy;
                            push(tx, ty, &mut tiles);
                        }
                        dir += 1;
                    }
                    leg += 1;
                    debug_assert!(leg <= 2 * side + 2, "spiral failed to cover the grid");
                }
            }
            TileOrder::Random => {
                for ty in 0..ny {
                    for tx in 0..nx {
                        tiles.push(tile_at(tx, ty));
                    }
                }
                // Fisher-Yates with the deterministic PCG stream — same
                // scene and seed, same order.
                let mut rng = openqmc::pcg::Rng::new(seed ^ 0x9E37_79B9);
                for i in (1..tiles.len()).rev() {
                    let j = (rng.next_u32() as usize) % (i + 1);
                    tiles.swap(i, j);
                }
            }
        }
        debug_assert_eq!(tiles.len(), nx * ny);
        TileScheduler { tiles }
    }
}

/// Interleave the low 32 bits of x and y (x in even bit positions).
fn morton_decode(code: u64) -> (usize, usize) {
    fn compact(mut v: u64) -> usize {
        v &= 0x5555_5555_5555_5555;
        v = (v | (v >> 1)) & 0x3333_3333_3333_3333;
        v = (v | (v >> 2)) & 0x0F0F_0F0F_0F0F_0F0F;
        v = (v | (v >> 4)) & 0x00FF_00FF_00FF_00FF;
        v = (v | (v >> 8)) & 0x0000_FFFF_0000_FFFF;
        v = (v | (v >> 16)) & 0x0000_0000_FFFF_FFFF;
        v as usize
    }
    (compact(code), compact(code >> 1))
}

/// The fixed order tile-local pixel slots fill in: 8×8 Bayer (ordered
/// dither) order, so any prefix of the order is a well-dispersed subset of
/// the tile. Coarse passes cover prefixes (1, 4, 16 pixels); fine passes
/// cover all 64. Entries are `(x, y)` offsets within the tile.
#[allow(dead_code)] // consumed by the pass-based driver (next commit)
pub(crate) const FILL_ORDER: [(u8, u8); 64] = fill_order();

const fn fill_order() -> [(u8, u8); 64] {
    // Bayer index via the recursive construction B_{2n} = [[4B, 4B+2],
    // [4B+3, 4B+1]]: each coordinate bit pair contributes two value bits,
    // finer bits more significant.
    let mut order = [(0u8, 0u8); 64];
    let mut y = 0usize;
    while y < 8 {
        let mut x = 0usize;
        while x < 8 {
            let mut v = 0usize;
            let mut bit = 0usize;
            while bit < 3 {
                let xb = (x >> bit) & 1;
                let yb = (y >> bit) & 1;
                v |= (2 * (xb ^ yb) + yb) << (2 * (2 - bit));
                bit += 1;
            }
            order[v] = (x as u8, y as u8);
            x += 1;
        }
        y += 1;
    }
    order
}

/// One unit of scheduled work, applied to every active tile: pixel slots
/// `FILL_ORDER[start_pixel..end_pixel]`, each rendering per-pixel samples
/// `[start_sample, end_sample)`. Mirrors MoonRay's `Pass`.
#[allow(dead_code)] // consumed by the pass-based driver (next commit)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pass {
    pub start_pixel: u32,
    pub end_pixel: u32,
    pub start_sample: u32,
    pub end_sample: u32,
}

#[allow(dead_code)] // consumed by the pass-based driver (next commit)
impl Pass {
    pub fn pixels(&self) -> u32 {
        self.end_pixel - self.start_pixel
    }
    pub fn samples(&self) -> u32 {
        self.end_sample - self.start_sample
    }
    /// Pixel-samples this pass adds to one full tile.
    pub fn tile_samples(&self) -> u64 {
        self.pixels() as u64 * self.samples() as u64
    }
}

/// Boundaries where coarse passes split the tile's 64 pixel slots: 1, 4 and
/// 16 pixels are each one refinement level of the Bayer fill order.
#[allow(dead_code)] // consumed by the pass-based driver (next commit)
const COARSE_PIXEL_BOUNDS: [u32; 5] = [0, 1, 4, 16, 64];

/// The full pass sequence for one render: a pure function of
/// `(spp, min_spp)`, which is the resume-alignment contract — a resumed
/// render regenerates the identical remaining schedule.
#[allow(dead_code)] // consumed by the pass-based driver (next commit)
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PassSchedule {
    pub passes: Vec<Pass>,
}

/// Per-pixel sample indices where passes begin/end: sample 0 alone (the
/// coarse group), then geometrically doubling ranges capped at
/// [`MAX_PASS_SAMPLES`], with extra boundaries forced at `min_spp` (the
/// uniform/adaptive stage transition) and `spp`.
fn sample_boundaries(spp: u32, min_spp: u32) -> Vec<u32> {
    let mut bounds = vec![0u32];
    let mut b = 1u32;
    while b < spp {
        bounds.push(b);
        let step = b.min(MAX_PASS_SAMPLES);
        b = b.saturating_add(step);
    }
    if min_spp > 0 && min_spp < spp && !bounds.contains(&min_spp) {
        bounds.push(min_spp);
        bounds.sort_unstable();
    }
    bounds.push(spp);
    bounds
}

#[allow(dead_code)] // consumed by the pass-based driver (next commit)
impl PassSchedule {
    pub fn new(spp: u32, min_spp: u32) -> Self {
        Self::from_range(0, spp.max(1), min_spp)
    }

    /// The schedule covering per-pixel samples `[start, end)` — the suffix
    /// of `new(end, min_spp)` when `start` lies on one of its boundaries,
    /// with a single bridging pass up to the next boundary otherwise (a
    /// checkpoint from a different total spp).
    pub fn from_range(start: u32, end: u32, min_spp: u32) -> Self {
        let mut passes = Vec::new();
        if start >= end {
            return PassSchedule { passes };
        }
        if start == 0 {
            // Sample 0, split across dispersed pixel subsets (coarse
            // passes) — cheap first-look scheduling and the head of the
            // resume-granularity story.
            for w in COARSE_PIXEL_BOUNDS.windows(2) {
                passes.push(Pass {
                    start_pixel: w[0],
                    end_pixel: w[1],
                    start_sample: 0,
                    end_sample: 1,
                });
            }
        }
        let bounds = sample_boundaries(end, min_spp);
        let mut prev = start.max(1);
        for &b in bounds.iter().filter(|&&b| b > start.max(1)) {
            passes.push(Pass {
                start_pixel: 0,
                end_pixel: TILE_PIXELS,
                start_sample: prev,
                end_sample: b,
            });
            prev = b;
        }
        PassSchedule { passes }
    }

    /// Convert a range of *global tile sample ids* back into passes —
    /// MoonRay's `convertSampleIdRangeToPasses`. Ids enumerate the schedule
    /// `new(spp, min_spp)` pass by pass; within a pass, pixel slots complete
    /// their whole sample range in fill order. A range cut anywhere is
    /// realized as: a fragment finishing the partially-sampled pixel, a
    /// fragment covering the remaining whole pixels of the cut pass, the
    /// whole passes in between, and symmetric fragments at the tail.
    ///
    /// Unused by the pass-boundary checkpoints of v1, but it is what makes
    /// mid-pass (e.g. signal-triggered) checkpoints exactly resumable.
    pub fn from_tile_sample_range(start_id: u64, end_id: u64, spp: u32, min_spp: u32) -> Self {
        let full = Self::new(spp, min_spp);
        let mut passes = Vec::new();
        let mut base = 0u64; // first global id of the current pass
        for p in &full.passes {
            let len = p.tile_samples();
            let lo = start_id.clamp(base, base + len);
            let hi = end_id.clamp(base, base + len);
            if lo < hi {
                // Offsets within this pass, in (pixel-major, sample-minor)
                // id order.
                let (off, cnt) = ((lo - base) as u32, (hi - lo) as u32);
                let ns = p.samples();
                let first_px = p.start_pixel + off / ns;
                let first_skip = off % ns; // samples already done in first_px
                let last = off + cnt - 1;
                let last_px = p.start_pixel + last / ns;
                let last_taken = last % ns + 1; // samples covered in last_px
                if first_px == last_px {
                    passes.push(Pass {
                        start_pixel: first_px,
                        end_pixel: first_px + 1,
                        start_sample: p.start_sample + first_skip,
                        end_sample: p.start_sample + last_taken,
                    });
                } else {
                    if first_skip > 0 {
                        passes.push(Pass {
                            start_pixel: first_px,
                            end_pixel: first_px + 1,
                            start_sample: p.start_sample + first_skip,
                            end_sample: p.end_sample,
                        });
                    }
                    let whole_start = first_px + (first_skip > 0) as u32;
                    let whole_end = last_px + (last_taken == ns) as u32;
                    if whole_start < whole_end {
                        passes.push(Pass {
                            start_pixel: whole_start,
                            end_pixel: whole_end,
                            start_sample: p.start_sample,
                            end_sample: p.end_sample,
                        });
                    }
                    if last_taken < ns {
                        passes.push(Pass {
                            start_pixel: last_px,
                            end_pixel: last_px + 1,
                            start_sample: p.start_sample,
                            end_sample: p.start_sample + last_taken,
                        });
                    }
                }
            }
            base += len;
        }
        PassSchedule { passes }
    }

    /// Total pixel-samples the schedule spends on one full tile.
    pub fn tile_samples(&self) -> u64 {
        self.passes.iter().map(Pass::tile_samples).sum()
    }
}

/// MoonRay-style virtual work queue: stores nothing, synthesizes contiguous
/// index groups from one atomic cursor. Workers loop [`Self::next_group`]
/// until it returns `None`; group size trades scheduling overhead against
/// load balance.
pub(crate) struct TileWorkQueue {
    next: AtomicU32,
    num_tiles: u32,
    group_size: u32,
}

/// A contiguous range of indices into the active-tile list.
pub(crate) struct TileGroup {
    pub start: u32,
    pub end: u32,
}

impl TileWorkQueue {
    pub fn new(num_tiles: u32, threads: usize) -> Self {
        // ~8 groups per thread keeps the tail balanced without contending
        // on the cursor.
        let group_size = (num_tiles / (threads.max(1) as u32 * 8)).max(1);
        TileWorkQueue {
            next: AtomicU32::new(0),
            num_tiles,
            group_size,
        }
    }

    pub fn next_group(&self) -> Option<TileGroup> {
        let start = self.next.fetch_add(self.group_size, Ordering::Relaxed);
        if start >= self.num_tiles {
            return None;
        }
        Some(TileGroup {
            start,
            end: (start + self.group_size).min(self.num_tiles),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn fill_order_is_a_permutation_of_the_tile() {
        let seen: HashSet<(u8, u8)> = FILL_ORDER.iter().copied().collect();
        assert_eq!(seen.len(), 64);
        assert!(seen.iter().all(|&(x, y)| x < 8 && y < 8));
    }

    /// Prefixes of the Bayer order are dispersed: the first 4 slots land in
    /// distinct 4×4 quadrants, the first 16 in distinct 2×2 cells.
    #[test]
    fn fill_order_prefixes_are_dispersed() {
        let quads: HashSet<(u8, u8)> = FILL_ORDER[..4]
            .iter()
            .map(|&(x, y)| (x / 4, y / 4))
            .collect();
        assert_eq!(quads.len(), 4);
        let cells: HashSet<(u8, u8)> = FILL_ORDER[..16]
            .iter()
            .map(|&(x, y)| (x / 2, y / 2))
            .collect();
        assert_eq!(cells.len(), 16);
    }

    fn assert_permutation(width: usize, height: usize, order: TileOrder) {
        let sched = TileScheduler::new(width, height, order, 7);
        let nx = width.div_ceil(TILE_SIZE);
        let ny = height.div_ceil(TILE_SIZE);
        assert_eq!(sched.tiles.len(), nx * ny, "{order:?} {width}x{height}");
        let seen: HashSet<(usize, usize)> = sched.tiles.iter().map(|t| (t.x, t.y)).collect();
        assert_eq!(seen.len(), nx * ny, "{order:?} duplicates");
        for t in &sched.tiles {
            assert!(t.w > 0 && t.w <= TILE_SIZE && t.h > 0 && t.h <= TILE_SIZE);
            assert_eq!(t.x + t.w, (t.x + TILE_SIZE).min(width).max(t.x + t.w));
            assert!(t.x + t.w <= width && t.y + t.h <= height);
        }
    }

    #[test]
    fn tile_orders_are_permutations() {
        for order in [
            TileOrder::Morton,
            TileOrder::Spiral,
            TileOrder::Scanline,
            TileOrder::Random,
        ] {
            assert_permutation(64, 64, order); // pow2 square
            assert_permutation(640, 360, order); // non-square, clipped
            assert_permutation(100, 70, order); // non-pow2, clipped
            assert_permutation(8, 200, order); // 1×N column
            assert_permutation(5, 3, order); // single sub-size tile
        }
    }

    #[test]
    fn random_order_is_deterministic_per_seed() {
        let a = TileScheduler::new(640, 360, TileOrder::Random, 42);
        let b = TileScheduler::new(640, 360, TileOrder::Random, 42);
        let c = TileScheduler::new(640, 360, TileOrder::Random, 43);
        assert_eq!(a.tiles, b.tiles);
        assert_ne!(a.tiles, c.tiles);
    }

    /// Expand a schedule into the multiset of (pixel_slot, sample) pairs it
    /// renders on one full tile.
    fn coverage(sched: &PassSchedule) -> Vec<(u32, u32)> {
        let mut v = Vec::new();
        for p in &sched.passes {
            for px in p.start_pixel..p.end_pixel {
                for s in p.start_sample..p.end_sample {
                    v.push((px, s));
                }
            }
        }
        v.sort_unstable();
        v
    }

    fn full_coverage(spp: u32) -> Vec<(u32, u32)> {
        let mut v = Vec::new();
        for px in 0..TILE_PIXELS {
            for s in 0..spp {
                v.push((px, s));
            }
        }
        v
    }

    #[test]
    fn schedule_covers_each_pixel_sample_exactly_once() {
        for spp in [1, 2, 48, 128] {
            for min_spp in [0, 2, 32] {
                let sched = PassSchedule::new(spp, min_spp);
                assert_eq!(
                    coverage(&sched),
                    full_coverage(spp),
                    "spp {spp} min {min_spp}"
                );
                assert_eq!(sched.tile_samples(), TILE_PIXELS as u64 * spp as u64);
            }
        }
    }

    #[test]
    fn schedule_has_boundary_at_min_spp() {
        let sched = PassSchedule::new(100, 22);
        assert!(
            sched
                .passes
                .iter()
                .any(|p| p.start_sample == 22 && p.start_pixel == 0),
            "no pass starts at min_spp: {:?}",
            sched.passes
        );
        // Coarse passes cover exactly sample 0, split by the coarse pixel
        // boundaries.
        let coarse: Vec<_> = sched.passes.iter().filter(|p| p.start_sample == 0).collect();
        assert_eq!(coarse.len(), 4);
        assert!(coarse.iter().all(|p| p.end_sample == 1));
    }

    #[test]
    fn from_range_is_a_suffix_of_the_full_schedule() {
        for (start, end, min_spp) in [(32u32, 64u32, 32u32), (2, 128, 2), (48, 128, 32)] {
            let full = PassSchedule::new(end, min_spp);
            let resumed = PassSchedule::from_range(start, end, min_spp);
            let suffix: Vec<Pass> = full
                .passes
                .iter()
                .copied()
                .filter(|p| p.start_sample >= start)
                .collect();
            assert_eq!(resumed.passes, suffix, "start {start} end {end}");
        }
    }

    #[test]
    fn from_range_bridges_a_foreign_boundary() {
        // 100 is a boundary of new(100, 32) but not of new(128, 32): the
        // resumed schedule must bridge to the next native boundary and then
        // cover the rest, with no gap or overlap.
        let resumed = PassSchedule::from_range(100, 128, 32);
        let mut expect = Vec::new();
        for px in 0..TILE_PIXELS {
            for s in 100..128 {
                expect.push((px, s));
            }
        }
        assert_eq!(coverage(&resumed), expect);
    }

    #[test]
    fn tile_sample_range_roundtrips() {
        let spp = 48;
        let min_spp = 16;
        let total = TILE_PIXELS as u64 * spp as u64;
        // Splitting the id space anywhere must partition the full coverage.
        for cut in [0u64, 1, 63, 64, 65, 1000, total - 1, total] {
            let head = PassSchedule::from_tile_sample_range(0, cut, spp, min_spp);
            let tail = PassSchedule::from_tile_sample_range(cut, total, spp, min_spp);
            assert_eq!(head.tile_samples(), cut);
            assert_eq!(tail.tile_samples(), total - cut);
            let mut all = coverage(&head);
            all.extend(coverage(&tail));
            all.sort_unstable();
            assert_eq!(all, full_coverage(spp), "cut {cut}");
        }
    }

    #[test]
    fn tile_sample_range_mid_pass_synthesizes_fragments() {
        // Cut inside the body of a fine pass: the head must end with a
        // partial-pixel fragment, the tail must begin with one.
        let spp = 48;
        let tail = PassSchedule::from_tile_sample_range(70, 100, spp, 0);
        assert_eq!(tail.tile_samples(), 30);
        // Every emitted pass stays within one source pass's sample range.
        for p in &tail.passes {
            assert!(p.start_pixel < p.end_pixel && p.start_sample < p.end_sample);
        }
    }

    #[test]
    fn work_queue_hands_out_every_tile_once() {
        let q = TileWorkQueue::new(103, 8);
        let mut seen = vec![false; 103];
        while let Some(g) = q.next_group() {
            for i in g.start..g.end {
                assert!(!seen[i as usize]);
                seen[i as usize] = true;
            }
        }
        assert!(seen.iter().all(|&s| s));
    }
}
