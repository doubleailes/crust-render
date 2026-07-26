// Port of oqmc/lattice.h — the rank-1 lattice sampler.
//
// No cache: samples are computed on the fly with a low per-draw cost. Runtime
// performance is high, though the rate of integration per pixel can be lower than
// the Sobol or PMJ samplers.

use crate::pcg;
use crate::rank1::shuffled_rotated_lattice;
use crate::sampler::{Sampler, SamplerImpl};
use crate::state::State64Bit;

#[derive(Clone, Copy, Debug)]
pub struct LatticeImpl {
    state: State64Bit,
}

impl SamplerImpl for LatticeImpl {
    #[inline]
    fn from_pixel(x: i32, y: i32, frame: i32, index: i32) -> Self {
        Self {
            state: State64Bit::new(x, y, frame, index).pixel_decorrelate(),
        }
    }

    #[inline]
    fn new_domain(&self, key: i32) -> Self {
        Self {
            state: self.state.new_domain(key),
        }
    }

    #[inline]
    fn new_domain_split(&self, key: i32, size: i32, index: i32) -> Self {
        Self {
            state: self.state.new_domain_split(key, size, index),
        }
    }

    #[inline]
    fn new_domain_distrib(&self, key: i32, index: i32) -> Self {
        Self {
            state: self.state.new_domain_distrib(key, index),
        }
    }

    #[inline]
    fn draw_block(&self) -> [u32; 4] {
        // The lattice takes the raw pattern id (it applies pcg::output / pcg::rng
        // internally for the shuffle seed and the toroidal shifts).
        shuffled_rotated_lattice::<4>(self.state.sample_id as u32, self.state.pattern_id)
    }

    #[inline]
    fn draw_rnd_block(&self) -> [u32; 4] {
        self.state.draw_rnd::<4>()
    }

    #[inline]
    fn rng(&self) -> pcg::Rng {
        self.state.rng()
    }
}

/// Rank-1 lattice sampler.
pub type LatticeSampler = Sampler<LatticeImpl>;
