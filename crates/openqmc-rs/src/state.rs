// Port of oqmc/state.h — the generic 8-byte sampler state shared by all sampler
// implementations. Small enough to pass by value; carries the pattern/sample/
// pixel identifiers and the domain-tree mutations.

use crate::encode::{encode_bits16, EncodeKey};
use crate::pcg;

/// 2^16 sample-index upper limit (bit size).
pub const MAX_INDEX_BIT_SIZE: u32 = 16;
/// 2^16 sample-index upper limit.
pub const MAX_INDEX_SIZE: usize = 1 << 16;
/// Pixel-x encoding precision (256 pixels).
pub const SPATIAL_ENCODE_BITS_X: u32 = 8;
/// Pixel-y encoding precision (256 pixels).
pub const SPATIAL_ENCODE_BITS_Y: u32 = 8;

/// Top 16 bits of a sample index (the domain key for indices >= 2^16).
#[inline]
pub const fn compute_index_key(index: i32) -> i32 {
    index >> MAX_INDEX_BIT_SIZE
}

/// Bottom 16 bits of a sample index.
#[inline]
pub const fn compute_index_id(index: i32) -> i32 {
    index & (MAX_INDEX_SIZE as i32 - 1)
}

/// Generic sampler state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State64Bit {
    /// Identifier for the domain pattern.
    pub pattern_id: u32,
    /// Identifier for the sample index.
    pub sample_id: u16,
    /// Identifier for the pixel position.
    pub pixel_id: u16,
}

impl State64Bit {
    /// Construct from pixel, frame and sample indices. Pixels are correlated by
    /// default; call [`pixel_decorrelate`](Self::pixel_decorrelate) to separate
    /// them.
    #[inline]
    pub fn new(x: i32, y: i32, frame: i32, index: i32) -> Self {
        debug_assert!(index >= 0);

        let index_key = compute_index_key(index);
        let index_id = compute_index_id(index);

        let pixel_id =
            encode_bits16::<SPATIAL_ENCODE_BITS_X, SPATIAL_ENCODE_BITS_Y, 0>(EncodeKey {
                x,
                y,
                z: 0,
            });

        Self {
            pattern_id: pcg::init_seed((frame + index_key) as u32),
            sample_id: index_id as u16,
            pixel_id,
        }
    }

    /// Decorrelate the state between pixels using the pixel identifier.
    #[inline]
    pub fn pixel_decorrelate(&self) -> Self {
        self.new_domain(self.pixel_id as i32)
    }

    /// Derive a child domain with an independent 4D pattern.
    #[inline]
    pub fn new_domain(&self, key: i32) -> Self {
        let mut ret = *self;
        ret.pattern_id = pcg::state_transition(self.pattern_id.wrapping_add(key as u32));
        ret
    }

    /// Derive a split domain with a fixed sample-rate multiplier (`size`).
    #[inline]
    pub fn new_domain_split(&self, key: i32, size: i32, index: i32) -> Self {
        debug_assert!(size > 0 && index >= 0);

        let combined = self.sample_id as i32 * size + index;
        let index_key = compute_index_key(combined);
        let index_id = compute_index_id(combined);

        let mut ret = self.new_domain(key).new_domain(index_key);
        ret.sample_id = index_id as u16;
        ret
    }

    /// Derive a split domain with an adaptive (local) distribution.
    #[inline]
    pub fn new_domain_distrib(&self, key: i32, index: i32) -> Self {
        debug_assert!(index >= 0);

        let index_key = compute_index_key(index);
        let index_id = compute_index_id(index);

        let mut ret = self
            .new_domain(key)
            .new_domain(index_key)
            .new_domain(self.sample_id as i32);
        ret.sample_id = index_id as u16;
        ret
    }

    /// Draw `N` low-quality pseudo-random values seeded from this state.
    #[inline]
    pub fn draw_rnd<const N: usize>(&self) -> [u32; N] {
        let mut rng_state = self.pattern_id.wrapping_add(self.sample_id as u32);
        let mut rnd = [0u32; N];
        let mut i = 0;
        while i < N {
            rnd[i] = pcg::rng(&mut rng_state);
            i += 1;
        }
        rnd
    }

    /// A sequential PRNG stream seeded from this state (the same seed
    /// `pattern_id + sample_id` used by [`draw_rnd`](Self::draw_rnd)). Use it for
    /// unbounded random streams — Russian roulette, delta tracking.
    #[inline]
    pub fn rng(&self) -> pcg::Rng {
        pcg::Rng::new(self.pattern_id.wrapping_add(self.sample_id as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: i32 = 2;
    const INDEX: i32 = 3;
    const PIXEL_X: i32 = 5;
    const PIXEL_Y: i32 = 7;
    const PRIMES: [i32; 20] = [
        2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71,
    ];

    fn default_state() -> State64Bit {
        State64Bit::new(PIXEL_X, PIXEL_Y, FRAME, INDEX)
    }

    #[test]
    fn is_eight_bytes() {
        assert_eq!(std::mem::size_of::<State64Bit>(), 8);
    }

    // Mirrors state.cpp:AlterSample.
    #[test]
    fn alter_sample() {
        let d = default_state();
        for prime in PRIMES {
            let s = State64Bit::new(PIXEL_X, PIXEL_Y, FRAME, prime);
            assert_eq!(s.pattern_id, d.pattern_id);
            assert_eq!(s.pixel_id, d.pixel_id);
            if prime == INDEX {
                assert_eq!(s.sample_id, d.sample_id);
            } else {
                assert_ne!(s.sample_id, d.sample_id);
            }
        }

        let a = State64Bit::new(PIXEL_X, PIXEL_Y, FRAME, MAX_INDEX_SIZE as i32);
        let b = State64Bit::new(PIXEL_X, PIXEL_Y, FRAME, MAX_INDEX_SIZE as i32 - 1);
        assert_eq!(a.sample_id, 0);
        assert_eq!(b.sample_id, (MAX_INDEX_SIZE - 1) as u16);
        assert_ne!(a.pattern_id, b.pattern_id);
    }

    // Mirrors state.cpp:AlterPixel — changing pixel changes only pixel_id, and
    // decorrelation makes pattern_id distinct.
    #[test]
    fn alter_pixel() {
        let base = State64Bit::new(0, 0, 0, INDEX);
        let base_dec = base.pixel_decorrelate();
        for x in 1..11 {
            let s = State64Bit::new(x, 0, 0, INDEX);
            assert_eq!(s.pattern_id, base.pattern_id);
            assert_eq!(s.sample_id, base.sample_id);
            assert_ne!(s.pixel_id, base.pixel_id);
            assert_ne!(s.pixel_decorrelate().pattern_id, base_dec.pattern_id);
        }
    }

    // Mirrors state.cpp:NewDomain — the three domain kinds produce distinct,
    // non-colliding pattern ids while preserving the right sample/pixel ids.
    #[test]
    fn new_domain_distinct() {
        let d = default_state();
        let mut seen_state = Vec::new();
        let mut seen_distr = Vec::new();
        let mut seen_split = Vec::new();
        for prime in PRIMES {
            let state = d.new_domain(prime);
            let distr = d.new_domain_distrib(prime, 0);
            let split = d.new_domain_split(prime, 11, 0);

            assert_ne!(state.pattern_id, d.pattern_id);
            assert_ne!(distr.pattern_id, d.pattern_id);
            assert_ne!(split.pattern_id, d.pattern_id);

            assert_eq!(state.sample_id, d.sample_id);
            assert_eq!(distr.sample_id, 0);
            assert!(split.sample_id >= d.sample_id);

            assert_eq!(state.pixel_id, d.pixel_id);
            assert_ne!(state.pattern_id, distr.pattern_id);
            assert_ne!(state.pattern_id, split.pattern_id);

            assert!(!seen_state.contains(&state.pattern_id));
            assert!(!seen_distr.contains(&distr.pattern_id));
            assert!(!seen_split.contains(&split.pattern_id));
            seen_state.push(state.pattern_id);
            seen_distr.push(distr.pattern_id);
            seen_split.push(split.pattern_id);
        }
    }
}
