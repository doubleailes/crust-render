//! Render statistics and per-phase profiling.
//!
//! A render is not one cost but several very different ones — parsing the
//! USD stage, decoding assets, building the acceleration structure, tracing
//! paths, encoding the image — and a single wall-clock number hides which
//! of them actually dominated. This module separates them, in the spirit of
//! Guerilla Render's "Profiling And Statistics": phases are recorded as a
//! tree and then reported two ways, **by execution tree** (the order and
//! nesting the engine actually ran them in) and **by time** (largest
//! first), alongside a **statistics** block counting what the committed
//! scene holds.
//!
//! Collection is deliberately cheap and coarse: one `Instant` per phase,
//! never per ray, plus integer counters ([`RayStats`]) the integrator bumps
//! in registers it already holds. So `--stats` costs the same whether or
//! not the report is printed, and its Render phase stays comparable between
//! runs — which `bench_ab.sh` relies on.
//!
//! Timing *inside* the render — Guerilla's "Render Profile" — cannot be that
//! cheap, so it lives in [`crate::profile`] behind `--profile`, and is only
//! printed here (after the phases, since it zooms into one of them).

use std::fmt;
use std::time::Duration;

/// One timed phase. `depth` gives the nesting used by the execution-tree
/// view: a phase at depth 1 is a sub-phase of the nearest preceding
/// depth-0 phase, and its time is *included* in that parent's.
///
/// The two memory figures are sampled when the phase ends. `rss_end` is
/// what is still resident; `peak_end` is the high-water mark reached at
/// any point up to then. A large gap between them says the phase
/// allocated far more than it kept — transient build churn, which costs
/// page faults and time even though the final structures are small.
#[derive(Clone, Debug)]
pub struct Phase {
    pub name: String,
    pub depth: u8,
    pub duration: Duration,
    pub rss_end: Option<u64>,
    pub peak_end: Option<u64>,
}

/// Resident and peak memory at one instant. Capture at a phase boundary
/// with [`MemorySample::now`] and hand to [`RenderStats::record_at`].
#[derive(Clone, Copy, Debug, Default)]
pub struct MemorySample {
    pub rss: Option<u64>,
    pub peak: Option<u64>,
}

impl MemorySample {
    /// Both figures from **one** read of `/proc/self/status`. The kernel
    /// only guarantees `VmHWM >= VmRSS` within a single read (it reports
    /// `max(hiwater_rss, current rss)`); the high-water mark is updated
    /// lazily and RSS is batched per thread, so two separate reads can
    /// return an RSS above the peak while other threads allocate. `None`
    /// for both anywhere procfs is not available.
    pub fn now() -> Self {
        #[cfg(target_os = "linux")]
        {
            let Some(status) = read_proc_status() else {
                return MemorySample::default();
            };
            MemorySample {
                rss: parse_proc_status_bytes(&status, "VmRSS:"),
                peak: parse_proc_status_bytes(&status, "VmHWM:"),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            MemorySample::default()
        }
    }
}

/// Primitives split by kind. Mirrors [`crate::rt::PrimitiveBreakdown`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrimitiveCounts {
    pub triangles: usize,
    pub spheres: usize,
    /// Analytic disks (UsdLux `DiskLight` surfaces).
    pub disks: usize,
    /// Analytic open cylinders (UsdLux `CylinderLight` surfaces).
    pub cylinders: usize,
    /// Straight (linear / pre-flattened) round curve segments.
    pub curve_segments: usize,
    /// Analytically intersected cubic curve spans.
    pub cubic_curve_spans: usize,
    pub instances: usize,
}

impl PrimitiveCounts {
    pub fn total(&self) -> usize {
        self.triangles
            + self.spheres
            + self.disks
            + self.cylinders
            + self.curve_segments
            + self.cubic_curve_spans
            + self.instances
    }

    fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

impl From<crate::rt::PrimitiveBreakdown> for PrimitiveCounts {
    fn from(b: crate::rt::PrimitiveBreakdown) -> Self {
        PrimitiveCounts {
            triangles: b.triangles,
            spheres: b.spheres,
            disks: b.disks,
            cylinders: b.cylinders,
            curve_segments: b.curve_segments,
            cubic_curve_spans: b.cubic_curve_spans,
            instances: b.instances,
        }
    }
}

/// What the committed scene actually holds.
///
/// Two primitive views, because for an instanced scene they answer
/// different questions. `top_level` is what the root BVH traverses — an
/// instance is one primitive there, however much geometry it references.
/// `unique` descends into instances and counts each distinct prototype
/// once, so it is what actually occupies memory. The gap between them is
/// the benefit instancing is buying.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneCounters {
    /// Attached geometries (`geom_id`s), i.e. entries in the material table.
    pub geometries: usize,
    pub top_level: PrimitiveCounts,
    pub unique: PrimitiveCounts,
    pub lights: usize,
    pub volumes: usize,
    /// Exact kernel-resident bytes, by structure. Everything outside
    /// `crust-rt` — materials, the USD stage, import caches — is *not*
    /// counted, so this being well under peak RSS is expected and the gap
    /// is itself informative.
    pub footprint: crate::rt::MemoryFootprint,
}

/// Work the integrator did, in rays and path vertices.
///
/// Counters, not timers: a `Instant::now()` pair costs tens of nanoseconds
/// against a few hundred for a ray query, so timing individual rays would
/// both slow the render and distort what it measured. Counting is a
/// register increment, and dividing the totals by the render phase's
/// wall-clock gives throughput without either problem.
///
/// Accumulated per work unit (a tile or a scanline) and summed when the
/// pass collects its results, so no two threads ever touch the same
/// counter and there is nothing to contend on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RayStats {
    /// Primary rays cast from the camera.
    pub camera_rays: u64,
    /// Closest-hit queries: camera and bounce rays both.
    pub closest_hit: u64,
    /// Occlusion queries — the shadow rays next-event estimation casts.
    pub shadow_rays: u64,
    /// Vertices shaded, over surfaces and volume scatters alike.
    pub vertices: u64,
    /// Russian-roulette decisions reached, and how many ended the path.
    /// Their ratio says whether roulette is doing anything worth tuning.
    pub rr_tested: u64,
    pub rr_killed: u64,
    /// Paths that ended by leaving the scene.
    pub ended_escaped: u64,
    /// Paths that ended by exhausting `max_depth` — if this is ~0, the
    /// depth ceiling is not what a render is paying for.
    pub ended_depth: u64,
    /// Paths that ended because the material sampled no continuation — an
    /// absorbed or below-the-horizon sample. Roulette kills are `rr_killed`.
    pub ended_absorbed: u64,
    /// Of `vertices`: scatter events inside a volume region, and inside a
    /// carried medium (subsurface / glass interiors). The rest are surfaces.
    pub volume_scatters: u64,
    pub medium_scatters: u64,
    /// Subsurface random walks started, and how many found an exit (the rest
    /// were absorbed or left no way out). Their steps are the walks' free
    /// flights, and their rays the closest-hit queries those issued —
    /// counted apart from `closest_hit` so `bounce_rays` still means bounces,
    /// and folded into `total_rays`.
    pub sss_walks: u64,
    pub sss_exits: u64,
    pub sss_steps: u64,
    pub sss_rays: u64,
    /// Cutouts (`Material::opacity`): the hits a path passed straight
    /// through, and the closest-hit queries cutouts cost beyond the ones the
    /// integrator makes anyway — the query past each hit passed through, and
    /// every query of a shadow ray the any-hit test found blocked in a world
    /// with cutouts. Counted apart from `closest_hit`, like `sss_rays`, and
    /// folded into `total_rays`.
    pub cutout_passes: u64,
    pub cutout_rays: u64,
    /// Light samples drawn by next-event estimation that reached a light
    /// (`sample_li` answered). A shadow ray follows only when the connection
    /// can carry something, so `light_samples - shadow_rays` is the NEE work
    /// the cheap tests saved.
    pub light_samples: u64,
    /// Shadow rays the occlusion query found blocked.
    pub shadow_occluded: u64,
    /// Adaptive sampling, over the pixels of adaptive passes only: pixels,
    /// samples they took, how many stopped before the full budget, and the
    /// fewest and most any pixel took.
    pub adaptive_pixels: u64,
    pub adaptive_samples: u64,
    pub early_stopped: u64,
    pub spp_min: u32,
    pub spp_max: u32,
    /// Pixels that passed their own convergence test but were held back,
    /// at least once, by a less converged cross neighbour (see
    /// `RenderSettings::with_adaptive_neighbour_tolerance`).
    pub neighbour_held: u64,
}

impl RayStats {
    /// All ray queries, of every kind.
    pub fn total_rays(&self) -> u64 {
        self.closest_hit + self.shadow_rays + self.sss_rays + self.cutout_rays
    }

    /// Mean shaded vertices per camera ray — the effective path length.
    pub fn mean_path_length(&self) -> f64 {
        if self.camera_rays == 0 {
            return 0.0;
        }
        self.vertices as f64 / self.camera_rays as f64
    }

    /// Fraction of roulette decisions that terminated the path.
    pub fn rr_kill_rate(&self) -> f64 {
        if self.rr_tested == 0 {
            return 0.0;
        }
        self.rr_killed as f64 / self.rr_tested as f64
    }

    /// Shaded surface vertices — `vertices` less the volume and medium
    /// scatters.
    pub fn surface_vertices(&self) -> u64 {
        self.vertices - self.volume_scatters - self.medium_scatters
    }

    /// Closest-hit queries that were not camera rays: bounces.
    pub fn bounce_rays(&self) -> u64 {
        self.closest_hit.saturating_sub(self.camera_rays)
    }

    /// Guerilla's "shadow rays / shading point": usually well under 5, and a
    /// much larger figure means NEE is doing more per vertex than intended.
    pub fn shadow_rays_per_vertex(&self) -> f64 {
        match self.vertices {
            0 => 0.0,
            n => self.shadow_rays as f64 / n as f64,
        }
    }

    /// Sums another unit's counters into this one.
    pub fn merge(&mut self, o: &RayStats) {
        // Min over pixels actually counted: a unit with no adaptive pixel has
        // `spp_min == 0`, which is not a pixel that took zero samples.
        if o.adaptive_pixels > 0 {
            self.spp_min = if self.adaptive_pixels == 0 {
                o.spp_min
            } else {
                self.spp_min.min(o.spp_min)
            };
        }
        self.spp_max = self.spp_max.max(o.spp_max);
        self.adaptive_pixels += o.adaptive_pixels;
        self.adaptive_samples += o.adaptive_samples;
        self.early_stopped += o.early_stopped;
        self.neighbour_held += o.neighbour_held;
        self.ended_absorbed += o.ended_absorbed;
        self.volume_scatters += o.volume_scatters;
        self.medium_scatters += o.medium_scatters;
        self.sss_walks += o.sss_walks;
        self.sss_exits += o.sss_exits;
        self.sss_steps += o.sss_steps;
        self.sss_rays += o.sss_rays;
        self.cutout_passes += o.cutout_passes;
        self.cutout_rays += o.cutout_rays;
        self.light_samples += o.light_samples;
        self.shadow_occluded += o.shadow_occluded;
        self.camera_rays += o.camera_rays;
        self.closest_hit += o.closest_hit;
        self.shadow_rays += o.shadow_rays;
        self.vertices += o.vertices;
        self.rr_tested += o.rr_tested;
        self.rr_killed += o.rr_killed;
        self.ended_escaped += o.ended_escaped;
        self.ended_depth += o.ended_depth;
    }

    fn is_empty(&self) -> bool {
        *self == RayStats::default()
    }
}

/// Image-level parameters worth reporting next to the costs they drove.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageCounters {
    pub width: usize,
    pub height: usize,
    pub samples_per_pixel: u32,
    pub max_depth: u32,
}

impl From<&crate::tracer::RenderSettings> for ImageCounters {
    fn from(s: &crate::tracer::RenderSettings) -> Self {
        let (width, height) = s.get_dimensions();
        ImageCounters {
            width,
            height,
            samples_per_pixel: s.samples_per_pixel(),
            max_depth: s.max_depth(),
        }
    }
}

/// Phase timings plus scene counters for one render.
///
/// Built up as the render proceeds — the importer fills in its own phases
/// and the scene counts, the host adds render and output timings — then
/// formatted with [`RenderStats::report`].
#[derive(Clone, Debug, Default)]
pub struct RenderStats {
    pub phases: Vec<Phase>,
    pub scene: SceneCounters,
    pub image: ImageCounters,
    /// Integrator work; empty unless the host asked the renderer for it.
    pub rays: RayStats,
    /// Streaming texture cache; empty unless the host streams textures and
    /// pushed its counters in.
    ///
    /// Pushed rather than collected, because the cache lives in the *host*
    /// (`crust-assets`) and the dependency runs that way — crust-core cannot
    /// reach into it. `main.rs` snapshots it after the render exactly as it
    /// already assigns `stats.rays`.
    pub textures: TextureCacheStats,
    /// Ptex residency; empty unless the scene bound a `.ptx`. Pushed by the
    /// host for the same reason `textures` is.
    pub ptex: PtexCacheStats,
    /// Distinct materials by kind ("allocated materials"), and lights by
    /// kind. Filled by the host from the committed `World` / `LightList`
    /// (see [`RenderStats::inventory_from`]), so the procedural fallback
    /// reports them as well as a USD stage.
    pub materials: Vec<(&'static str, usize)>,
    pub light_kinds: Vec<(&'static str, usize)>,
    /// The per-section render profile, when `--profile` asked for one.
    pub profile: Option<crate::profile::RenderProfile>,
    /// Adaptive subdivision's choices; empty (and not reported) in uniform
    /// subdivision.
    pub subdivision: SubdivisionCounters,
    /// Scalar displacement applied at import; all zero (and not reported)
    /// for a scene without displacement.
    pub displacement: DisplacementCounters,
}

/// What the displacement pass did over a load.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplacementCounters {
    /// Distinct meshes displaced — once per distinct mesh, however many prims
    /// share it.
    pub meshes: u64,
    /// Unique vertices displaced across those meshes.
    pub vertices: u64,
    /// Time spent displacing (charts, offsets, normals), within "Traverse
    /// prims".
    pub time: Duration,
    /// The largest `|offset|` applied, in local units.
    pub max_offset: f32,
    /// Displaced meshes left at their authored cage resolution — the
    /// displacement then moves only cage vertices.
    pub at_cage: u64,
    /// Displaced meshes with no known bound, whose adaptive frustum test was
    /// turned off so displacement cannot push under-diced geometry into view.
    pub frustum_skipped: u64,
}

/// What adaptive subdivision chose over a load.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubdivisionCounters {
    /// The target edge length in pixels and the level ceiling; `None` in
    /// uniform subdivision, which reports nothing here.
    pub adaptive: Option<(f32, u32)>,
    /// Subdivision meshes read, by the level each was refined to (index =
    /// level): a direct prim once per placement, a prototype's mesh once per
    /// version of the prototype.
    pub levels: Vec<u64>,
    /// Meshes of shared prototypes (placed more than once in their top-level
    /// subtree), all refined to `shared_level` whatever their distance.
    pub shared_meshes: u64,
    pub shared_level: u32,
    /// Subdivision meshes tessellated per face, and adaptive meshes that took
    /// the per-mesh level instead (a `loop` mesh, a face-varying chart, or
    /// `CRUST_ADAPTIVE_PER_FACE=0`).
    pub per_face_meshes: u64,
    pub per_face_fallbacks: u64,
    /// Cage edges and spokes of per-face meshes by segment count, binned by
    /// `ceil(log2(rate))` (index 0: rate 1, 1: 2, 2: 3–4, 3: 5–8, …).
    pub rate_bins: Vec<u64>,
}

/// What Ptex cost over a render, under whichever backend ran.
///
/// Unlike [`TextureCacheStats`] this reports for the **preloading** path too,
/// because the first question it has to answer is which backend ran at all.
/// Without that the report is silent about Ptex, and on a stage where Ptex is
/// gigabytes — the island's is 4.58 GiB at the default cap — an operator
/// cannot tell a streamed run from a preloaded one except by diffing peak RSS
/// against a run they have to do separately.
///
/// A mixed report is possible and is not a bug: streaming falls back to
/// preloading for a file it cannot open, so `streamed < textures` means some
/// file declined, which is exactly when you want to be told.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PtexCacheStats {
    /// Textures opened, by either backend.
    pub textures: u32,
    /// How many of those streamed.
    pub streamed: u32,
    /// Preloaded because they were **under the streaming size threshold** —
    /// a policy decision, not a failure. Counted apart from `open_failed`
    /// because the two mean opposite things to whoever reads the report: the
    /// first says the admission rule worked, the second says a file is broken.
    pub below_threshold: u32,
    /// Preloaded because streaming them **failed** — the fallback firing.
    pub open_failed: u32,
    /// Preloaded because the budget had no room for another reader. Also a
    /// policy decision, but a different one from `below_threshold`: these
    /// textures were big enough to want streaming and the budget could not
    /// seat them, so it is the line that says "raise CRUST_PTEX_CACHE_MB".
    pub budget_full: u32,
    /// Preloaded because streaming them would have read a mip chain the file
    /// reduced in its own colour encoding, where the preloaded pyramid is
    /// reduced in linear light. A correctness refusal, and the default
    /// policy — `CRUST_PTEX_STREAM_MIPSPACE=file` accepts the file's chain
    /// instead. Counted apart because it is the line that explains a render
    /// where `CRUST_PTEX_STREAM=1` was set and nothing streamed.
    pub mip_space: u32,
    pub faces: u64,
    /// Resident bytes held by the **preloaded** textures. Fixed for the
    /// render, and the number streaming exists to replace.
    pub preloaded_bytes: u64,
    pub micro_hits: u64,
    /// Texel fetches that reached the reader rather than this thread's slots.
    pub reader_lookups: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub evictions: u64,
    /// Bytes the streamed caches hold *now* — a live figure, bounded by
    /// `budget_bytes`, not a peak.
    pub resident_bytes: u64,
    pub budget_bytes: u64,
    /// Bytes the per-thread tile microcaches hold right now, process-wide.
    ///
    /// Reported because it is **real residency the reader's own counters
    /// cannot see**: those slots hold decoded tiles outside its cache. It is
    /// bounded by `micro_reserve_bytes`, which is taken out of
    /// `CRUST_PTEX_CACHE_MB` before the readers divide the rest — so this
    /// line and `resident_bytes` together are what the budget actually buys.
    pub micro_retained_bytes: u64,
    /// The allowance reserved for those slots. See `micro_retained_bytes`.
    pub micro_reserve_bytes: u64,
}

impl PtexCacheStats {
    pub fn lookups(&self) -> u64 {
        self.micro_hits + self.reader_lookups
    }

    /// Share of texel fetches answered without touching the reader's mutex.
    pub fn micro_rate(&self) -> f64 {
        match self.lookups() {
            0 => 0.0,
            n => self.micro_hits as f64 / n as f64,
        }
    }

    fn is_empty(&self) -> bool {
        self.textures == 0
    }
}

/// What a streaming texture cache did over a render.
///
/// Three tiers of hit are counted separately because they answer different
/// questions. Microcache hits say whether the sampler's access pattern is
/// coherent — a bilinear tap reads one tile four times, so a low number here
/// means something is wrong upstream. Shard hits say the tile was resident.
/// And `redundant` — tiles paged in more than once over one render — is the
/// only number that distinguishes "the cache is full" from "the cache is too
/// small for the working set", which is the question an operator actually has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextureCacheStats {
    pub micro_hits: u64,
    pub hits: u64,
    pub misses: u64,
    /// Tiles decoded again after being evicted — the budget is below the
    /// working set.
    pub redundant: u64,
    /// Tiles two workers decoded at the same time. Wasted, but bounded by the
    /// thread count and unrelated to the budget; kept separate from
    /// `redundant` because the remedies are different.
    pub raced: u64,
    pub evictions: u64,
    pub bytes_read: u64,
    pub peak_bytes: u64,
    pub errors: u64,
    pub budget_bytes: u64,
    /// Streamed files registered with the cache.
    pub files: u64,
    /// Tiles decoded from disk ("loaded tiles"); `evictions` is the
    /// "unloaded tiles" beside it. The same order of magnitude means the
    /// cache purged and re-read repeatedly to fit.
    pub loaded_tiles: u64,
    /// Held by the cache when the render ended ("still in cache").
    pub resident_bytes: u64,
    /// The streamed files' whole mip chains — what preloading them would
    /// have cost ("total memory").
    pub total_bytes: u64,
    /// UV textures that took the preload path instead, and their bytes.
    pub preloaded: u64,
    pub preloaded_bytes: u64,
}

impl TextureCacheStats {
    pub fn lookups(&self) -> u64 {
        self.micro_hits + self.hits + self.misses
    }

    /// Fraction of lookups answered without touching the disk.
    pub fn hit_rate(&self) -> f64 {
        match self.lookups() {
            0 => 0.0,
            n => (self.micro_hits + self.hits) as f64 / n as f64,
        }
    }

    fn is_empty(&self) -> bool {
        self.lookups() == 0 && self.preloaded == 0
    }
}

impl RenderStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a completed phase, sampling memory **now**. Correct only
    /// when called at the moment the phase ends; a caller that records
    /// several phases together must capture a [`MemorySample`] at each
    /// boundary and use [`RenderStats::record_at`] instead, or every phase
    /// will report the same figures.
    pub fn record(&mut self, name: impl Into<String>, depth: u8, duration: Duration) {
        self.record_at(name, depth, duration, MemorySample::now());
    }

    /// Records a completed phase with memory captured earlier — at the
    /// phase's actual end rather than at reporting time.
    pub fn record_at(
        &mut self,
        name: impl Into<String>,
        depth: u8,
        duration: Duration,
        mem: MemorySample,
    ) {
        self.phases.push(Phase {
            name: name.into(),
            depth,
            duration,
            rss_end: mem.rss,
            peak_end: mem.peak,
        });
    }

    /// The top-level "Render" phase, if the host recorded one.
    fn render_phase(&self) -> Option<&Phase> {
        self.phases
            .iter()
            .find(|p| p.depth == 0 && p.name == "Render")
    }

    /// Records the material and light breakdowns of what will render.
    pub fn inventory_from(&mut self, world: &crate::World, lights: &crate::LightList) {
        self.materials = world.material_breakdown();
        self.light_kinds = lights.kind_breakdown();
    }

    /// Total of the top-level phases. Sub-phases are skipped: their time is
    /// already inside a parent, so adding them would double count.
    pub fn total(&self) -> Duration {
        self.phases
            .iter()
            .filter(|p| p.depth == 0)
            .map(|p| p.duration)
            .sum()
    }

    /// The formatted report — statistics, then the profile by execution
    /// tree, then the profile by time.
    pub fn report(&self) -> String {
        self.to_string()
    }
}

/// One snapshot of `/proc/self/status`. Fields that must agree with each
/// other (`VmRSS` and `VmHWM`) are parsed from the same snapshot with
/// [`parse_proc_status_bytes`], never from two reads.
#[cfg(target_os = "linux")]
fn read_proc_status() -> Option<String> {
    std::fs::read_to_string("/proc/self/status").ok()
}

/// One `VmXxx:` field of a `/proc/self/status` snapshot, in bytes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_status_bytes(status: &str, field: &str) -> Option<u64> {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

/// Reads one `VmXxx:` field of `/proc/self/status`, in bytes.
#[cfg(target_os = "linux")]
fn proc_status_bytes(field: &str) -> Option<u64> {
    parse_proc_status_bytes(&read_proc_status()?, field)
}

/// Peak resident set size in bytes, if the platform can report it.
///
/// Peak rather than current: the interesting number for a renderer is the
/// high-water mark, which is usually reached mid-build and released before
/// the process ends. Reads `VmHWM` from procfs; `None` anywhere else.
pub fn peak_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        proc_status_bytes("VmHWM:")
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Currently resident set size in bytes. Paired with
/// [`peak_memory_bytes`], the difference exposes transient allocation:
/// memory a phase took and gave back. To compare the two, take them from
/// one [`MemorySample::now`] rather than calling both: each call reads
/// procfs afresh, and across reads the peak may lag the RSS.
pub fn current_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        proc_status_bytes("VmRSS:")
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// `1234567` → `1 234 567`, so seven-digit primitive counts stay readable.
/// Counts `kinds` by name, heaviest first and alphabetically among equals —
/// the shape of every `--stats` breakdown (lights by kind, materials by
/// kind).
pub(crate) fn breakdown(kinds: impl Iterator<Item = &'static str>) -> Vec<(&'static str, usize)> {
    let mut out: Vec<(&'static str, usize)> = Vec::new();
    for kind in kinds {
        match out.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, n)) => *n += 1,
            None => out.push((kind, 1)),
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    out
}

pub(crate) fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{v:.2} {}", UNITS[unit])
    }
}

/// `01:23.4` for anything over a minute, `1.234s` below — long renders and
/// millisecond phases both land in the same column.
pub(crate) fn human_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs >= 60.0 {
        let mins = (secs / 60.0).floor();
        let rem = secs - mins * 60.0;
        format!("{mins:02.0}:{rem:04.1}")
    } else {
        format!("{secs:7.3}s")
    }
}

impl fmt::Display for RenderStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Wide enough that the longest phase name ("Commit acceleration
        // structure", nested one level) still clears the time column.
        const WIDTH: usize = 84;
        const NAME: usize = 36;
        let rule = "-".repeat(WIDTH);
        let total = self.total();
        let total_secs = total.as_secs_f64();
        let pct = |d: Duration| {
            if total_secs > 0.0 {
                100.0 * d.as_secs_f64() / total_secs
            } else {
                0.0
            }
        };

        writeln!(f, "{rule}")?;
        writeln!(f, "Render Statistics")?;
        writeln!(f, "{rule}")?;

        let img = &self.image;
        if img.width > 0 && img.height > 0 {
            writeln!(f, "  {:<28} {}x{}", "resolution", img.width, img.height)?;
            writeln!(f, "  {:<28} {}", "samples per pixel", img.samples_per_pixel)?;
            writeln!(f, "  {:<28} {}", "max path depth", img.max_depth)?;
        }

        let s = &self.scene;
        writeln!(f, "  {:<28} {}", "geometries", thousands(s.geometries))?;

        // Per-type breakdown, skipping what the scene does not use — a
        // wall of zeroes helps nobody.
        let mut breakdown = |title: &str, c: &PrimitiveCounts| -> fmt::Result {
            writeln!(f, "  {:<28} {}", title, thousands(c.total()))?;
            for (label, count) in [
                ("triangles", c.triangles),
                ("spheres", c.spheres),
                ("disks", c.disks),
                ("cylinders", c.cylinders),
                ("curve segments", c.curve_segments),
                ("cubic curve spans", c.cubic_curve_spans),
                ("instances", c.instances),
            ] {
                if count > 0 {
                    writeln!(f, "    {:<26} {}", label, thousands(count))?;
                }
            }
            Ok(())
        };
        breakdown("top-level BVH primitives", &s.top_level)?;
        // Only worth printing when instancing actually made the two
        // differ; for a flat scene it would repeat the block verbatim.
        if !s.unique.is_empty() && s.unique != s.top_level {
            breakdown("primitives in memory", &s.unique)?;
        }

        // Distinct materials ("allocated materials"): geometries that share
        // one material are one allocation, so this is well under
        // `geometries` on any stage that binds by hierarchy.
        if !self.materials.is_empty() {
            let n: usize = self.materials.iter().map(|(_, n)| n).sum();
            writeln!(f, "  {:<28} {}", "allocated materials", thousands(n))?;
            for (kind, n) in &self.materials {
                writeln!(f, "    {:<26} {}", kind, thousands(*n))?;
            }
        }
        writeln!(f, "  {:<28} {}", "lights", thousands(s.lights))?;
        for (kind, n) in &self.light_kinds {
            writeln!(f, "    {:<26} {}", kind, thousands(*n))?;
        }
        if let Some((target, max)) = self.subdivision.adaptive {
            let sub = &self.subdivision;
            let levels: Vec<String> = sub
                .levels
                .iter()
                .enumerate()
                .filter(|&(_, &n)| n > 0)
                .map(|(level, &n)| format!("L{level} {}", thousands(n as usize)))
                .collect();
            writeln!(
                f,
                "  {:<28} {target} px, at most level {max}",
                "adaptive subdivision"
            )?;
            writeln!(
                f,
                "    {:<26} {}",
                "subdivision levels",
                if levels.is_empty() {
                    "none".to_string()
                } else {
                    levels.join(" · ")
                }
            )?;
            if sub.shared_meshes > 0 {
                writeln!(
                    f,
                    "    {:<26} {} at level {}",
                    "shared meshes",
                    thousands(sub.shared_meshes as usize),
                    sub.shared_level
                )?;
            }
            if sub.per_face_meshes + sub.per_face_fallbacks > 0 {
                writeln!(
                    f,
                    "    {:<26} {} (fallback: {})",
                    "per-face meshes",
                    thousands(sub.per_face_meshes as usize),
                    thousands(sub.per_face_fallbacks as usize)
                )?;
            }
            let bins: Vec<String> = sub
                .rate_bins
                .iter()
                .enumerate()
                .filter(|&(_, &n)| n > 0)
                .map(|(b, &n)| {
                    let label = match b {
                        0 => "1".to_string(),
                        1 => "2".to_string(),
                        _ => format!("{}-{}", (1u64 << (b - 1)) + 1, 1u64 << b),
                    };
                    format!("{label}: {}", thousands(n as usize))
                })
                .collect();
            if !bins.is_empty() {
                writeln!(f, "    {:<26} {}", "edge rates", bins.join(" · "))?;
            }
        }
        let d = &self.displacement;
        if d.meshes > 0 {
            writeln!(
                f,
                "  {:<28} {} meshes, {} vertices in {:.2?}, max |offset| {}",
                "displacement",
                thousands(d.meshes as usize),
                thousands(d.vertices as usize),
                d.time,
                d.max_offset
            )?;
            if d.at_cage > 0 {
                writeln!(
                    f,
                    "    {:<26} {}",
                    "at cage resolution",
                    thousands(d.at_cage as usize)
                )?;
            }
            if d.frustum_skipped > 0 {
                writeln!(
                    f,
                    "    {:<26} {}",
                    "frustum test skipped",
                    thousands(d.frustum_skipped as usize)
                )?;
            }
        }
        if s.volumes > 0 {
            writeln!(f, "  {:<28} {}", "volume regions", thousands(s.volumes))?;
        }

        // Exact kernel bytes, then peak RSS. The difference is everything
        // the kernel does not own — materials, the USD stage, caches — so
        // showing both says where to look next.
        let fp = &s.footprint;
        if fp.total() > 0 {
            writeln!(
                f,
                "  {:<28} {}",
                "kernel memory",
                human_bytes(fp.total() as u64)
            )?;
            for (label, bytes) in [
                ("vertices", fp.vertices),
                ("vertex normals", fp.vertex_normals),
                ("triangle records", fp.triangle_records),
                ("triangle packets (gathered)", fp.packets),
                ("triangle packets (indexed)", fp.packets_indexed),
                ("primitive nodes", fp.prim_nodes),
                ("instances", fp.instances),
                ("cubic curve spans", fp.cubic_spans),
                ("BVH nodes", fp.bvh_nodes),
                ("leaf indices", fp.indices),
                ("leaves", fp.leaves),
                ("geometry tables", fp.geometry_tables),
            ] {
                if bytes > 0 {
                    writeln!(f, "    {:<26} {}", label, human_bytes(bytes as u64))?;
                }
            }
            // How well the packets are used, and the one number a layout
            // change is judged by: kernel bytes per resident triangle.
            if fp.lanes > 0 {
                writeln!(
                    f,
                    "    {:<26} {:.1}% ({} of {})",
                    "lanes filled",
                    100.0 * fp.lanes_filled as f64 / fp.lanes as f64,
                    thousands(fp.lanes_filled),
                    thousands(fp.lanes)
                )?;
            }
            let triangles = if s.unique.is_empty() {
                s.top_level.triangles
            } else {
                s.unique.triangles
            };
            if triangles > 0 {
                writeln!(
                    f,
                    "    {:<26} {:.1}",
                    "bytes per triangle",
                    fp.total() as f64 / triangles as f64
                )?;
            }
        }
        if let Some(peak) = peak_memory_bytes() {
            writeln!(f, "  {:<28} {}", "peak memory (RSS)", human_bytes(peak))?;
        }

        // -- Ray statistics -------------------------------------------
        let r = &self.rays;
        if !r.is_empty() {
            let count = |n: u64| thousands(n as usize);
            let share = |n: u64, of: u64| 100.0 * n as f64 / of.max(1) as f64;
            writeln!(f, "{rule}")?;
            writeln!(f, "Ray Statistics")?;
            writeln!(f, "{rule}")?;
            writeln!(f, "  {:<28} {}", "primary rays", count(r.camera_rays))?;
            writeln!(f, "  {:<28} {}", "bounce rays", count(r.bounce_rays()))?;
            writeln!(
                f,
                "  {:<28} {} ({:.1}% occluded)",
                "shadow rays",
                count(r.shadow_rays),
                share(r.shadow_occluded, r.shadow_rays)
            )?;
            if r.sss_walks > 0 {
                writeln!(f, "  {:<28} {}", "subsurface walk rays", count(r.sss_rays))?;
            }
            if r.cutout_rays > 0 {
                writeln!(
                    f,
                    "  {:<28} {} ({} hits passed through)",
                    "cutout rays",
                    count(r.cutout_rays),
                    count(r.cutout_passes)
                )?;
            }
            writeln!(f, "  {:<28} {}", "total ray queries", count(r.total_rays()))?;
            // Throughput needs the render phase alone, not the whole run:
            // dividing by total would credit rays to time spent parsing.
            if let Some(render) = self.render_phase() {
                let secs = render.duration.as_secs_f64();
                if secs > 0.0 {
                    // Scale the unit: a pathological scene can sit near a
                    // few thousand rays a second, and "0.00 Mray/s" hides
                    // exactly the number worth looking at.
                    let rps = r.total_rays() as f64 / secs;
                    let (v, unit) = if rps >= 1e6 {
                        (rps / 1e6, "Mray/s")
                    } else if rps >= 1e3 {
                        (rps / 1e3, "Kray/s")
                    } else {
                        (rps, "ray/s")
                    };
                    writeln!(f, "  {:<28} {:.2} {}", "throughput", v, unit)?;
                    writeln!(
                        f,
                        "  {:<28} {:.2} us",
                        "mean time per ray query",
                        1e6 * secs / r.total_rays().max(1) as f64
                    )?;
                }
            }

            writeln!(f, "  {:<28} {}", "shading points", count(r.vertices))?;
            for (label, n) in [
                ("surfaces", r.surface_vertices()),
                ("volume scatters", r.volume_scatters),
                ("medium scatters", r.medium_scatters),
            ] {
                if n > 0 {
                    writeln!(
                        f,
                        "    {:<26} {} ({:.1}%)",
                        label,
                        count(n),
                        share(n, r.vertices)
                    )?;
                }
            }
            if r.sss_walks > 0 {
                writeln!(
                    f,
                    "  {:<28} {} ({:.1}% exited, {:.1} steps each)",
                    "subsurface walks",
                    count(r.sss_walks),
                    share(r.sss_exits, r.sss_walks),
                    r.sss_steps as f64 / r.sss_walks as f64
                )?;
            }
            // Guerilla's "shadow rays / shading point": normally well under
            // 5, and crust casts at most one per vertex, so this is also the
            // fraction of vertices whose light sample survived the cheap
            // tests (radiance, BSDF) and was worth a shadow ray.
            writeln!(
                f,
                "  {:<28} {:.3}",
                "shadow rays / shading point",
                r.shadow_rays_per_vertex()
            )?;
            writeln!(
                f,
                "  {:<28} {} ({:.1}% cast a shadow ray)",
                "light samples",
                count(r.light_samples),
                share(r.shadow_rays, r.light_samples)
            )?;
            writeln!(
                f,
                "  {:<28} {:.2}",
                "mean path length",
                r.mean_path_length()
            )?;

            let pixels = (self.image.width * self.image.height) as u64;
            if pixels > 0 {
                // Over every pass, training included — the samples the
                // render actually paid for, not the budget it was given.
                writeln!(
                    f,
                    "  {:<28} {:.2}",
                    "average samples / pixel",
                    r.camera_rays as f64 / pixels as f64
                )?;
            }
            if r.adaptive_pixels > 0 {
                writeln!(
                    f,
                    "  {:<28} {} of {} pixels ({:.1}%)",
                    "adaptive: stopped early",
                    count(r.early_stopped),
                    count(r.adaptive_pixels),
                    share(r.early_stopped, r.adaptive_pixels)
                )?;
                writeln!(
                    f,
                    "  {:<28} min {} / mean {:.1} / max {}",
                    "adaptive: samples / pixel",
                    r.spp_min,
                    r.adaptive_samples as f64 / r.adaptive_pixels as f64,
                    r.spp_max
                )?;
                writeln!(
                    f,
                    "  {:<28} {} of {} pixels ({:.1}%)",
                    "adaptive: held by neighbour",
                    count(r.neighbour_held),
                    count(r.adaptive_pixels),
                    share(r.neighbour_held, r.adaptive_pixels)
                )?;
            }
            writeln!(
                f,
                "  {:<28} {} of {} ({:.1}%)",
                "roulette kills",
                count(r.rr_killed),
                count(r.rr_tested),
                100.0 * r.rr_kill_rate()
            )?;
            // Every path ends exactly one way, so these four sum to the
            // primary rays — a quick check that the counters are honest.
            let ended = r.ended_escaped + r.ended_absorbed + r.rr_killed + r.ended_depth;
            writeln!(f, "  {:<28} {}", "paths ended", count(ended))?;
            for (label, n) in [
                ("escaped", r.ended_escaped),
                ("absorbed", r.ended_absorbed),
                ("roulette", r.rr_killed),
                ("depth cap", r.ended_depth),
            ] {
                writeln!(
                    f,
                    "    {:<26} {} ({:.1}%)",
                    label,
                    count(n),
                    share(n, ended)
                )?;
            }
        }

        // -- Textures --------------------------------------------------
        let t = &self.textures;
        if !t.is_empty() {
            let count = |n: u64| thousands(n as usize);
            writeln!(f, "{rule}")?;
            writeln!(f, "Textures")?;
            writeln!(f, "{rule}")?;
            // The half of residency the cache cannot see: UV textures with no
            // `.tx`, held whole for the render.
            if t.preloaded > 0 {
                writeln!(
                    f,
                    "  {:<28} {} ({})",
                    "preloaded textures",
                    count(t.preloaded),
                    human_bytes(t.preloaded_bytes)
                )?;
            }
        }
        if t.lookups() > 0 {
            let count = |n: u64| thousands(n as usize);
            let share = |n: u64| 100.0 * n as f64 / t.lookups().max(1) as f64;
            writeln!(f, "  {:<28} {}", "streamed files", count(t.files))?;
            // Guerilla's texture memory triple. `total` is what preloading
            // would have held; `loaded` what was actually read (larger than
            // `still in cache` by whatever was evicted); `still in cache` is
            // what is resident now. loaded >> still in cache, with unloaded
            // tiles of the same order as loaded tiles, means the budget is
            // below the working set and the cache purged to fit.
            writeln!(f, "  {:<28} {}", "total memory", human_bytes(t.total_bytes))?;
            // Every decode counts, so this can exceed `total memory` with no
            // eviction at all: many workers first touching the same small
            // tiles decode them concurrently. Say so, or it reads as a bug.
            let rereads = t.redundant + t.raced;
            writeln!(
                f,
                "  {:<28} {}{}",
                "loaded memory",
                human_bytes(t.bytes_read),
                if rereads > 0 {
                    format!(
                        " (incl. {} tile(s) decoded more than once)",
                        thousands(rereads as usize)
                    )
                } else {
                    String::new()
                }
            )?;
            writeln!(
                f,
                "  {:<28} {}",
                "still in cache",
                human_bytes(t.resident_bytes)
            )?;
            writeln!(
                f,
                "  {:<28} {} / {}",
                "peak resident / budget",
                human_bytes(t.peak_bytes),
                human_bytes(t.budget_bytes)
            )?;
            writeln!(f, "  {:<28} {}", "loaded tiles", count(t.loaded_tiles))?;
            writeln!(f, "  {:<28} {}", "unloaded tiles", count(t.evictions))?;
            writeln!(f, "  {:<28} {}", "lookups", count(t.lookups()))?;
            writeln!(
                f,
                "    {:<26} {} ({:.1}%)",
                "thread microcache hits",
                count(t.micro_hits),
                share(t.micro_hits)
            )?;
            writeln!(
                f,
                "    {:<26} {} ({:.1}%)",
                "cache hits",
                count(t.hits),
                share(t.hits)
            )?;
            writeln!(
                f,
                "    {:<26} {} ({:.1}%)",
                "misses (read from disk)",
                count(t.misses),
                share(t.misses)
            )?;
            writeln!(f, "  {:<28} {:.2}%", "hit rate", 100.0 * t.hit_rate())?;
            // The line worth reading when a render is slower than it should
            // be: a tile decoded again after being evicted means the budget is
            // below the working set, which is the one thing a bigger budget
            // actually fixes.
            writeln!(
                f,
                "  {:<28} {}{}",
                "re-read after eviction",
                count(t.redundant),
                if t.redundant > t.misses / 4 && t.misses > 0 {
                    "   (raise CRUST_TEX_CACHE_MB)"
                } else {
                    ""
                }
            )?;
            // Deliberately a separate line: two workers decoding the same tile
            // at once is wasted work bounded by the thread count, and no
            // amount of budget removes it. Reporting it as thrashing sent the
            // first measured render chasing a cache size that was 0.006% full.
            if t.raced > 0 {
                writeln!(f, "  {:<28} {}", "concurrent double fills", count(t.raced))?;
            }
            if t.errors > 0 {
                writeln!(f, "  {:<28} {}", "tile read errors", count(t.errors))?;
            }
        }

        // -- Ptex ------------------------------------------------------
        let p = &self.ptex;
        if !p.is_empty() {
            writeln!(f, "{rule}")?;
            writeln!(f, "Ptex")?;
            writeln!(f, "{rule}")?;
            // The line the whole block exists for. An island run's peak RSS
            // cannot be read without knowing which backend produced it.
            // A mixed report is the normal case on a production stage, not a
            // warning — the island streams 39 of 3 618 and preloads the rest
            // by design. So "declined" and "failed" are named separately:
            // the first says the admission rule worked, the second says a
            // file is broken, and calling both a fallback (as this line once
            // did) reads as 3 579 errors.
            let backend = if p.streamed == 0
                && p.below_threshold == 0
                && p.budget_full == 0
                && p.mip_space == 0
                && p.open_failed == 0
            {
                // Nothing streamed and nothing considered: streaming is off.
                "preloaded".to_string()
            } else if p.streamed == p.textures {
                "streamed".to_string()
            } else {
                // A mixed report is the normal case on a production stage, not
                // a warning — the island streams 39 of 3 618 and preloads the
                // rest by design. So the two reasons are named separately: one
                // says the admission rule worked, the other says a file is
                // broken. Calling both a fallback, as this line once did, read
                // as 3 579 errors.
                let mut parts = Vec::new();
                if p.streamed > 0 {
                    parts.push(format!("{} streamed", thousands(p.streamed as usize)));
                }
                if p.below_threshold > 0 {
                    parts.push(format!(
                        "{} preloaded under the size threshold",
                        thousands(p.below_threshold as usize)
                    ));
                }
                if p.budget_full > 0 {
                    parts.push(format!(
                        "{} preloaded for want of budget (raise CRUST_PTEX_CACHE_MB)",
                        thousands(p.budget_full as usize)
                    ));
                }
                if p.mip_space > 0 {
                    parts.push(format!(
                        "{} preloaded for a linear mip chain \
                         (CRUST_PTEX_STREAM_MIPSPACE=file to stream them)",
                        thousands(p.mip_space as usize)
                    ));
                }
                if p.open_failed > 0 {
                    parts.push(format!(
                        "{} PRELOADED BECAUSE STREAMING FAILED",
                        thousands(p.open_failed as usize)
                    ));
                }
                parts.join(", ")
            };
            writeln!(f, "  {:<28} {}", "backend", backend)?;
            writeln!(
                f,
                "  {:<28} {} ({} faces)",
                "textures",
                thousands(p.textures as usize),
                thousands(p.faces as usize)
            )?;
            if p.preloaded_bytes > 0 {
                writeln!(
                    f,
                    "  {:<28} {}",
                    "preloaded resident",
                    human_bytes(p.preloaded_bytes)
                )?;
            }
            if p.streamed > 0 {
                // Resident is live rather than peak, and the budget is the
                // total across every streamed texture -- `FileAssets` divides
                // one budget over them so this is what was asked for, not a
                // multiple of it.
                writeln!(
                    f,
                    "  {:<28} {} / {} over {} textures",
                    "streamed resident / budget",
                    human_bytes(p.resident_bytes),
                    human_bytes(p.budget_bytes),
                    thousands(p.streamed as usize)
                )?;
                // The slots are process-wide, not per texture, and they hold
                // the half of residency the reader cannot count — decoded
                // tiles outside its cache. So they get their own line against
                // their own allowance rather than being folded into the
                // figure above, which is the reader's alone.
                writeln!(
                    f,
                    "  {:<28} {} / {}",
                    "thread tiles / reserve",
                    human_bytes(p.micro_retained_bytes),
                    human_bytes(p.micro_reserve_bytes)
                )?;
                writeln!(
                    f,
                    "  {:<28} {}",
                    "texel fetches",
                    thousands(p.lookups() as usize)
                )?;
                writeln!(
                    f,
                    "  {:<28} {} ({:.1}%)",
                    "  thread microcache hits",
                    thousands(p.micro_hits as usize),
                    100.0 * p.micro_rate()
                )?;
                writeln!(
                    f,
                    "  {:<28} {}",
                    "  reader cache hits",
                    thousands(p.cache_hits as usize)
                )?;
                writeln!(
                    f,
                    "  {:<28} {}",
                    "  reads from disk",
                    thousands(p.cache_misses as usize)
                )?;
                // Same reasoning as the `.tx` block's re-read line: evictions
                // running with the misses means the budget is under the
                // working set, which is the one thing raising it fixes.
                writeln!(
                    f,
                    "  {:<28} {}{}",
                    "evictions",
                    thousands(p.evictions as usize),
                    if p.evictions > p.cache_misses / 4 && p.cache_misses > 0 {
                        "   (raise CRUST_PTEX_CACHE_MB)"
                    } else {
                        ""
                    }
                )?;
            }
        }

        if self.phases.is_empty() {
            return Ok(());
        }

        // -- Phases by execution tree ----------------------------------
        writeln!(f, "{rule}")?;
        writeln!(f, "Phases by execution tree (wall clock)")?;
        // `rss` is what the phase left resident; `peak` the high-water
        // reached by its end. peak >> rss means transient churn.
        writeln!(
            f,
            "{:<NAME$} {:>9}  {:>5}  {:>9} {:>9}",
            "", "time", "%", "rss", "peak"
        )?;
        writeln!(f, "{rule}")?;
        for p in &self.phases {
            let indent = "  ".repeat(1 + p.depth as usize);
            let name_width = NAME.saturating_sub(indent.len());
            let mem = |b: Option<u64>| b.map(human_bytes).unwrap_or_default();
            writeln!(
                f,
                "{indent}{:<name_width$} {:>9}  {:>5.1}%  {:>9} {:>9}",
                p.name,
                human_duration(p.duration),
                pct(p.duration),
                mem(p.rss_end),
                mem(p.peak_end),
            )?;
        }
        writeln!(
            f,
            "  {:<width$} {:>9}",
            "total",
            human_duration(total),
            width = NAME - 2
        )?;

        // -- Phases by time --------------------------------------------
        // Sub-phases are listed alongside their parents, so the column
        // does not sum to the total; the marker says which are nested.
        writeln!(f, "{rule}")?;
        writeln!(f, "Phases by time (* = nested, counted in its parent)")?;
        writeln!(f, "{rule}")?;
        let mut by_time: Vec<&Phase> = self.phases.iter().collect();
        by_time.sort_by_key(|p| std::cmp::Reverse(p.duration));
        for p in by_time {
            let marker = if p.depth > 0 { "*" } else { " " };
            writeln!(
                f,
                "  {marker}{:<width$} {:>9}  {:>5.1}%",
                p.name,
                human_duration(p.duration),
                pct(p.duration),
                width = NAME - 3
            )?;
        }

        // -- Render profile (`--profile`) --------------------------------
        // After the phases, because it zooms into one of them: every figure
        // below is thread time inside the Render row above.
        if let Some(profile) = &self.profile {
            profile.write_report(f, &rule, self.render_phase().map(|p| p.duration))?;
        }
        write!(f, "{rule}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_skips_nested_phases() {
        let mut s = RenderStats::new();
        s.record("Parse", 0, Duration::from_secs(10));
        s.record("Open stage", 1, Duration::from_secs(4));
        s.record("Traverse", 1, Duration::from_secs(6));
        s.record("Render", 0, Duration::from_secs(30));
        // 10 + 30, not 10 + 4 + 6 + 30.
        assert_eq!(s.total(), Duration::from_secs(40));
    }

    #[test]
    fn report_lists_every_phase_in_both_views() {
        let mut s = RenderStats::new();
        s.record("Parse USD stage", 0, Duration::from_secs(2));
        s.record("Load assets", 1, Duration::from_millis(500));
        s.record("Trace paths", 0, Duration::from_secs(8));
        let out = s.report();
        assert!(out.contains("Phases by execution tree"));
        assert!(out.contains("Phases by time"));
        // Each phase appears once per profile view. The names here are
        // deliberately distinct from the report's own headings, so a
        // heading cannot be mistaken for a phase row.
        assert_eq!(out.matches("Load assets").count(), 2);
        assert_eq!(out.matches("Trace paths").count(), 2);
        assert_eq!(out.matches("Parse USD stage").count(), 2);
    }

    #[test]
    fn percentages_are_relative_to_top_level_total() {
        let mut s = RenderStats::new();
        s.record("A", 0, Duration::from_secs(1));
        s.record("B", 0, Duration::from_secs(3));
        let out = s.report();
        assert!(out.contains("25.0%"), "{out}");
        assert!(out.contains("75.0%"), "{out}");
    }

    #[test]
    fn zero_counts_are_omitted_from_the_breakdown() {
        let s = RenderStats {
            scene: SceneCounters {
                top_level: PrimitiveCounts {
                    triangles: 12,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let out = s.report();
        assert!(out.contains("triangles"));
        assert!(!out.contains("spheres"));
    }

    /// The kernel-memory block names every table the triangle storage holds,
    /// splits the packets by layout, and derives the two numbers a layout
    /// change is judged by: the lane fill and the bytes per resident triangle.
    #[test]
    fn report_shows_the_geometry_layout() {
        let fp = crate::rt::MemoryFootprint {
            vertices: 1_200,
            vertex_normals: 1_200,
            triangle_records: 4_800,
            packets: 0,
            packets_indexed: 4_600,
            bvh_nodes: 2_048,
            leaves: 320,
            geometry_tables: 24,
            lanes: 200,
            lanes_filled: 190,
            ..Default::default()
        };
        let s = RenderStats {
            scene: SceneCounters {
                top_level: PrimitiveCounts {
                    triangles: 200,
                    ..Default::default()
                },
                footprint: fp,
                ..Default::default()
            },
            ..Default::default()
        };
        let out = s.report();
        for row in [
            "vertices",
            "vertex normals",
            "triangle records",
            "triangle packets (indexed)",
            "geometry tables",
        ] {
            assert!(out.contains(row), "missing {row}: {out}");
        }
        assert!(!out.contains("triangle packets (gathered)"), "{out}");
        assert!(
            out.contains("lanes filled               95.0% (190 of 200)"),
            "{out}"
        );
        // 14 192 bytes over 200 triangles.
        assert!(out.contains("bytes per triangle         71.0"), "{out}");
        assert_eq!(fp.total(), 14_192);
    }

    #[test]
    fn unique_breakdown_is_shown_only_when_it_differs() {
        let flat = PrimitiveCounts {
            triangles: 3,
            ..Default::default()
        };
        // A scene with no instancing: printing the same numbers twice
        // would be noise.
        let same = RenderStats {
            scene: SceneCounters {
                top_level: flat,
                unique: flat,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(!same.report().contains("primitives in memory"));

        // Instanced: the two views genuinely differ, so both are useful.
        let instanced = RenderStats {
            scene: SceneCounters {
                top_level: PrimitiveCounts {
                    instances: 2,
                    ..Default::default()
                },
                unique: PrimitiveCounts {
                    instances: 2,
                    cubic_curve_spans: 900,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let out = instanced.report();
        assert!(out.contains("primitives in memory"));
        assert!(out.contains("900"));
    }

    #[test]
    fn primitive_counts_total_every_kind() {
        let c = PrimitiveCounts {
            triangles: 1,
            spheres: 2,
            disks: 6,
            cylinders: 7,
            curve_segments: 3,
            cubic_curve_spans: 4,
            instances: 5,
        };
        assert_eq!(c.total(), 28);
    }

    #[test]
    fn merge_takes_min_spp_only_over_counted_pixels() {
        let mut a = RayStats::default();
        // A unit with no adaptive pixel must not drag the minimum to 0.
        a.merge(&RayStats::default());
        let unit = |min, max| RayStats {
            adaptive_pixels: 1,
            spp_min: min,
            spp_max: max,
            ..Default::default()
        };
        a.merge(&unit(12, 20));
        a.merge(&RayStats::default());
        a.merge(&unit(8, 16));
        assert_eq!((a.spp_min, a.spp_max, a.adaptive_pixels), (8, 20, 2));
    }

    #[test]
    fn report_prints_guerilla_style_statistics() {
        let s = RenderStats {
            image: ImageCounters {
                width: 2,
                height: 2,
                samples_per_pixel: 4,
                max_depth: 8,
            },
            rays: RayStats {
                camera_rays: 16,
                closest_hit: 24,
                shadow_rays: 10,
                shadow_occluded: 5,
                vertices: 20,
                light_samples: 12,
                ended_escaped: 16,
                ..Default::default()
            },
            materials: vec![("OpenPBR", 3), ("Emissive", 1)],
            light_kinds: vec![("rect", 2)],
            textures: TextureCacheStats {
                preloaded: 2,
                preloaded_bytes: 2048,
                ..Default::default()
            },
            ..Default::default()
        };
        let out = s.report();
        for needle in [
            "allocated materials          4",
            "rect                       2",
            "bounce rays                  8",
            "shadow rays                  10 (50.0% occluded)",
            "shadow rays / shading point  0.500",
            "average samples / pixel      4.00",
            "preloaded textures           2 (2.00 KiB)",
        ] {
            assert!(out.contains(needle), "missing {needle:?} in\n{out}");
        }
        // No adaptive pixels were counted, so there is no adaptive line, and
        // no `--profile`, so no render profile.
        assert!(!out.contains("adaptive:"));
        assert!(!out.contains("Render profile"));
    }

    #[test]
    fn thousands_separates_groups_of_three() {
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(1234), "1 234");
        assert_eq!(thousands(1234567), "1 234 567");
    }

    #[test]
    fn human_bytes_scales_to_gibibytes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2 * 1024 * 1024 * 1024), "2.00 GiB");
    }

    #[test]
    fn parse_proc_status_bytes_reads_fields_from_one_snapshot() {
        // Field order and the trailing unit follow the real file; the
        // prefix match must not mistake `VmRSS` for `VmHWM`, nor the
        // `Rss*` breakdown lines for `VmRSS`.
        let status = "Name:\tcrust-render\nVmPeak:\t  900000 kB\nVmSize:\t  800000 kB\n\
                      VmHWM:\t  300000 kB\nVmRSS:\t  200000 kB\nRssAnon:\t  150000 kB\n\
                      RssFile:\t   50000 kB\nThreads:\t8\n";
        assert_eq!(
            parse_proc_status_bytes(status, "VmRSS:"),
            Some(200_000 * 1024)
        );
        assert_eq!(
            parse_proc_status_bytes(status, "VmHWM:"),
            Some(300_000 * 1024)
        );
        assert_eq!(parse_proc_status_bytes(status, "VmSwap:"), None);
        assert_eq!(
            parse_proc_status_bytes("VmRSS:\t  bogus kB\n", "VmRSS:"),
            None
        );
    }
}
