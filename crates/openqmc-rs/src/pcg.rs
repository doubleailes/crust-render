// Port of oqmc/pcg.h — the PCG-RXS-M-XS-32 PRNG by Melissa O'Neill, used both as
// a classic sequential PRNG and (via `hash`) as a stateless parallel hash
// function per Jarzynski & Olano, "Hash Functions for GPU Rendering". Constants
// are taken from https://github.com/imneme/pcg-c.

use crate::float::uint_to_float;

/// LCG state-transition: advance the PRNG index along the sequence.
#[inline]
pub const fn state_transition(state: u32) -> u32 {
    state.wrapping_mul(747796405).wrapping_add(2891336453)
}

/// Output permutation of the state (the RXS-M-XS steps), giving a usable value.
#[inline]
pub const fn output(mut state: u32) -> u32 {
    // RXS
    state ^= state >> (4 + (state >> 28));
    // M
    state = state.wrapping_mul(277803737);
    // XS
    state ^= state >> 22;
    state
}

/// Default-initialise the PRNG state (zero seed).
#[inline]
pub const fn init() -> u32 {
    state_transition(0)
}

/// Initialise the PRNG state from a seed.
#[inline]
pub const fn init_seed(seed: u32) -> u32 {
    init().wrapping_add(seed)
}

/// Stateless hash of an input key.
#[inline]
pub const fn hash(key: u32) -> u32 {
    output(state_transition(key))
}

/// Advance `state` and return the next value in the sequence.
#[inline]
pub fn rng(state: &mut u32) -> u32 {
    *state = state_transition(*state);
    output(*state)
}

/// A sequential PCG PRNG stream.
///
/// This is not part of the upstream C++ headers as a standalone type, but it is
/// exactly the mechanism `State64Bit::drawRnd` uses internally (run `pcg::rng`
/// on a running state). It is the natural source for *unbounded* or *incidental*
/// random streams — Russian roulette, volume delta-tracking — where stratified
/// QMC dimensions do not apply, and a convenient uniform RNG for testing.
#[derive(Clone, Copy, Debug)]
pub struct Rng {
    state: u32,
}

impl Rng {
    /// Seed a stream. The seed is used directly as the running state (matching
    /// `State64Bit::drawRnd`, which seeds with `patternId + sampleId`).
    #[inline]
    pub const fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    /// Next raw 32-bit value.
    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        rng(&mut self.state)
    }

    /// Next value in `[0, 1)`.
    #[inline]
    pub fn next_f32(&mut self) -> f32 {
        uint_to_float(self.next_u32())
    }

    /// Next two values, each in `[0, 1)`.
    #[inline]
    pub fn next_2d(&mut self) -> [f32; 2] {
        [self.next_f32(), self.next_f32()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIMES: [u32; 20] = [
        2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71,
    ];

    // Mirrors src/tests/pcg.cpp structural invariants.
    #[test]
    fn state_transition_changes() {
        assert_ne!(state_transition(0), 0);
        for p in PRIMES {
            assert_ne!(state_transition(p), p);
        }
    }

    #[test]
    fn output_permutation_fixes_zero() {
        assert_eq!(output(0), 0);
        for p in PRIMES {
            assert_ne!(output(p), p);
        }
    }

    #[test]
    fn init_default_equals_zero_seed() {
        assert_eq!(init(), init_seed(0));
    }

    #[test]
    fn hash_equals_rng_on_init() {
        // hash(state) == rng(state) when state is freshly initialised.
        for seed in PRIMES {
            let mut state = init_seed(seed);
            let h = hash(state);
            let r = rng(&mut state);
            assert_eq!(h, r);
        }
    }

    #[test]
    fn rng_stream_is_deterministic() {
        let mut a = Rng::new(0xC0FF_EE00);
        let mut b = Rng::new(0xC0FF_EE00);
        for _ in 0..16 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
    }
}
