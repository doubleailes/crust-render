// Port of oqmc/sampler.h — the public, static-polymorphic sampler interface.
//
// Upstream `SamplerInterface<Impl>` composes an internal implementation and
// exposes a uniform domain-tree API. We model that with the [`SamplerImpl`] trait
// plus the generic [`Sampler`] wrapper. Each impl computes a full 4-dimensional
// block; the wrapper's const-generic `draw_*::<N>` methods return the first `N`
// values, which is identical to computing `N` directly in every upstream draw
// path (each output dimension is independent of the requested depth).
//
// Divergence from the C++ headers: upstream takes a caller-allocated `void*`
// cache, a design driven by GPU memory management (see sampler.h). This CPU port
// instead builds any required tables in lazily-initialised process globals, so
// every `Sampler<T>` stays a small `Copy + Send + 'static` value.

use crate::float::uint_to_float;
use crate::pcg;
use crate::range::uint_to_range;

/// Internal sampler implementation. Not called directly — use [`Sampler`].
pub trait SamplerImpl: Copy {
    /// Construct from pixel/frame/sample indices (index must be `>= 0`).
    fn from_pixel(x: i32, y: i32, frame: i32, index: i32) -> Self;
    /// Derive a child domain (independent 4D pattern).
    fn new_domain(&self, key: i32) -> Self;
    /// Derive a fixed-rate split domain.
    fn new_domain_split(&self, key: i32, size: i32, index: i32) -> Self;
    /// Derive an adaptive-rate (local) split domain.
    fn new_domain_distrib(&self, key: i32, index: i32) -> Self;
    /// Compute the 4D high-quality sample block for this domain.
    fn draw_block(&self) -> [u32; 4];
    /// Compute the 4D low-quality pseudo-random block for this domain.
    fn draw_rnd_block(&self) -> [u32; 4];
    /// A sequential PRNG stream for this domain (for unbounded draws).
    fn rng(&self) -> pcg::Rng;
    /// Force any lazily-built cache to initialise. Optional; draws trigger it
    /// anyway. Default no-op for cache-free samplers.
    fn warm_cache() {}
}

#[inline]
fn take<const N: usize>(block: [u32; 4]) -> [u32; N] {
    const { assert!(N >= 1 && N <= 4, "Draw size must be within [1, 4]") };
    let mut out = [0u32; N];
    out.copy_from_slice(&block[..N]);
    out
}

/// The public sampler. Different `Impl`s are interchangeable at every call site.
///
/// A sampler is immutable: derive children with `new_domain*`, draw values with
/// `draw_*`. Deriving a domain is cheap (an LCG state transition); drawing is
/// where the sequence work happens.
#[derive(Clone, Copy, Debug)]
pub struct Sampler<T: SamplerImpl> {
    imp: T,
}

impl<T: SamplerImpl> Sampler<T> {
    /// Eagerly initialise any lazily-built cache for this sampler type.
    #[inline]
    pub fn warm_cache() {
        T::warm_cache();
    }

    /// Construct from pixel coordinate, frame and sample index (`index >= 0`).
    #[inline]
    pub fn new(x: i32, y: i32, frame: i32, index: i32) -> Self {
        debug_assert!(index >= 0);
        Self {
            imp: T::from_pixel(x, y, frame, index),
        }
    }

    /// Derive a child domain with an independent 4D pattern.
    #[inline]
    pub fn new_domain(&self, key: i32) -> Self {
        Self {
            imp: self.imp.new_domain(key),
        }
    }

    /// Derive a split domain with a fixed sample-rate multiplier.
    #[inline]
    pub fn new_domain_split(&self, key: i32, size: i32, index: i32) -> Self {
        debug_assert!(size > 0 && index >= 0);
        Self {
            imp: self.imp.new_domain_split(key, size, index),
        }
    }

    /// Derive a split domain with a local (adaptive) distribution.
    #[inline]
    pub fn new_domain_distrib(&self, key: i32, index: i32) -> Self {
        debug_assert!(index >= 0);
        Self {
            imp: self.imp.new_domain_distrib(key, index),
        }
    }

    /// Derive a split domain with a global (adaptive) distribution
    /// (`new_domain(key).new_domain(index)`).
    #[inline]
    pub fn new_domain_chain(&self, key: i32, index: i32) -> Self {
        debug_assert!(index >= 0);
        self.new_domain(key).new_domain(index)
    }

    /// Draw `N` (1..=4) high-quality integer sample values, each in `[0, 2^32)`.
    #[inline]
    pub fn draw_sample<const N: usize>(&self) -> [u32; N] {
        take(self.imp.draw_block())
    }

    /// Draw `N` high-quality sample values within `[0, range)`.
    #[inline]
    pub fn draw_sample_range<const N: usize>(&self, range: u32) -> [u32; N] {
        debug_assert!(range > 0);
        let block = self.draw_sample::<N>();
        let mut out = [0u32; N];
        for i in 0..N {
            out[i] = uint_to_range(block[i], range);
        }
        out
    }

    /// Draw `N` high-quality sample values within `[0, 1)`.
    #[inline]
    pub fn draw_sample_f32<const N: usize>(&self) -> [f32; N] {
        let block = self.draw_sample::<N>();
        let mut out = [0.0f32; N];
        for i in 0..N {
            out[i] = uint_to_float(block[i]);
        }
        out
    }

    /// Draw `N` low-quality pseudo-random integer values, each in `[0, 2^32)`.
    #[inline]
    pub fn draw_rnd<const N: usize>(&self) -> [u32; N] {
        take(self.imp.draw_rnd_block())
    }

    /// Draw `N` low-quality pseudo-random values within `[0, range)`.
    #[inline]
    pub fn draw_rnd_range<const N: usize>(&self, range: u32) -> [u32; N] {
        debug_assert!(range > 0);
        let block = self.draw_rnd::<N>();
        let mut out = [0u32; N];
        for i in 0..N {
            out[i] = uint_to_range(block[i], range);
        }
        out
    }

    /// Draw `N` low-quality pseudo-random values within `[0, 1)`.
    #[inline]
    pub fn draw_rnd_f32<const N: usize>(&self) -> [f32; N] {
        let block = self.draw_rnd::<N>();
        let mut out = [0.0f32; N];
        for i in 0..N {
            out[i] = uint_to_float(block[i]);
        }
        out
    }

    /// A sequential PRNG stream seeded from this domain, for unbounded random
    /// draws (Russian roulette, delta tracking) where QMC stratification does not
    /// apply.
    #[inline]
    pub fn rng(&self) -> pcg::Rng {
        self.imp.rng()
    }
}
