//! Routing a path's light into its light path expressions, and the albedo
//! it carries — the AOV instantiation of `trace_path` only.
//!
//! The beauty is one backward recurrence over the path's records:
//! `R(k) = segment_emit + atten · (emit_here + nee + factor · (next_emit · w + R(k+1)))`.
//! An LPE AOV is the same recurrence with each term kept only when the event
//! word it ends accepts the expression. Every term ends a different word, so
//! the walk forward records each vertex's events — per lobe, because crust's
//! BSDFs are one-sample mixtures: a bounce's weight `f_all / p_all` holds
//! every lobe's value toward the sampled direction, and routing all of it by
//! the lobe that was picked would put specular energy into a diffuse AOV
//! (biased per AOV, though the sum would still match). Splitting `f_all` into
//! its lobe summands `f_j` routes each lobe's share by its own event, which
//! is exact.
//!
//! The backward gather then evaluates the recurrence once per (vertex, DFA
//! state reachable there), for every expression at once. Where every lobe
//! of a vertex leads to the same state — the common case, and always for
//! `C.*[LO]` — it uses the beauty's own totals (`nee`, `factor`) in the
//! beauty's own expression, so that AOV is the beauty bit for bit, and a
//! partition of AOVs sums to it up to rounding.
//!
//! MIS is untouched: NEE and bounce keep their weights; routing only chooses
//! which AOV each weighted contribution lands in.

use glam::Vec3A;

use crate::lpe::{self, EventType, LabelId, LobeEvent, LobeLabel, Lpe, MAX_SPLIT, Scatter};

use super::path::VertexRec;

/// A symbol slot meaning "no event": the vertex passes the path's state
/// through unchanged (a subsurface walk's exit, which the entry already
/// counted).
pub(crate) const NO_EVENT: u16 = u16::MAX;

/// A light-list index meaning "no entry": emission that is not a light's
/// (`O`), or a backdrop, which the light list does not hold.
pub const NO_LIGHT: u32 = u32::MAX;

/// What the routing needs from the render: the compiled expressions and the
/// symbols of the events the integrator emits.
#[derive(Debug)]
pub struct RouteCtx {
    lpe: Option<Lpe>,
    /// The light path inspector's alphabet ([`RouteCtx::inspection`]): no
    /// expressions, every lobe named, lights by index.
    inspect: bool,
    /// Per light-list index, its `L` symbol (with the light's tag).
    light: Vec<u16>,
    /// `L` with no tag: backdrops.
    backdrop: u16,
    /// `O`: emission that is not a light.
    object: u16,
    /// `V`: a volume scatter.
    volume: u16,
    /// `Ts`: a cutout passed straight through.
    straight: u16,
    /// `TS` `'transmission'`: a thin wall passed straight through — the
    /// event its delta transmission was when the wall was a vertex.
    thin: u16,
    /// Whether the albedo is wanted.
    pub(crate) albedo: bool,
    /// Whether the first hit's diffuse filter is wanted (raw light AOVs,
    /// `diffuse_albedo`).
    pub(crate) diffuse_filter: bool,
}

impl RouteCtx {
    /// Compiles `expressions` against the lights' tags. `tags[i]` is light
    /// `i`'s tag.
    ///
    /// The importer compiles a render's expressions as it accepts them, so
    /// this does not fail for an imported scene. A request built by hand
    /// that does not compile is warned about, and its expressions route
    /// nothing.
    pub(crate) fn new(
        expressions: &[String],
        tags: &[Option<&str>],
        albedo: bool,
        diffuse_filter: bool,
    ) -> RouteCtx {
        let lpe = if expressions.is_empty() {
            None
        } else {
            let exprs: Vec<&str> = expressions.iter().map(String::as_str).collect();
            match Lpe::compile(&exprs) {
                Ok(lpe) => Some(lpe),
                Err(e) => {
                    tracing::warn!(
                        "light path expressions do not compile ({e}); their channels stay black"
                    );
                    None
                }
            }
        };
        let sym = |ty, label: LabelId| {
            lpe.as_ref()
                .map_or(0, |l| l.symbol(ty, Scatter::None, label))
        };
        let light = tags
            .iter()
            .map(|t| {
                let label = t
                    .and_then(|t| lpe.as_ref().and_then(|l| l.label(t)))
                    .unwrap_or(0);
                sym(EventType::Light, label)
            })
            .collect();
        let straight = lpe
            .as_ref()
            .map_or(0, |l| l.symbol(EventType::Transmit, Scatter::Straight, 0));
        let thin = lpe.as_ref().map_or(0, |l| {
            l.lobe_symbol(LobeEvent::transmit(
                Scatter::Singular,
                LobeLabel::Transmission,
            ))
        });
        RouteCtx {
            backdrop: sym(EventType::Light, 0),
            object: sym(EventType::Object, 0),
            volume: sym(EventType::Volume, 0),
            straight,
            thin,
            light,
            lpe,
            inspect: false,
            albedo,
            diffuse_filter,
        }
    }

    /// The light path inspector's routing over `lights` lights: no
    /// expression is compiled, and the symbols are the inspection alphabet
    /// ([`lpe::inspection_symbol`]), which names every lobe label, scatter
    /// and event type, so a symbol decodes back to `<RG'coat'>`. Every light
    /// is a plain `L`: which light it was travels beside the symbol, by
    /// light-list index ([`Route`]'s side channel), so the alphabet stays the
    /// same size however many lights the scene has, and a tag no label can
    /// hold costs nothing.
    pub(crate) fn inspection(lights: usize) -> RouteCtx {
        let sym = |ty, scatter, label| lpe::inspection_symbol(ty, scatter, label);
        RouteCtx {
            lpe: None,
            inspect: true,
            light: vec![sym(EventType::Light, Scatter::None, 0); lights],
            backdrop: sym(EventType::Light, Scatter::None, 0),
            object: sym(EventType::Object, Scatter::None, 0),
            volume: sym(EventType::Volume, Scatter::None, 0),
            straight: sym(EventType::Transmit, Scatter::Straight, 0),
            thin: lpe::inspection_lobe(LobeEvent::transmit(
                Scatter::Singular,
                LobeLabel::Transmission,
            )),
            albedo: false,
            diffuse_filter: false,
        }
    }

    /// Whether this is the inspector's routing.
    #[inline]
    pub(crate) fn inspects(&self) -> bool {
        self.inspect
    }

    /// The DFA's size, for the debug log.
    pub(crate) fn describe(&self) -> String {
        self.lpe
            .as_ref()
            .map_or_else(String::new, |l| format!(" ({} DFA states)", l.states()))
    }

    /// Whether the path's events are recorded: any light path expression is
    /// routed, or the path is inspected.
    #[inline]
    pub(crate) fn routes(&self) -> bool {
        self.lpe.is_some() || self.inspect
    }

    pub(crate) fn lobe(&self, e: LobeEvent) -> u16 {
        match &self.lpe {
            Some(l) => l.lobe_symbol(e),
            None => lpe::inspection_lobe(e),
        }
    }

    pub(crate) fn light(&self, index: usize) -> u16 {
        self.light[index]
    }

    pub(crate) fn backdrop(&self) -> u16 {
        self.backdrop
    }

    pub(crate) fn volume(&self) -> u16 {
        self.volume
    }

    /// The emission symbol of a hit on `geom_id` — its light's `L`, or `O`
    /// — and its light-list index ([`NO_LIGHT`] for `O`).
    pub(crate) fn emitter(&self, lights: &crate::LightList, geom_id: u32) -> (u16, u32) {
        match lights.index_of_geom(geom_id) {
            Some(i) => (self.light[i], i as u32),
            None => (self.object, NO_LIGHT),
        }
    }
}

/// The events of the pass-throughs a segment crossed before the vertex it
/// reaches (or before escaping): `ts` cutouts, each a `Ts` — or, when the
/// segment also passed a thin wall, the events in order, a range into
/// `Route::arrivals`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Arrival {
    ts: u16,
    events: (u32, u32),
}

impl Arrival {
    /// `ts` cutouts passed.
    pub(crate) fn cutouts(ts: u16) -> Arrival {
        Arrival { ts, events: (0, 0) }
    }

    /// The first `k` of these events: what a hidden light crossed after `k`
    /// pass-throughs of the segment follows.
    pub(crate) fn prefix(self, k: u16) -> Arrival {
        if self.events.0 == self.events.1 {
            Arrival::cutouts(k)
        } else {
            Arrival {
                ts: 0,
                events: (self.events.0, self.events.0 + k as u32),
            }
        }
    }
}

/// One routed contribution: the event it passes through and its share.
#[derive(Debug, Clone, Copy)]
struct Entry {
    sym: u16,
    value: Vec3A,
}

/// The events of one path vertex.
#[derive(Debug, Clone)]
struct RouteVertex {
    /// Pass-throughs on the segment arriving here: their events come before
    /// this vertex's.
    arrival: Arrival,
    /// The emission counted here (`emit_here`): its light, or `O`, and the
    /// light's index ([`NO_LIGHT`] for `O`).
    emit: u16,
    emit_light: u32,
    /// NEE's share per light sample and lobe, and the bounce's
    /// continuation factor per lobe (ranges into `Route::entries`). Each NEE
    /// entry's light is the entry at the same offset of `Route::nee_lights`.
    nee: (u32, u32),
    nee_lights: (u32, u32),
    bounce: (u32, u32),
    /// The emission the bounce found at the next vertex (`next_emit`), and
    /// its light's index.
    next_emit: u16,
    next_emit_light: u32,
    /// The hidden lights the bounce crossed on its way there (`crossed`):
    /// a range into `Route::crossings`, each its own `L`.
    crossed: (u32, u32),
}

/// A path's routing record, owned by the worker's `PathScratch` like the
/// path records themselves.
#[derive(Debug, Default)]
pub(crate) struct Route {
    /// Whether the path is inspected: the light-list index of every `L`
    /// entry is kept beside its symbol, in the `*_lights` side channel below
    /// and `RouteVertex::*_light`. Set for each path by [`Route::begin`].
    inspect: bool,
    vertices: Vec<RouteVertex>,
    entries: Vec<Entry>,
    /// Per NEE entry, in order, the `L` symbol of the light it sampled.
    nee_lights: Vec<u16>,
    /// Inspected paths only: per light sample, in order, where its entries
    /// end in `entries` and the index of the light it sampled — a vertex's
    /// samples are those ending within its `nee` range.
    nee_samples: Vec<(u32, u32)>,
    /// Pass-through events on the segment after the last vertex.
    terminal_arrival: Arrival,
    /// The ordered pass-through events [`Arrival`]s with thin walls name.
    arrivals: Vec<u16>,
    /// Every hidden light crossed, with its weighted share and the
    /// pass-throughs before it on its segment.
    crossings: Vec<Entry>,
    crossing_arrivals: Vec<Arrival>,
    /// Inspected paths only: per crossing, its light's index.
    crossing_lights: Vec<u32>,
    /// What the path collected beyond its last vertex, when it escaped:
    /// volume emission and transmittance on the final segment, and the
    /// lights at infinity, each with its share (summing, in order, to
    /// `background`).
    escaped: bool,
    terminal_emit: Vec3A,
    terminal_tr: Vec3A,
    background: Vec3A,
    terminal: Vec<Entry>,
    /// Inspected paths only: per `terminal` entry, its light's index
    /// ([`NO_LIGHT`] for a backdrop).
    terminal_lights: Vec<u32>,
    /// The beauty's radiance arriving at the primary vertex, for the
    /// indirect clamp's factor.
    pub(crate) r1: Vec3A,
    /// Per expression, this sample's radiance — what the film accumulates.
    pub(crate) out: Vec<Vec3A>,
    /// The albedo of the first non-delta hit, through the delta chain
    /// before it.
    pub(crate) albedo: Vec3A,
    /// The diffuse filter of the camera ray's first hit, or 0 off a surface:
    /// what raw light AOVs divide by and `diffuse_albedo` reports.
    pub(crate) diffuse_filter: Vec3A,
    albedo_chain: Vec3A,
    albedo_done: bool,
    // Gather scratch: states and radiance per vertex.
    levels: Vec<Vec<u16>>,
    tables: Vec<Vec<Vec3A>>,
    masks: Vec<u64>,
    /// Per NEE entry: where its share's `L` ends.
    nee_masks: Vec<u64>,
    /// Per lobe, then per crossing: where each crossed light's `L` ends.
    cross_masks: Vec<u64>,
    /// The inspection fold's scratch ([`Route::fold`]).
    fold: FoldScratch,
}

impl Route {
    /// Forgets the previous path; `inspect` keeps each `L` entry's light
    /// index for the light path inspector.
    pub(crate) fn begin(&mut self, inspect: bool) {
        self.inspect = inspect;
        self.vertices.clear();
        self.entries.clear();
        self.nee_lights.clear();
        self.nee_samples.clear();
        self.terminal.clear();
        self.terminal_lights.clear();
        self.terminal_arrival = Arrival::default();
        self.arrivals.clear();
        self.crossings.clear();
        self.crossing_arrivals.clear();
        self.crossing_lights.clear();
        self.escaped = false;
        self.r1 = Vec3A::ZERO;
        self.albedo = Vec3A::ONE;
        self.diffuse_filter = Vec3A::ZERO;
        self.albedo_chain = Vec3A::ONE;
        self.albedo_done = false;
    }

    /// The arrival of a segment that passed thin walls: `thin[i]` says
    /// whether its `i`-th pass was a thin wall (`TS`) or a cutout (`Ts`).
    pub(crate) fn arrival_through(&mut self, ctx: &RouteCtx, thin: &[bool]) -> Arrival {
        let start = self.arrivals.len() as u32;
        self.arrivals.extend(
            thin.iter()
                .map(|&t| if t { ctx.thin } else { ctx.straight }),
        );
        Arrival {
            ts: 0,
            events: (start, self.arrivals.len() as u32),
        }
    }

    /// Opens the record of a new vertex, reached through `arrival`.
    pub(crate) fn vertex(&mut self, arrival: Arrival) {
        let at = self.entries.len() as u32;
        let lights = self.nee_lights.len() as u32;
        let crossed = self.crossings.len() as u32;
        self.vertices.push(RouteVertex {
            arrival,
            emit: NO_EVENT,
            emit_light: NO_LIGHT,
            nee: (at, at),
            nee_lights: (lights, lights),
            bounce: (at, at),
            next_emit: NO_EVENT,
            next_emit_light: NO_LIGHT,
            crossed: (crossed, crossed),
        });
    }

    fn current(&mut self) -> &mut RouteVertex {
        self.vertices.last_mut().expect("a vertex is open")
    }

    /// The emission counted at the current vertex: its symbol and its
    /// light's index ([`NO_LIGHT`] for `O`).
    pub(crate) fn emit(&mut self, sym: u16, light: u32) {
        let v = self.current();
        v.emit = sym;
        v.emit_light = light;
    }

    /// One light sample's NEE at the current vertex: the light it sampled
    /// (its `L` symbol and its index) and its share per lobe. Called once
    /// per light sample that contributes, all before any bounce entry; the
    /// samples' entries run on from the vertex's opening.
    pub(crate) fn nee(
        &mut self,
        light: u16,
        index: u32,
        shares: impl Iterator<Item = (u16, Vec3A)>,
    ) {
        let start = self.entries.len() as u32;
        self.entries
            .extend(shares.map(|(sym, value)| Entry { sym, value }));
        let end = self.entries.len() as u32;
        self.nee_lights
            .extend(std::iter::repeat_n(light, (end - start) as usize));
        if self.inspect {
            self.nee_samples.push((end, index));
        }
        let lights = self.nee_lights.len() as u32;
        let v = self.current();
        debug_assert_eq!(v.bounce, (v.nee.1, v.nee.1), "NEE after a bounce entry");
        v.nee.1 = end;
        v.nee_lights.1 = lights;
        v.bounce = (end, end);
    }

    /// The bounce's continuation factor per lobe at the current vertex.
    pub(crate) fn bounce(&mut self, factors: impl Iterator<Item = (u16, Vec3A)>) {
        let start = self.entries.len() as u32;
        self.entries
            .extend(factors.map(|(sym, value)| Entry { sym, value }));
        let end = self.entries.len() as u32;
        self.current().bounce = (start, end);
    }

    /// The emission the last vertex's bounce found, and its light's index.
    pub(crate) fn next_emit(&mut self, sym: u16, light: u32) {
        if let Some(v) = self.vertices.last_mut() {
            v.next_emit = sym;
            v.next_emit_light = light;
        }
    }

    /// A hidden light the last vertex's bounce crossed: its `L` symbol and
    /// index, its share as the beauty's `crossed` adds it, and the
    /// pass-throughs before it. Crossings come in order, before the next
    /// vertex opens.
    pub(crate) fn cross(&mut self, sym: u16, light: u32, value: Vec3A, arrival: Arrival) {
        self.crossings.push(Entry { sym, value });
        self.crossing_arrivals.push(arrival);
        if self.inspect {
            self.crossing_lights.push(light);
        }
        let end = self.crossings.len() as u32;
        if let Some(v) = self.vertices.last_mut() {
            v.crossed.1 = end;
        }
    }

    pub(crate) fn terminal_arrival(&mut self, arrival: Arrival) {
        self.terminal_arrival = arrival;
    }

    /// The path escaped: what the final segment collected — its volume
    /// emission and transmittance, and the beauty's background. The lights
    /// that make up the background follow through `escape_light`.
    pub(crate) fn escape_begin(&mut self, emit: Vec3A, tr: Vec3A, background: Vec3A) {
        self.escaped = true;
        self.terminal_emit = emit;
        self.terminal_tr = tr;
        self.background = background;
    }

    /// Opens the list of the background's lights.
    pub(crate) fn escape_lights_begin(&mut self) {
        self.terminal.clear();
        self.terminal_lights.clear();
    }

    /// One light's share of the background, in `escaped_emission`'s order:
    /// its `L` symbol and index ([`NO_LIGHT`] for a backdrop).
    pub(crate) fn escape_light(&mut self, sym: u16, light: u32, value: Vec3A) {
        self.terminal.push(Entry { sym, value });
        if self.inspect {
            self.terminal_lights.push(light);
        }
    }

    /// The shares' sum, as `escaped_emission` adds them.
    pub(crate) fn escaped_total(&self) -> Vec3A {
        self.terminal
            .iter()
            .fold(Vec3A::ZERO, |acc, e| acc + e.value)
    }

    /// A delta interface on the way to the first non-delta hit: its
    /// throughput scales whatever albedo is found behind it.
    pub(crate) fn albedo_through(&mut self, throughput: Vec3A) {
        if !self.albedo_done {
            self.albedo_chain *= throughput.clamp(Vec3A::ZERO, Vec3A::ONE);
        }
    }

    /// The first non-delta hit's albedo.
    pub(crate) fn albedo_at(&mut self, albedo: Vec3A) {
        if !self.albedo_done {
            self.albedo = (self.albedo_chain * albedo).clamp(Vec3A::ZERO, Vec3A::ONE);
            self.albedo_done = true;
        }
    }

    /// Settles the albedo of a path that found no non-delta hit: what the
    /// delta chain lets through, or 1 with no chain at all.
    pub(crate) fn finish_albedo(&mut self) {
        if !self.albedo_done {
            self.albedo = self.albedo_chain;
            self.albedo_done = true;
        }
    }

    /// Every expression's radiance for this path, into `out`.
    pub(crate) fn gather(
        &mut self,
        ctx: &RouteCtx,
        records: &[VertexRec],
        indirect_clamp: Option<f32>,
    ) {
        let Some(lpe) = &ctx.lpe else {
            return;
        };
        let n = lpe.len();
        self.out.clear();
        self.out.resize(n, Vec3A::ZERO);
        let nv = records.len();
        debug_assert_eq!(nv, self.vertices.len());

        // Forward: the states each vertex can be reached in (before its
        // arrival events), live ones only.
        self.levels.resize_with(nv + 1, Vec::new);
        self.tables.resize_with(nv + 1, Vec::new);
        for l in &mut self.levels[..=nv] {
            l.clear();
        }
        if lpe.live(lpe.start()) {
            self.levels[0].push(lpe.start());
        }
        let arrivals = &self.arrivals;
        let arrive = |mut s: u16, a: Arrival| {
            for _ in 0..a.ts {
                s = lpe.step(s, ctx.straight);
            }
            // Only a segment that passed a thin wall names its events: the
            // common arrival skips the slice (walking an empty one at every
            // arrival cost the gather 1.3% of its instructions).
            if a.events.0 != a.events.1 {
                for &sym in &arrivals[a.events.0 as usize..a.events.1 as usize] {
                    s = lpe.step(s, sym);
                }
            }
            s
        };
        let after = |s: u16, sym: u16| if sym == NO_EVENT { s } else { lpe.step(s, sym) };
        for k in 0..nv {
            let v = &self.vertices[k];
            let (b0, b1) = v.bounce;
            let (head, tail) = self.levels.split_at_mut(k + 1);
            for &s in &head[k] {
                let a = arrive(s, v.arrival);
                for e in &self.entries[b0 as usize..b1 as usize] {
                    let t = after(a, e.sym);
                    if lpe.live(t) && !tail[0].contains(&t) {
                        tail[0].push(t);
                    }
                }
            }
        }

        // The terminal level: what lies beyond the last vertex.
        {
            let level = &self.levels[nv];
            let table = &mut self.tables[nv];
            table.clear();
            table.resize(level.len() * n, Vec3A::ZERO);
            if self.escaped {
                for (si, &s) in level.iter().enumerate() {
                    let a = arrive(s, self.terminal_arrival);
                    let emit_mask = lpe.accepts(lpe.step(a, ctx.object));
                    let masks = &mut self.masks;
                    masks.clear();
                    masks.extend(
                        self.terminal
                            .iter()
                            .map(|e| lpe.accepts(lpe.step(a, e.sym))),
                    );
                    for i in 0..n {
                        let bit = 1u64 << i;
                        let emit = if emit_mask & bit != 0 {
                            self.terminal_emit
                        } else {
                            Vec3A::ZERO
                        };
                        let bg = share(masks, bit, self.background, &self.terminal);
                        table[si * n + i] = emit + self.terminal_tr * bg;
                    }
                }
            }
        }

        // Backward.
        for k in (0..nv).rev() {
            let rec = &records[k];
            let v = &self.vertices[k];
            let ts_next = self
                .vertices
                .get(k + 1)
                .map_or(self.terminal_arrival, |n| n.arrival);
            let nee = &self.entries[v.nee.0 as usize..v.nee.1 as usize];
            let nee_lights = &self.nee_lights[v.nee_lights.0 as usize..v.nee_lights.1 as usize];
            let bounce = &self.entries[v.bounce.0 as usize..v.bounce.1 as usize];
            let crossed = &self.crossings[v.crossed.0 as usize..v.crossed.1 as usize];
            let crossed_at = &self.crossing_arrivals[v.crossed.0 as usize..v.crossed.1 as usize];
            let nc = crossed.len();
            let (head, tail) = self.tables.split_at_mut(k + 1);
            let table = &mut head[k];
            let next_level = &self.levels[k + 1];
            let next_table = &tail[0];
            let level = &self.levels[k];
            table.clear();
            table.resize(level.len() * n, Vec3A::ZERO);
            let clamp = indirect_clamp.filter(|_| k == 0 && nv > 1);
            let ne = rec.next_emit * rec.next_emit_weight;
            for (si, &s) in level.iter().enumerate() {
                let a = arrive(s, v.arrival);
                let seg_mask = lpe.accepts(lpe.step(a, ctx.object));
                let emit_mask = if v.emit == NO_EVENT {
                    0
                } else {
                    lpe.accepts(lpe.step(a, v.emit))
                };
                // Per NEE entry (a light sample's lobe share), where it ends;
                // per lobe (at most `MAX_SPLIT`, so on the stack): the state
                // the bounce leads to, its row in the next vertex's table, and
                // where the next emission ends.
                let nee_masks = &mut self.nee_masks;
                nee_masks.clear();
                nee_masks.extend(nee.iter().zip(nee_lights).map(|(e, &light)| {
                    if light == NO_EVENT {
                        0
                    } else {
                        lpe.accepts(lpe.step(after(a, e.sym), light))
                    }
                }));
                let nee_masks = &*nee_masks;
                let mut targets = [0u16; MAX_SPLIT];
                let mut rows = [None; MAX_SPLIT];
                let mut ne_masks = [0u64; MAX_SPLIT];
                let cross_masks = &mut self.cross_masks;
                cross_masks.clear();
                for (j, e) in bounce.iter().enumerate() {
                    let t = after(a, e.sym);
                    targets[j] = t;
                    rows[j] = next_level.iter().position(|x| *x == t);
                    if v.next_emit != NO_EVENT {
                        ne_masks[j] = lpe.accepts(lpe.step(arrive(t, ts_next), v.next_emit));
                    }
                    cross_masks.extend(
                        crossed
                            .iter()
                            .zip(crossed_at)
                            .map(|(c, &at)| lpe.accepts(lpe.step(arrive(t, at), c.sym))),
                    );
                }
                let cross_masks = &*cross_masks;
                let targets = &targets[..bounce.len()];
                // Expressions this state can no longer accept get nothing
                // from here on: their entries stay zero.
                let live = lpe.live_mask(s);
                for i in 0..n {
                    if live & (1u64 << i) == 0 {
                        continue;
                    }
                    // The lobes agree, for this expression: they lead to
                    // states it cannot tell apart, so the beauty's totals.
                    let agree = targets
                        .windows(2)
                        .all(|w| lpe.class(i, w[0]) == lpe.class(i, w[1]));
                    let bit = 1u64 << i;
                    let pick = |mask: u64, x: Vec3A| if mask & bit != 0 { x } else { Vec3A::ZERO };
                    let r_next = |j: usize| rows[j].map_or(Vec3A::ZERO, |r| next_table[r * n + i]);
                    // The hidden lights the lobe's ray crossed: the beauty's
                    // `crossed` when they all agree, zero with none.
                    let cross = |j: usize| {
                        share(
                            &cross_masks[j * nc..(j + 1) * nc],
                            bit,
                            rec.crossed,
                            crossed,
                        )
                    };
                    let seg = pick(seg_mask, rec.segment_emit);
                    let emit = pick(emit_mask, rec.emit_here);
                    let nee_i = share(nee_masks, bit, rec.nee, nee);
                    // The bounce, split into what reaches the next vertex's
                    // emission (direct) and what continues past it.
                    let (direct, onward) = if bounce.is_empty() {
                        (Vec3A::ZERO, Vec3A::ZERO)
                    } else if agree {
                        (pick(ne_masks[0], ne) + cross(0), r_next(0))
                    } else {
                        (Vec3A::ZERO, Vec3A::ZERO)
                    };
                    table[si * n + i] = match clamp {
                        None => {
                            let bounce_i = if bounce.is_empty() || agree {
                                rec.factor * (direct + onward)
                            } else {
                                bounce.iter().enumerate().fold(Vec3A::ZERO, |acc, (j, e)| {
                                    acc + e.value * (pick(ne_masks[j], ne) + cross(j) + r_next(j))
                                })
                            };
                            seg + rec.atten * (emit + nee_i + bounce_i)
                        }
                        Some(limit) => {
                            // As the beauty: direct light whole, the
                            // continuation scaled by the beauty's own clamp
                            // factor, so a partition still sums.
                            let (d, ind) = if bounce.is_empty() || agree {
                                (rec.factor * direct, rec.factor * onward)
                            } else {
                                bounce.iter().enumerate().fold(
                                    (Vec3A::ZERO, Vec3A::ZERO),
                                    |(d, o), (j, e)| {
                                        (
                                            d + e.value * (pick(ne_masks[j], ne) + cross(j)),
                                            o + e.value * r_next(j),
                                        )
                                    },
                                )
                            };
                            let direct = seg + rec.atten * (emit + nee_i + d);
                            let indirect = rec.atten * ind;
                            let beauty = rec.atten * (rec.factor * self.r1);
                            let peak = beauty.max_element();
                            direct
                                + if peak > limit && peak.is_finite() {
                                    indirect * (limit / peak)
                                } else {
                                    indirect
                                }
                        }
                    };
                }
            }
        }
        if !self.levels[0].is_empty() {
            self.out.copy_from_slice(&self.tables[0][..n]);
        }
    }
}

/// One light path of an inspected camera sample ([`Route::fold`]): one
/// emission-ending term of the beauty's recurrence, with the events that
/// lead to it.
pub struct PathTerm<'a> {
    /// The events after `C`, in the inspection alphabet: each vertex's
    /// arrival pass-throughs and its dominant lobe, then the emission that
    /// ends the path (`L` or `O`).
    pub(crate) events: &'a [u16],
    /// Per event, its lobe split: a range into `shares`, empty for a
    /// pass-through and for the emission.
    pub(crate) splits: &'a [(u32, u32)],
    /// Each event of a scattering vertex with its share of the vertex's
    /// luminance toward the path's direction, lobes of one event merged.
    pub(crate) shares: &'a [(u16, f32)],
    /// The index of the light that ends it in the light list, or
    /// [`NO_LIGHT`] (`O`, a backdrop).
    pub(crate) light: u32,
    /// Its contribution to the sample's radiance, and the same after the
    /// indirect clamp.
    pub(crate) value: Vec3A,
    pub(crate) clamped: Vec3A,
    /// The smallest dominant share along the path: 1 when every vertex's
    /// label carries all of it.
    pub(crate) min_share: f32,
}

/// [`Route::fold`]'s buffers, kept with the route so a sample allocates
/// nothing once they have grown.
#[derive(Debug, Default)]
struct FoldScratch {
    events: Vec<u16>,
    splits: Vec<(u32, u32)>,
    shares: Vec<(u16, f32)>,
}

impl FoldScratch {
    fn mark(&self) -> (usize, usize, usize) {
        (self.events.len(), self.splits.len(), self.shares.len())
    }

    fn reset(&mut self, (events, splits, shares): (usize, usize, usize)) {
        self.events.truncate(events);
        self.splits.truncate(splits);
        self.shares.truncate(shares);
    }

    /// A pass-through: an event with no split.
    fn pass(&mut self, sym: u16) {
        let n = self.shares.len() as u32;
        self.events.push(sym);
        self.splits.push((n, n));
    }

    /// The pass-throughs of `a`, in order.
    fn arrival(&mut self, a: Arrival, arrivals: &[u16], straight: u16) {
        for _ in 0..a.ts {
            self.pass(straight);
        }
        for &sym in &arrivals[a.events.0 as usize..a.events.1 as usize] {
            self.pass(sym);
        }
    }

    /// A scattering vertex whose value toward the path's direction is
    /// `entries`, one per lobe: its events' luminance shares (lobes of one
    /// event summed), and the dominant event as the vertex's own. Returns
    /// the dominant share — 1, and nothing pushed, for a vertex with no
    /// event (a random walk's exit).
    fn vertex(&mut self, entries: &[Entry], luma: utils::Luma) -> f32 {
        let start = self.shares.len();
        let mut total = 0.0f32;
        for e in entries {
            // A negative (out-of-gamut) or NaN luminance carries no share.
            let l = luma.of(e.value).max(0.0);
            total += l;
            match self.shares[start..].iter_mut().find(|(s, _)| *s == e.sym) {
                Some(x) => x.1 += l,
                None => self.shares.push((e.sym, l)),
            }
        }
        let n = (self.shares.len() - start) as f32;
        for x in &mut self.shares[start..] {
            x.1 = if total > 0.0 && total.is_finite() {
                x.1 / total
            } else {
                1.0 / n
            };
        }
        let (event, share) =
            self.shares[start..]
                .iter()
                .fold(
                    (NO_EVENT, -1.0f32),
                    |best, &(s, x)| {
                        if x > best.1 { (s, x) } else { best }
                    },
                );
        if event == NO_EVENT {
            self.shares.truncate(start);
            return 1.0;
        }
        self.events.push(event);
        self.splits.push((start as u32, self.shares.len() as u32));
        share
    }

    /// The term ending in `sym`: handed to `f` unless it carries nothing.
    #[allow(clippy::too_many_arguments)]
    fn term(
        &mut self,
        sym: u16,
        light: u32,
        value: Vec3A,
        clamp: f32,
        min_share: f32,
        f: &mut impl FnMut(&PathTerm),
    ) {
        if value == Vec3A::ZERO {
            return;
        }
        self.pass(sym);
        f(&PathTerm {
            events: &self.events,
            splits: &self.splits,
            shares: &self.shares,
            light,
            value,
            clamped: if clamp == 1.0 { value } else { value * clamp },
            min_share,
        });
        self.events.pop();
        self.splits.pop();
    }
}

impl Route {
    /// The light path inspector's view of the path just traced: every term
    /// of the beauty's recurrence that ends on emission, handed to `f` in
    /// path order (the light path inspection design record, D1):
    ///
    /// | term | ends with |
    /// |------|-----------|
    /// | `segment_emit` (volume emission) | `O` after the vertex's arrival |
    /// | `emit_here` | the emitter's `L` or `O` |
    /// | each NEE light sample | that light's `L` |
    /// | `next_emit` of a bounce | the hit emitter's `L` or `O` |
    /// | each crossing | the crossed light's `L` |
    /// | the escaped segment's emission, each light at infinity | `O`, `L` |
    ///
    /// A term's value is the prefix — each earlier vertex's `atten` and
    /// whole `factor` — times the term with its MIS weight, as the gather
    /// builds the beauty, so a sample's terms sum to its radiance up to
    /// associativity. Each vertex is labelled by its dominant event toward
    /// the term's direction (the NEE sample's for an NEE term, the bounce's
    /// otherwise); the labels change no value. A term past the primary
    /// vertex is also given clamped, by the factor the beauty's indirect
    /// clamp applied to the sample (`indirect_clamp`, the gather's).
    ///
    /// Inspected paths only (`begin(true)`, the inspection alphabet).
    pub(crate) fn fold(
        &mut self,
        ctx: &RouteCtx,
        records: &[VertexRec],
        indirect_clamp: Option<f32>,
        luma: utils::Luma,
        mut f: impl FnMut(&PathTerm),
    ) {
        debug_assert!(
            self.inspect,
            "a path folded without the inspection's side channel"
        );
        let Route {
            vertices,
            entries,
            nee_lights,
            nee_samples,
            terminal_arrival,
            arrivals,
            crossings,
            crossing_arrivals,
            crossing_lights,
            escaped,
            terminal_emit,
            terminal_tr,
            terminal,
            terminal_lights,
            r1,
            fold,
            ..
        } = self;
        let nv = records.len();
        debug_assert_eq!(nv, vertices.len());
        // The beauty's clamp factor on everything past the primary vertex.
        let clamp = match indirect_clamp {
            Some(limit) if nv > 1 => {
                let peak = (records[0].atten * (records[0].factor * *r1)).max_element();
                if peak > limit && peak.is_finite() {
                    limit / peak
                } else {
                    1.0
                }
            }
            _ => 1.0,
        };
        fold.reset((0, 0, 0));
        let mut prefix = Vec3A::ONE;
        let mut prefix_share = 1.0f32;
        let mut sample = 0;
        for (k, (rec, v)) in records.iter().zip(vertices.iter()).enumerate() {
            let c = if k == 0 { 1.0 } else { clamp };
            fold.arrival(v.arrival, arrivals, ctx.straight);
            let arrived = fold.mark();
            fold.term(
                ctx.object,
                NO_LIGHT,
                prefix * rec.segment_emit,
                c,
                prefix_share,
                &mut f,
            );
            let pa = prefix * rec.atten;
            if v.emit != NO_EVENT {
                fold.term(
                    v.emit,
                    v.emit_light,
                    pa * rec.emit_here,
                    c,
                    prefix_share,
                    &mut f,
                );
            }
            // Each light sample: the entries up to where it ends.
            let mut start = v.nee.0;
            while let Some(&(end, light)) = nee_samples.get(sample)
                && end <= v.nee.1
            {
                sample += 1;
                let shares = &entries[start as usize..end as usize];
                let symbol = nee_lights.get((v.nee_lights.0 + (start - v.nee.0)) as usize);
                start = end;
                let Some(&symbol) = symbol.filter(|_| !shares.is_empty()) else {
                    continue;
                };
                let value = pa * shares.iter().fold(Vec3A::ZERO, |acc, e| acc + e.value);
                let share = fold.vertex(shares, luma);
                fold.term(symbol, light, value, c, prefix_share.min(share), &mut f);
                fold.reset(arrived);
            }
            let bounce = &entries[v.bounce.0 as usize..v.bounce.1 as usize];
            if bounce.is_empty() {
                break;
            }
            let through = prefix_share.min(fold.vertex(bounce, luma));
            let bounced = fold.mark();
            if v.next_emit != NO_EVENT {
                let ts_next = vertices.get(k + 1).map_or(*terminal_arrival, |n| n.arrival);
                fold.arrival(ts_next, arrivals, ctx.straight);
                fold.term(
                    v.next_emit,
                    v.next_emit_light,
                    pa * (rec.factor * (rec.next_emit * rec.next_emit_weight)),
                    c,
                    through,
                    &mut f,
                );
                fold.reset(bounced);
            }
            for i in v.crossed.0 as usize..v.crossed.1 as usize {
                fold.arrival(crossing_arrivals[i], arrivals, ctx.straight);
                fold.term(
                    crossings[i].sym,
                    crossing_lights.get(i).copied().unwrap_or(NO_LIGHT),
                    pa * (rec.factor * crossings[i].value),
                    c,
                    through,
                    &mut f,
                );
                fold.reset(bounced);
            }
            prefix = pa * rec.factor;
            prefix_share = through;
        }
        if *escaped {
            fold.arrival(*terminal_arrival, arrivals, ctx.straight);
            fold.term(
                ctx.object,
                NO_LIGHT,
                prefix * *terminal_emit,
                clamp,
                prefix_share,
                &mut f,
            );
            for (i, t) in terminal.iter().enumerate() {
                fold.term(
                    t.sym,
                    terminal_lights.get(i).copied().unwrap_or(NO_LIGHT),
                    prefix * (*terminal_tr * t.value),
                    clamp,
                    prefix_share,
                    &mut f,
                );
            }
        }
    }
}

/// A sum routed by acceptance: when every entry's mask agrees on `bit`, the
/// beauty's own `total` (or nothing); otherwise the entries that accept.
fn share(masks: &[u64], bit: u64, total: Vec3A, entries: &[Entry]) -> Vec3A {
    let Some(first) = masks.first() else {
        return Vec3A::ZERO;
    };
    let on = first & bit != 0;
    if masks.iter().all(|m| (m & bit != 0) == on) {
        if on { total } else { Vec3A::ZERO }
    } else {
        masks
            .iter()
            .zip(entries)
            .filter(|(m, _)| *m & bit != 0)
            .fold(Vec3A::ZERO, |acc, (_, e)| acc + e.value)
    }
}
