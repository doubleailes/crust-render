//! Render profile: where the *render* spends its time, section by section.
//!
//! [`crate::stats`] times the coarse phases of a run — parse, build, render,
//! write — with one `Instant` each, which says the render took 38 s but not
//! whether that was ray traversal, shading, texture lookups or light
//! sampling. This module answers that, after Guerilla Render's "Render
//! Profile": the integrator, the kernel queries and the shading engine are
//! wrapped in named [`Section`]s, each belonging to a [`Category`], and the
//! result is reported three ways — flat by time, by category, and by
//! execution tree with Guerilla's `local` / `total` / `glob.` columns.
//!
//! **It is opt-in, and has to be.** A section costs two clock reads and a
//! little bookkeeping (~40–60 ns, measured at start-up and printed with the
//! report), against a few hundred nanoseconds for a ray query, so profiling
//! slows a render by a scene-dependent 10–30 %. `--stats` is what
//! `bench_ab.sh -p Render` reads, so it must not pay that; `--profile` does.
//! Disabled, a section is one relaxed atomic load and a predictable branch.
//! The overhead is *inside* the numbers: a parent's local time includes the
//! cost of timing its children, which the report estimates rather than
//! subtracts (subtracting an estimate can drive a small section negative).
//!
//! Recording is per thread and lock-free: each worker builds its own call
//! tree in a thread-local, and the renderer calls [`flush`] once per work
//! unit (a tile, or a pixel in scanline mode) to merge it into the global
//! tree. Times are **thread time** — summed over workers — so a 10 s render
//! on 16 threads profiles about 160 s, exactly as Guerilla's does.

use std::cell::RefCell;
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// What a section's time is spent on, at the granularity Guerilla groups
/// by. A render normally splits roughly evenly between the first three; a
/// strong deviation is the thing worth investigating.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    Raytrace,
    Integrator,
    Shading,
    IO,
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::fmt::Display for Section {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl Category {
    pub fn name(self) -> &'static str {
        match self {
            Category::Raytrace => "Raytrace",
            Category::Integrator => "Integrator",
            Category::Shading => "Shading",
            Category::IO => "IO",
        }
    }
}

/// A timed region of the render. Names follow Guerilla's where the meaning
/// matches, so a profile reads the same in either renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    /// One pixel's sample loop, adaptive stop included — the root every
    /// other section nests under.
    MainLoop,
    /// Filter importance sampling, the camera ray and its cone.
    GeneratePrimary,
    /// Closest-hit kernel queries: camera and bounce rays.
    Trace,
    /// Shadow rays: the occlusion query plus any volume transmittance.
    Occlusion,
    /// Distance sampling through volume regions and carried media.
    Volume,
    /// Resolving a hit's material into a shading point — the per-vertex
    /// network run for MaterialX, `UsdUVTexture` and Ptex surfaces.
    EvalBsdfs,
    /// A MaterialX program's execution (interpreter or JIT).
    RunShader,
    /// Texture lookups, UV and Ptex, preloaded or streamed.
    Texture,
    /// A streamed tile paged in from disk on a cache miss.
    TextureLoad,
    /// Next-event estimation at a surface vertex: light pick and sample,
    /// BSDF evaluation, the shadow ray (nested as `Occlusion`).
    SurfaceLighting,
    /// Next-event estimation at a volume scatter vertex.
    VolumeLighting,
    /// Choosing the continuation: BSDF or guide sampling, and roulette.
    Bounce,
    /// A subsurface random walk, entry to exit record: its free flights,
    /// owner-only ray casts and channel MIS.
    Subsurface,
    /// The backward gather folding a path's vertices into its radiance
    /// (and emitting guiding training samples).
    Contributions,
}

impl Section {
    pub const ALL: [Section; 14] = [
        Section::MainLoop,
        Section::GeneratePrimary,
        Section::Trace,
        Section::Occlusion,
        Section::Volume,
        Section::EvalBsdfs,
        Section::RunShader,
        Section::Texture,
        Section::TextureLoad,
        Section::SurfaceLighting,
        Section::VolumeLighting,
        Section::Bounce,
        Section::Subsurface,
        Section::Contributions,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Section::MainLoop => "MainLoop",
            Section::GeneratePrimary => "GeneratePrimary",
            Section::Trace => "Trace",
            Section::Occlusion => "Occlusion",
            Section::Volume => "Volume",
            Section::EvalBsdfs => "EvalBsdfs",
            Section::RunShader => "RunShader",
            Section::Texture => "Texture",
            Section::TextureLoad => "TextureLoad",
            Section::SurfaceLighting => "SurfaceLighting",
            Section::VolumeLighting => "VolumeLighting",
            Section::Bounce => "Bounce",
            Section::Subsurface => "Subsurface",
            Section::Contributions => "Contributions",
        }
    }

    pub fn category(self) -> Category {
        match self {
            Section::Trace | Section::Occlusion | Section::Volume => Category::Raytrace,
            Section::EvalBsdfs | Section::RunShader | Section::Texture => Category::Shading,
            Section::TextureLoad => Category::IO,
            Section::MainLoop
            | Section::GeneratePrimary
            | Section::SurfaceLighting
            | Section::VolumeLighting
            | Section::Bounce
            | Section::Subsurface
            | Section::Contributions => Category::Integrator,
        }
    }
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static GLOBAL: Mutex<Option<Tree>> = Mutex::new(None);

thread_local! {
    static RECORDER: RefCell<Recorder> = const { RefCell::new(Recorder::new()) };
}

/// Turns section timing on or off for the whole process, discarding
/// anything recorded so far. Call before the render, not during it.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    *GLOBAL.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[inline(always)]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Times `section` until the guard drops. Free (a load and a branch) while
/// profiling is off.
///
/// Both halves are split the same way: an `inline(always)` test of the
/// switch, and the recording itself behind a `cold`, never-inlined call.
/// Without that the guard's drop glue was an out-of-line call per section
/// even when disabled, and the recording code grew `trace_path` past what
/// LLVM would inline into `render_pixel` — together +4.5% instructions on
/// cornellbox for a feature that was switched off.
#[inline(always)]
#[must_use = "the section ends when the guard is dropped"]
pub fn scope(section: Section) -> Scope {
    let active = enabled();
    if active {
        enter_cold(section);
    }
    Scope {
        active,
        _thread: std::marker::PhantomData,
    }
}

/// [`scope`] decided at compile time: with `ON = false` the guard is inert
/// and the whole section compiles away. The integrator is monomorphised on
/// this — the switch is read once per pass (`Renderer::render_pass`) — so an
/// unprofiled render runs the same machine code as a renderer with no
/// profiler at all. A per-section runtime check cost +2.5% instructions on
/// cornellbox, mostly by pushing `trace_path` out of line.
#[inline(always)]
pub fn scope_if<const ON: bool>(section: Section) -> Scope {
    if ON {
        scope(section)
    } else {
        Scope {
            active: false,
            _thread: std::marker::PhantomData,
        }
    }
}

#[cold]
#[inline(never)]
fn enter_cold(section: Section) {
    RECORDER.with(|r| r.borrow_mut().enter(section));
}

#[cold]
#[inline(never)]
fn exit_cold() {
    RECORDER.with(|r| r.borrow_mut().exit());
}

/// Guard returned by [`scope`]. Not `Send`: a section must end on the
/// thread it began on, since the tree it belongs to is thread-local.
pub struct Scope {
    active: bool,
    _thread: std::marker::PhantomData<*const ()>,
}

impl Drop for Scope {
    #[inline(always)]
    fn drop(&mut self) {
        if self.active {
            exit_cold();
        }
    }
}

/// Merges this thread's recording into the global profile and zeroes it.
/// The renderer calls it at the end of every work unit. A no-op while
/// profiling is off, and while this thread is inside a section (the open
/// section's time is not known yet, so it waits for the next flush).
pub fn flush() {
    if !enabled() {
        return;
    }
    RECORDER.with(|r| {
        let mut r = r.borrow_mut();
        if !r.starts.is_empty() || r.tree.nodes.is_empty() {
            return;
        }
        let mut global = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        let dst = global.get_or_insert_with(Tree::new);
        dst.merge_from(&r.tree);
        r.tree.zero();
    });
}

/// Everything flushed since [`set_enabled`], or `None` if nothing was
/// recorded. `threads` is recorded with it, for the utilisation line.
pub fn take() -> Option<RenderProfile> {
    flush();
    let tree = GLOBAL.lock().unwrap_or_else(|e| e.into_inner()).take()?;
    if tree.nodes.len() <= 1 {
        return None;
    }
    Some(RenderProfile {
        tree,
        threads: rayon::current_num_threads(),
        scope_cost: measure_scope_cost(),
    })
}

/// What one `enter` + `exit` pair costs on this machine, timed on a private
/// recorder so nothing leaks into the real profile. Min of a few rounds,
/// since the question is the floor, not what a preempted round cost.
fn measure_scope_cost() -> Duration {
    const N: u32 = 1 << 15;
    let mut rec = Recorder::new();
    let mut best = Duration::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        for _ in 0..N {
            rec.enter(Section::Trace);
            rec.exit();
        }
        best = best.min(t.elapsed());
    }
    best / N
}

#[derive(Clone, Debug)]
struct Node {
    /// `None` only for the root.
    section: Option<Section>,
    parent: u32,
    children: Vec<u32>,
    ns: u64,
    calls: u64,
}

/// A call tree: node 0 is the root, every other node is one section
/// reached by one path from it.
#[derive(Clone, Debug, Default)]
struct Tree {
    nodes: Vec<Node>,
}

impl Tree {
    const fn empty() -> Self {
        Tree { nodes: Vec::new() }
    }

    fn new() -> Self {
        let mut t = Tree::empty();
        t.ensure_root();
        t
    }

    fn ensure_root(&mut self) {
        if self.nodes.is_empty() {
            self.nodes.push(Node {
                section: None,
                parent: 0,
                children: Vec::new(),
                ns: 0,
                calls: 0,
            });
        }
    }

    /// The child of `parent` for `section`, created if this path is new.
    fn child(&mut self, parent: u32, section: Section) -> u32 {
        for &c in &self.nodes[parent as usize].children {
            if self.nodes[c as usize].section == Some(section) {
                return c;
            }
        }
        let id = self.nodes.len() as u32;
        self.nodes.push(Node {
            section: Some(section),
            parent,
            children: Vec::new(),
            ns: 0,
            calls: 0,
        });
        self.nodes[parent as usize].children.push(id);
        id
    }

    fn merge_from(&mut self, src: &Tree) {
        self.ensure_root();
        // Iterative so a deep tree cannot overflow the stack.
        let mut stack = vec![(0u32, 0u32)];
        while let Some((s, d)) = stack.pop() {
            for &sc in &src.nodes[s as usize].children {
                let n = &src.nodes[sc as usize];
                let section = n.section.expect("only the root has no section");
                let dc = self.child(d, section);
                self.nodes[dc as usize].ns += n.ns;
                self.nodes[dc as usize].calls += n.calls;
                stack.push((sc, dc));
            }
        }
    }

    fn zero(&mut self) {
        for n in &mut self.nodes {
            n.ns = 0;
            n.calls = 0;
        }
    }

    fn children_ns(&self, id: u32) -> u64 {
        self.nodes[id as usize]
            .children
            .iter()
            .map(|&c| self.nodes[c as usize].ns)
            .sum()
    }

    /// Time spent in the node itself, children excluded.
    fn local_ns(&self, id: u32) -> u64 {
        self.nodes[id as usize]
            .ns
            .saturating_sub(self.children_ns(id))
    }

    /// Whether some ancestor of `id` is the same section — for recursion,
    /// whose inclusive time would otherwise be counted once per level.
    fn nested_in_itself(&self, id: u32) -> bool {
        let s = self.nodes[id as usize].section;
        let mut p = self.nodes[id as usize].parent;
        while p != 0 {
            if self.nodes[p as usize].section == s {
                return true;
            }
            p = self.nodes[p as usize].parent;
        }
        false
    }
}

struct Recorder {
    tree: Tree,
    current: u32,
    starts: Vec<Instant>,
}

impl Recorder {
    const fn new() -> Self {
        Recorder {
            tree: Tree::empty(),
            current: 0,
            starts: Vec::new(),
        }
    }

    #[inline]
    fn enter(&mut self, section: Section) {
        self.tree.ensure_root();
        self.current = self.tree.child(self.current, section);
        self.starts.push(Instant::now());
    }

    #[inline]
    fn exit(&mut self) {
        let Some(start) = self.starts.pop() else {
            return;
        };
        let ns = start.elapsed().as_nanos() as u64;
        let node = &mut self.tree.nodes[self.current as usize];
        node.ns += ns;
        node.calls += 1;
        self.current = node.parent;
    }
}

/// Per-section totals over the whole tree.
#[derive(Clone, Copy, Debug, Default)]
pub struct SectionTotals {
    /// Time in the section itself, children excluded — what `glob.` is.
    pub local: Duration,
    /// Time in the section with its children, counted once under recursion.
    pub total: Duration,
    pub calls: u64,
}

/// A finished render profile. See the module docs.
#[derive(Clone, Debug)]
pub struct RenderProfile {
    tree: Tree,
    /// Rayon workers the render ran on.
    pub threads: usize,
    /// Measured cost of one section's timing.
    pub scope_cost: Duration,
}

impl RenderProfile {
    /// Thread time under the root — the 100 % every percentage is of.
    pub fn thread_time(&self) -> Duration {
        Duration::from_nanos(self.tree.children_ns(0))
    }

    /// Timed sections entered, over every thread.
    pub fn scope_count(&self) -> u64 {
        self.tree.nodes.iter().map(|n| n.calls).sum()
    }

    pub fn section(&self, s: Section) -> SectionTotals {
        let mut out = SectionTotals::default();
        for (i, n) in self.tree.nodes.iter().enumerate() {
            if n.section != Some(s) {
                continue;
            }
            let i = i as u32;
            out.local += Duration::from_nanos(self.tree.local_ns(i));
            out.calls += n.calls;
            if !self.tree.nested_in_itself(i) {
                out.total += Duration::from_nanos(n.ns);
            }
        }
        out
    }

    /// The report: flat, by category, by execution tree. `render_wall` is
    /// the Render phase's wall clock, for the utilisation line.
    pub fn write_report(
        &self,
        f: &mut fmt::Formatter<'_>,
        rule: &str,
        render_wall: Option<Duration>,
    ) -> fmt::Result {
        let root_ns = self.tree.children_ns(0).max(1) as f64;
        let pct = |d: Duration| 100.0 * d.as_nanos() as f64 / root_ns;

        let sections = self.sections_by_local();

        // -- Profile ---------------------------------------------------
        writeln!(f, "{rule}")?;
        writeln!(f, "Render profile (thread time; glob. = local share)")?;
        writeln!(
            f,
            "  {:<28} {:>6}  {:>9}  {:>14}  {:>9}",
            "name", "glob.", "time", "calls", "avg"
        )?;
        writeln!(f, "{rule}")?;
        for (s, t) in &sections {
            writeln!(
                f,
                "  {:<28} {:>6.1}  {:>9}  {:>14}  {:>9}",
                s.name(),
                pct(t.local),
                super::stats::human_duration(t.local),
                super::stats::thousands(t.calls as usize),
                per_call(t.total, t.calls),
            )?;
        }

        // -- Profile by category ---------------------------------------
        writeln!(f, "{rule}")?;
        writeln!(f, "Render profile by category")?;
        writeln!(f, "  {:<28} {:>6}  {:>9}", "name", "glob.", "time")?;
        writeln!(f, "{rule}")?;
        let mut cats: Vec<(Category, Duration)> = Vec::new();
        for (s, t) in &sections {
            match cats.iter_mut().find(|(c, _)| *c == s.category()) {
                Some((_, d)) => *d += t.local,
                None => cats.push((s.category(), t.local)),
            }
        }
        cats.sort_by_key(|(_, d)| std::cmp::Reverse(*d));
        for (c, d) in &cats {
            writeln!(
                f,
                "  {:<28} {:>6.1}  {:>9}",
                c.name(),
                pct(*d),
                super::stats::human_duration(*d)
            )?;
            for (s, t) in sections.iter().filter(|(s, _)| s.category() == *c) {
                writeln!(
                    f,
                    "    {:<26} {:>6.1}  {:>9}",
                    s.name(),
                    pct(t.local),
                    super::stats::human_duration(t.local)
                )?;
            }
        }

        // -- Profile by execution tree ---------------------------------
        writeln!(f, "{rule}")?;
        writeln!(f, "Render profile by execution tree")?;
        writeln!(
            f,
            "  {:<32} {:>6} {:>6} {:>6}  {:>9}  {:>14}",
            "path", "local", "total", "glob.", "time", "calls"
        )?;
        writeln!(f, "{rule}")?;
        writeln!(
            f,
            "  {:<32} {:>6.1} {:>6.1} {:>6.1}  {:>9}  {:>14}",
            "Root",
            0.0,
            100.0,
            0.0,
            super::stats::human_duration(self.thread_time()),
            ""
        )?;
        // Depth-first, heaviest child first, so the hot path reads top-down.
        let mut stack: Vec<(u32, usize)> = self.sorted_children(0).map(|c| (c, 1)).collect();
        stack.reverse();
        while let Some((id, depth)) = stack.pop() {
            let n = &self.tree.nodes[id as usize];
            let s = n.section.expect("only the root has no section");
            let glob = pct(self.section(s).local);
            let path = format!("{}{}", ".".repeat(depth), s.name());
            writeln!(
                f,
                "  {:<32} {:>6.1} {:>6.1} {:>6.1}  {:>9}  {:>14}",
                path,
                pct(Duration::from_nanos(self.tree.local_ns(id))),
                pct(Duration::from_nanos(n.ns)),
                glob,
                super::stats::human_duration(Duration::from_nanos(n.ns)),
                super::stats::thousands(n.calls as usize),
            )?;
            let mut kids: Vec<(u32, usize)> =
                self.sorted_children(id).map(|c| (c, depth + 1)).collect();
            kids.reverse();
            stack.extend(kids);
        }
        writeln!(f, "{rule}")?;
        writeln!(
            f,
            "  local = in the section itself; total = with its children; glob. = the \
             section's local time summed over every path"
        )?;
        // Utilisation: how much of threads x wall the sections account for.
        // Well under 100% means workers idled — load imbalance at the end of
        // a pass, or time outside `MainLoop` (the guiding field's rebuilds).
        let thread_time = self.thread_time();
        if let Some(wall) = render_wall.filter(|w| !w.is_zero()) {
            let capacity = wall.as_secs_f64() * self.threads as f64;
            writeln!(
                f,
                "  {:<28} {} over {} threads, {:.1}% of {} x {}",
                "profiled thread time",
                super::stats::human_duration(thread_time).trim(),
                self.threads,
                100.0 * thread_time.as_secs_f64() / capacity,
                self.threads,
                super::stats::human_duration(wall).trim(),
            )?;
        }
        let overhead =
            Duration::from_secs_f64(self.scope_cost.as_secs_f64() * self.scope_count() as f64);
        writeln!(
            f,
            "  {:<28} ~{} ns/section x {} = {} ({:.1}% of thread time, charged to parents)",
            "profiling overhead (est.)",
            self.scope_cost.as_nanos(),
            super::stats::thousands(self.scope_count() as usize),
            super::stats::human_duration(overhead).trim(),
            100.0 * overhead.as_secs_f64() / thread_time.as_secs_f64().max(1e-12),
        )
    }

    /// Every profiled section with its totals, heaviest local time first —
    /// the rows of the flat report.
    fn sections_by_local(&self) -> Vec<(Section, SectionTotals)> {
        let mut sections: Vec<(Section, SectionTotals)> = Section::ALL
            .iter()
            .map(|&s| (s, self.section(s)))
            .filter(|(_, t)| t.calls > 0)
            .collect();
        sections.sort_by_key(|(_, t)| std::cmp::Reverse(t.local));
        sections
    }

    fn sorted_children(&self, id: u32) -> impl Iterator<Item = u32> {
        let mut kids = self.tree.nodes[id as usize].children.clone();
        kids.retain(|&c| self.tree.nodes[c as usize].calls > 0);
        kids.sort_by_key(|&c| std::cmp::Reverse(self.tree.nodes[c as usize].ns));
        kids.into_iter()
    }
}

/// The profile in `crust-stats/1`, under `profile`: the flat section table
/// and the execution tree, in the text report's row order, times in seconds
/// of thread time.
impl serde::Serialize for RenderProfile {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        #[derive(serde::Serialize)]
        struct SectionJson {
            name: &'static str,
            category: &'static str,
            local_s: f64,
            total_s: f64,
            calls: u64,
        }
        #[derive(serde::Serialize)]
        struct NodeJson {
            section: &'static str,
            depth: usize,
            local_s: f64,
            total_s: f64,
            calls: u64,
        }
        #[derive(serde::Serialize)]
        struct ProfileJson {
            threads: usize,
            thread_time_s: f64,
            scope_count: u64,
            scope_cost_s: f64,
            sections: Vec<SectionJson>,
            tree: Vec<NodeJson>,
        }
        let sections = self
            .sections_by_local()
            .into_iter()
            .map(|(sec, t)| SectionJson {
                name: sec.name(),
                category: sec.category().name(),
                local_s: t.local.as_secs_f64(),
                total_s: t.total.as_secs_f64(),
                calls: t.calls,
            })
            .collect();
        let mut tree = Vec::new();
        let mut stack: Vec<(u32, usize)> = self.sorted_children(0).map(|c| (c, 1)).collect();
        stack.reverse();
        while let Some((id, depth)) = stack.pop() {
            let n = &self.tree.nodes[id as usize];
            tree.push(NodeJson {
                section: n.section.expect("only the root has no section").name(),
                depth,
                local_s: Duration::from_nanos(self.tree.local_ns(id)).as_secs_f64(),
                total_s: Duration::from_nanos(n.ns).as_secs_f64(),
                calls: n.calls,
            });
            let mut kids: Vec<(u32, usize)> =
                self.sorted_children(id).map(|c| (c, depth + 1)).collect();
            kids.reverse();
            stack.extend(kids);
        }
        ProfileJson {
            threads: self.threads,
            thread_time_s: self.thread_time().as_secs_f64(),
            scope_count: self.scope_count(),
            scope_cost_s: self.scope_cost.as_secs_f64(),
            sections,
            tree,
        }
        .serialize(s)
    }
}

/// Mean inclusive time per call, in the unit that keeps it readable.
fn per_call(total: Duration, calls: u64) -> String {
    if calls == 0 {
        return String::new();
    }
    let ns = total.as_nanos() as f64 / calls as f64;
    if ns >= 1e6 {
        format!("{:.2} ms", ns / 1e6)
    } else if ns >= 1e3 {
        format!("{:.2} us", ns / 1e3)
    } else {
        format!("{ns:.0} ns")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree_of(rec: &Recorder) -> RenderProfile {
        let mut t = Tree::new();
        t.merge_from(&rec.tree);
        RenderProfile {
            tree: t,
            threads: 1,
            scope_cost: Duration::ZERO,
        }
    }

    fn spin(d: Duration) {
        let t = Instant::now();
        while t.elapsed() < d {}
    }

    #[test]
    fn local_excludes_children_and_total_includes_them() {
        let mut rec = Recorder::new();
        rec.enter(Section::MainLoop);
        spin(Duration::from_millis(2));
        rec.enter(Section::Trace);
        spin(Duration::from_millis(4));
        rec.exit();
        rec.exit();
        let p = tree_of(&rec);
        let main = p.section(Section::MainLoop);
        let trace = p.section(Section::Trace);
        assert!(main.total >= trace.total + Duration::from_millis(2));
        assert!(main.local < main.total);
        assert_eq!(main.total, main.local + trace.total);
        assert_eq!(p.thread_time(), main.total);
    }

    #[test]
    fn one_section_on_two_paths_sums_into_glob() {
        let mut rec = Recorder::new();
        rec.enter(Section::MainLoop);
        rec.enter(Section::Trace);
        rec.exit();
        rec.enter(Section::SurfaceLighting);
        rec.enter(Section::Trace);
        rec.exit();
        rec.exit();
        rec.enter(Section::Trace);
        rec.exit();
        rec.exit();
        let p = tree_of(&rec);
        // Two distinct paths reach Trace; the repeat under MainLoop reuses
        // its node rather than making a third.
        assert_eq!(p.section(Section::Trace).calls, 3);
        let trace_nodes = p
            .tree
            .nodes
            .iter()
            .filter(|n| n.section == Some(Section::Trace))
            .count();
        assert_eq!(trace_nodes, 2);
    }

    #[test]
    fn recursion_is_counted_once_in_total() {
        let mut rec = Recorder::new();
        rec.enter(Section::Texture);
        rec.enter(Section::Texture);
        spin(Duration::from_millis(2));
        rec.exit();
        rec.exit();
        let p = tree_of(&rec);
        let t = p.section(Section::Texture);
        assert!(t.total <= p.thread_time());
        assert_eq!(t.local, p.thread_time());
    }

    #[test]
    fn merging_twice_doubles_counts_without_new_nodes() {
        let mut rec = Recorder::new();
        rec.enter(Section::MainLoop);
        rec.enter(Section::Bounce);
        rec.exit();
        rec.exit();
        let mut t = Tree::new();
        t.merge_from(&rec.tree);
        let n = t.nodes.len();
        t.merge_from(&rec.tree);
        assert_eq!(t.nodes.len(), n);
        assert!(t.nodes.iter().skip(1).all(|n| n.calls == 2));
    }

    #[test]
    fn every_section_has_a_distinct_name() {
        let mut names: Vec<_> = Section::ALL.iter().map(|s| s.name()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), Section::ALL.len());
    }
}
