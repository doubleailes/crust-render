// Port of oqmc/sobol.h — the Owen-scrambled Sobol sampler.
//
// No cache: all samples are computed on the fly. The per-draw cost is higher than
// the other samplers, but Owen scrambling gives excellent random error
// cancellation and a very high rate of integration for smooth functions.

use crate::owen::shuffled_scrambled_sobol;
use crate::pcg;
use crate::sampler::{Sampler, SamplerImpl};
use crate::state::State64Bit;

#[derive(Clone, Copy, Debug)]
pub struct SobolImpl {
    state: State64Bit,
}

impl SamplerImpl for SobolImpl {
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
        shuffled_scrambled_sobol::<4>(self.state.sample_id as u32, pcg::output(self.state.pattern_id))
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

/// Owen-scrambled Sobol sampler.
pub type SobolSampler = Sampler<SobolImpl>;
