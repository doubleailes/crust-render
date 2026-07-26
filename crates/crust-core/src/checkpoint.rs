//! Checkpoint/resume state — the engine side of resumable rendering, after
//! MoonRay's resumable-EXR workflow (see `docs/moonray_comparison.md`).
//!
//! crust-core stays free of image-encoding dependencies: the engine hands
//! plain accumulation data to a [`CheckpointCallback`] at pass boundaries,
//! and accepts the same data back to resume. Serialization (the resumable
//! EXR) is the CLI's concern.
//!
//! Resume is **bit-exact** when the resumed render uses the same settings
//! (the [`fingerprint`](crate::RenderSettings::fingerprint) guards this)
//! and total sample budget: the Sobol sampler is stateless per
//! `(pixel, seed, sample index)`, the pass schedule is a pure function the
//! resumed run re-enters at the checkpoint's boundary, and raw f32 sums —
//! not means — cross the boundary. Extending the budget on resume
//! (a larger `-s`) is also exact *for the shared prefix of passes*, though
//! the final image then differs from a straight run at the larger budget
//! only in float-association at the bridged pass boundary.

use glam::Vec3A;

/// Version of the engine-side checkpoint contract. Bump when the state's
/// meaning changes; serializers should refuse to load a different version.
pub const CHECKPOINT_VERSION: u32 = 1;

/// A film snapshot at a uniform pass boundary, plus what resume needs to
/// validate and re-enter the schedule. All buffers are row-major
/// `width × height`.
#[derive(Debug, Clone)]
pub struct CheckpointState {
    pub width: usize,
    pub height: usize,
    /// Raw radiance sums (not means — means don't round-trip exactly).
    pub sum: Vec<Vec3A>,
    /// Sums over the odd-indexed samples (the adaptive split buffer).
    pub odd_sum: Vec<Vec3A>,
    /// Per-pixel sample counts. Uniform within a tile, but not across
    /// tiles: adaptively completed tiles freeze below `next_sample`.
    pub count: Vec<u32>,
    /// The uniform per-pixel sample boundary the snapshot was taken at —
    /// where the resumed schedule re-enters.
    pub next_sample: u32,
    /// [`crate::RenderSettings::fingerprint`] of the producing render.
    pub fingerprint: u64,
}

impl CheckpointState {
    /// Structural sanity: buffer lengths match the declared dimensions.
    pub fn is_consistent(&self) -> bool {
        let n = self.width * self.height;
        self.sum.len() == n && self.odd_sum.len() == n && self.count.len() == n
    }
}

/// Called on the driver thread at eligible pass boundaries once the
/// checkpoint interval has elapsed. The state is fully materialized before
/// the call — writing it out concurrently with the next pass is the
/// callback's own choice.
pub type CheckpointCallback<'a> = &'a (dyn Fn(&CheckpointState) + Sync);
