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
//! never per ray. Nothing here touches the integrator's inner loop, so
//! measuring costs the same whether or not the report is printed.

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
    pub fn now() -> Self {
        MemorySample {
            rss: current_memory_bytes(),
            peak: peak_memory_bytes(),
        }
    }
}

/// Primitives split by kind. Mirrors [`crate::rt::PrimitiveBreakdown`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrimitiveCounts {
    pub triangles: usize,
    pub spheres: usize,
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
}

impl RayStats {
    /// All ray queries, of every kind.
    pub fn total_rays(&self) -> u64 {
        self.closest_hit + self.shadow_rays
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

    /// Sums another unit's counters into this one.
    pub fn merge(&mut self, o: &RayStats) {
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
        self.lookups() == 0
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

/// Reads one `VmXxx:` field of `/proc/self/status`, in bytes.
#[cfg(target_os = "linux")]
fn proc_status_bytes(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
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
/// memory a phase took and gave back.
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
fn thousands(n: usize) -> String {
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

fn human_bytes(bytes: u64) -> String {
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
fn human_duration(d: Duration) -> String {
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

        writeln!(f, "  {:<28} {}", "lights", thousands(s.lights))?;
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
                ("primitive nodes", fp.prim_nodes),
                ("boxed primitives", fp.boxed_prims),
                ("BVH nodes", fp.bvh_nodes),
                ("triangle packets", fp.packets),
                ("leaf indices", fp.indices),
                ("leaves", fp.leaves),
            ] {
                if bytes > 0 {
                    writeln!(f, "    {:<26} {}", label, human_bytes(bytes as u64))?;
                }
            }
        }
        if let Some(peak) = peak_memory_bytes() {
            writeln!(f, "  {:<28} {}", "peak memory (RSS)", human_bytes(peak))?;
        }

        // -- Ray statistics -------------------------------------------
        let r = &self.rays;
        if !r.is_empty() {
            writeln!(f, "{rule}")?;
            writeln!(f, "Ray Statistics")?;
            writeln!(f, "{rule}")?;
            writeln!(
                f,
                "  {:<28} {}",
                "camera rays",
                thousands(r.camera_rays as usize)
            )?;
            writeln!(
                f,
                "  {:<28} {}",
                "closest-hit queries",
                thousands(r.closest_hit as usize)
            )?;
            writeln!(
                f,
                "  {:<28} {}",
                "shadow rays",
                thousands(r.shadow_rays as usize)
            )?;
            writeln!(
                f,
                "  {:<28} {}",
                "total ray queries",
                thousands(r.total_rays() as usize)
            )?;
            writeln!(
                f,
                "  {:<28} {}",
                "vertices shaded",
                thousands(r.vertices as usize)
            )?;
            writeln!(
                f,
                "  {:<28} {:.2}",
                "mean path length",
                r.mean_path_length()
            )?;
            // Throughput needs the render phase alone, not the whole run:
            // dividing by total would credit rays to time spent parsing.
            if let Some(render) = self
                .phases
                .iter()
                .find(|p| p.depth == 0 && p.name == "Render")
            {
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
            writeln!(
                f,
                "  {:<28} {} of {} ({:.1}%)",
                "roulette kills",
                thousands(r.rr_killed as usize),
                thousands(r.rr_tested as usize),
                100.0 * r.rr_kill_rate()
            )?;
            writeln!(
                f,
                "  {:<28} {}",
                "paths ended: escaped",
                thousands(r.ended_escaped as usize)
            )?;
            writeln!(
                f,
                "  {:<28} {}",
                "paths ended: depth cap",
                thousands(r.ended_depth as usize)
            )?;
        }

        // -- Texture cache ---------------------------------------------
        let t = &self.textures;
        if !t.is_empty() {
            writeln!(f, "{rule}")?;
            writeln!(f, "Texture Cache")?;
            writeln!(f, "{rule}")?;
            writeln!(f, "  {:<28} {}", "lookups", thousands(t.lookups() as usize))?;
            writeln!(
                f,
                "  {:<28} {} ({:.1}%)",
                "  thread microcache hits",
                thousands(t.micro_hits as usize),
                100.0 * t.micro_hits as f64 / t.lookups().max(1) as f64
            )?;
            writeln!(
                f,
                "  {:<28} {} ({:.1}%)",
                "  cache hits",
                thousands(t.hits as usize),
                100.0 * t.hits as f64 / t.lookups().max(1) as f64
            )?;
            writeln!(
                f,
                "  {:<28} {} ({:.1}%)",
                "  misses (read from disk)",
                thousands(t.misses as usize),
                100.0 * t.misses as f64 / t.lookups().max(1) as f64
            )?;
            writeln!(f, "  {:<28} {:.2}%", "hit rate", 100.0 * t.hit_rate())?;
            writeln!(
                f,
                "  {:<28} {} / {}",
                "peak resident / budget",
                human_bytes(t.peak_bytes),
                human_bytes(t.budget_bytes)
            )?;
            writeln!(
                f,
                "  {:<28} {}",
                "read from disk",
                human_bytes(t.bytes_read)
            )?;
            writeln!(
                f,
                "  {:<28} {}",
                "evictions",
                thousands(t.evictions as usize)
            )?;
            // The line worth reading when a render is slower than it should
            // be: a tile decoded again after being evicted means the budget is
            // below the working set, which is the one thing a bigger budget
            // actually fixes.
            writeln!(
                f,
                "  {:<28} {}{}",
                "re-read after eviction",
                thousands(t.redundant as usize),
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
                writeln!(
                    f,
                    "  {:<28} {}",
                    "concurrent double fills",
                    thousands(t.raced as usize)
                )?;
            }
            if t.errors > 0 {
                writeln!(
                    f,
                    "  {:<28} {}",
                    "tile read errors",
                    thousands(t.errors as usize)
                )?;
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

        // -- Profile by execution tree ---------------------------------
        writeln!(f, "{rule}")?;
        writeln!(f, "Profile by execution tree")?;
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

        // -- Profile by time -------------------------------------------
        // Sub-phases are listed alongside their parents, so the column
        // does not sum to the total; the marker says which are nested.
        writeln!(f, "{rule}")?;
        writeln!(f, "Profile by time (* = nested, counted in its parent)")?;
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
        assert!(out.contains("Profile by execution tree"));
        assert!(out.contains("Profile by time"));
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
            curve_segments: 3,
            cubic_curve_spans: 4,
            instances: 5,
        };
        assert_eq!(c.total(), 15);
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
}
