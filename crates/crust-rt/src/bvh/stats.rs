use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! {
    /// Instance-nesting depth of the traversal running on this thread.
    /// A top-level query is depth 0; descending into an instanced
    /// scene raises it, so work can be attributed to the tree it
    /// happened in rather than lumped together.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Counters are `[top-level, inside an instance]`.
pub static QUERIES: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
pub static NODES_VISITED: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
pub static LEAVES_VISITED: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
pub static PRIM_TESTS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
pub static PACKET_TESTS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

/// Deepest the traversal stack got, over every query — the measurement
/// that sizes [`super::STACK_INLINE`]. Not a pair: stack depth is a
/// property of one tree, and the interesting number is the maximum over
/// all of them, so both levels share it.
pub static STACK_HIGH_WATER: AtomicU64 = AtomicU64::new(0);

#[inline]
pub fn note_stack_depth(depth: usize) {
    STACK_HIGH_WATER.fetch_max(depth as u64, Ordering::Relaxed);
}

pub fn stack_high_water() -> u64 {
    STACK_HIGH_WATER.load(Ordering::Relaxed)
}

/// What [`stack_high_water`] has to stay under to avoid the heap.
pub fn stack_inline_capacity() -> usize {
    super::STACK_INLINE
}

/// Which half of each counter pair the current traversal belongs to.
#[inline]
pub fn slot() -> usize {
    DEPTH.with(|d| usize::from(d.get() > 0))
}

/// Bracket a descent into an instanced scene.
#[inline]
pub fn enter_instance() {
    DEPTH.with(|d| d.set(d.get() + 1));
}

#[inline]
pub fn leave_instance() {
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
}

#[inline]
pub fn bump(c: &[AtomicU64; 2], n: u64) {
    c[slot()].fetch_add(n, Ordering::Relaxed);
}

/// One tree level's totals: `(queries, nodes, leaves, packets, scalars)`.
pub fn read_level(level: usize) -> (u64, u64, u64, u64, u64) {
    let g = |c: &[AtomicU64; 2]| c[level].load(Ordering::Relaxed);
    (
        g(&QUERIES),
        g(&NODES_VISITED),
        g(&LEAVES_VISITED),
        g(&PACKET_TESTS),
        g(&PRIM_TESTS),
    )
}

type DescentMap = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<u32, u64>>>;

/// Every thread's descent map, so [`top_level_descents`] can merge them.
static DESCENT_MAPS: std::sync::Mutex<Vec<DescentMap>> = std::sync::Mutex::new(Vec::new());

thread_local! {
    /// Descents into each *top-level* instance, by `geom_id`, on this
    /// thread. Per thread (the mutex is only ever contended by the final
    /// merge) because a scene whose top level will not cull descends
    /// tens of thousands of times per ray.
    static DESCENTS: DescentMap = {
        let m = DescentMap::default();
        DESCENT_MAPS.lock().unwrap().push(m.clone());
        m
    };
}

/// Record a descent into the instance `geom_id`, if it is a top-level
/// one. Call before [`enter_instance`].
#[inline]
pub fn note_descent(geom_id: u32) {
    if slot() == 0 {
        DESCENTS.with(|m| *m.lock().unwrap().entry(geom_id).or_insert(0) += 1);
    }
}

/// Descents per top-level instance over every thread, most first.
pub fn top_level_descents() -> Vec<(u32, u64)> {
    let mut all = std::collections::HashMap::<u32, u64>::new();
    for m in DESCENT_MAPS.lock().unwrap().iter() {
        for (&k, &v) in m.lock().unwrap().iter() {
            *all.entry(k).or_insert(0) += v;
        }
    }
    let mut v: Vec<_> = all.into_iter().collect();
    v.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

pub fn reset() {
    for m in DESCENT_MAPS.lock().unwrap().iter() {
        m.lock().unwrap().clear();
    }
    for c in [
        &QUERIES,
        &NODES_VISITED,
        &LEAVES_VISITED,
        &PACKET_TESTS,
        &PRIM_TESTS,
    ] {
        for half in c {
            half.store(0, Ordering::Relaxed);
        }
    }
    STACK_HIGH_WATER.store(0, Ordering::Relaxed);
}
