//! Sampler abstraction for the crust-render path tracer.
//!
//! Provides a `Sampler` trait plus two implementations:
//!
//! - `SobolSampler` — Owen-scrambled Sobol (Burley 2020) via `sobol_burley`.
//!   Per-pixel decorrelated by hashing `(x, y, frame_seed)` into the Sobol
//!   scramble seed. This is the production sampler.
//! - `RngSampler` — thread-local `SmallRng` fallback, used in tests and as a
//!   safety net when the sample/dimension budget overflows.
//!
//! The trait is designed so call sites never see which implementation they
//! got — they just request `next_1d`, `next_2d`, or call `advance_bounce`
//! at the top of each bounce.

use rand::{Rng, SeedableRng, rngs::SmallRng};

pub trait Sampler: Send {
    /// Prepare the sampler for a new pixel. Derives a per-pixel decorrelation
    /// seed so neighbouring pixels get statistically independent sequences
    /// (and their residual error is blue-noise-like across the image).
    fn start_pixel(&mut self, x: u32, y: u32);

    /// Prepare the sampler for a new sample within the current pixel. Resets
    /// the per-sample dimension counter to zero.
    fn start_sample(&mut self, sample_index: u32);

    /// Return the next one-dimensional sample in `[0, 1)`.
    fn next_1d(&mut self) -> f32;

    /// Return the next two-dimensional sample, each component in `[0, 1)`.
    /// Consumes two consecutive dimensions of the underlying sequence.
    fn next_2d(&mut self) -> [f32; 2];

    /// Called at the top of each bounce. A no-op for the plain-Sobol sampler,
    /// but a hook for a future padded-Sobol scheme that reseeds per bounce
    /// once the total-dimension budget outgrows `sobol_burley`'s 256-dim
    /// window.
    fn advance_bounce(&mut self);
}

// ---------------------------------------------------------------------------
// Sobol (Burley) sampler
// ---------------------------------------------------------------------------

/// Owen-scrambled Sobol sampler. State is the per-pixel decorrelation seed,
/// the current sample index within that pixel, and the next Sobol dimension
/// to consume within the current sample.
pub struct SobolSampler {
    frame_seed: u32,
    pixel_seed: u32,
    sample_idx: u32,
    dim: u32,
}

impl SobolSampler {
    /// Build a sampler seeded by `frame_seed`. Different frames of an
    /// animation should use different seeds so per-pixel sequences are not
    /// identical across frames — a hash of the frame number works.
    pub fn new(frame_seed: u32) -> Self {
        Self {
            frame_seed,
            pixel_seed: 0,
            sample_idx: 0,
            dim: 0,
        }
    }
}

impl Default for SobolSampler {
    fn default() -> Self {
        Self::new(0)
    }
}

/// PCG-style 32-bit hash used to derive a per-pixel Sobol scramble seed from
/// `(x, y, frame_seed)`. Cheap, avalanches well, and stays reproducible.
#[inline]
fn hash_pixel(x: u32, y: u32, frame_seed: u32) -> u32 {
    let mut h = x.wrapping_mul(0x9E37_79B1);
    h ^= y.wrapping_mul(0x8508_9CE7);
    h ^= frame_seed.wrapping_mul(0xC2B2_AE35);
    h = (h ^ (h >> 16)).wrapping_mul(0x85EB_CA6B);
    h = (h ^ (h >> 13)).wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 16)
}

impl Sampler for SobolSampler {
    fn start_pixel(&mut self, x: u32, y: u32) {
        self.pixel_seed = hash_pixel(x, y, self.frame_seed);
        self.sample_idx = 0;
        self.dim = 0;
    }

    fn start_sample(&mut self, sample_index: u32) {
        self.sample_idx = sample_index;
        self.dim = 0;
    }

    fn next_1d(&mut self) -> f32 {
        let d = self.dim;
        self.dim = self.dim.wrapping_add(1);
        // sobol_burley caps at `NUM_DIMENSIONS` (256). Wrap gracefully once
        // we exceed that — very high dimensions get low-discrepancy-lite
        // behaviour rather than a panic; callers should not exceed the
        // budget in normal renders, but shallow-bounce heavy paths can.
        let dim = d % sobol_burley::NUM_DIMENSIONS as u32;
        sobol_burley::sample(self.sample_idx, dim, self.pixel_seed)
    }

    fn next_2d(&mut self) -> [f32; 2] {
        [self.next_1d(), self.next_1d()]
    }

    fn advance_bounce(&mut self) {
        // No-op today; kept in the trait so a padded-Sobol replacement can
        // slot in without changing call sites.
    }
}

// ---------------------------------------------------------------------------
// RNG fallback sampler
// ---------------------------------------------------------------------------

/// Uniform-random sampler backed by `SmallRng`. Not low-discrepancy — used
/// only by tests, benches, and as a safety net when a caller can't be given
/// a deterministic per-pixel seed.
pub struct RngSampler(SmallRng);

impl RngSampler {
    pub fn from_seed(seed: u64) -> Self {
        Self(SmallRng::seed_from_u64(seed))
    }
}

impl Default for RngSampler {
    fn default() -> Self {
        Self::from_seed(0xC0FF_EE00_BADD_C0DE)
    }
}

impl Sampler for RngSampler {
    fn start_pixel(&mut self, x: u32, y: u32) {
        // Reseed off (x, y) so behaviour matches Sobol's per-pixel
        // decorrelation contract; useful when swapping samplers under a
        // single call site.
        let s = ((x as u64) << 32) | y as u64;
        self.0 = SmallRng::seed_from_u64(s ^ 0xA5A5_A5A5_5A5A_5A5A);
    }

    fn start_sample(&mut self, _sample_index: u32) {}

    fn next_1d(&mut self) -> f32 {
        self.0.random::<f32>()
    }

    fn next_2d(&mut self) -> [f32; 2] {
        [self.0.random::<f32>(), self.0.random::<f32>()]
    }

    fn advance_bounce(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sobol_is_deterministic_per_pixel() {
        let mut a = SobolSampler::new(0);
        let mut b = SobolSampler::new(0);
        a.start_pixel(17, 42);
        b.start_pixel(17, 42);
        a.start_sample(3);
        b.start_sample(3);
        for _ in 0..8 {
            assert_eq!(a.next_1d(), b.next_1d());
        }
    }

    #[test]
    fn sobol_decorrelates_neighbouring_pixels() {
        let mut a = SobolSampler::new(0);
        let mut b = SobolSampler::new(0);
        a.start_pixel(17, 42);
        b.start_pixel(18, 42);
        a.start_sample(0);
        b.start_sample(0);
        // Different pixels must produce different sequences (probability of
        // 4 consecutive matches by chance is ~0).
        let matches = (0..4).all(|_| a.next_1d() == b.next_1d());
        assert!(!matches, "neighbouring pixels produced identical Sobol streams");
    }

    #[test]
    fn samples_in_unit_interval() {
        let mut s = SobolSampler::new(0);
        s.start_pixel(0, 0);
        s.start_sample(0);
        for _ in 0..64 {
            let v = s.next_1d();
            assert!((0.0..1.0).contains(&v), "sample {v} out of [0, 1)");
        }
    }
}
