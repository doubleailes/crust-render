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

pub fn reset() {
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
