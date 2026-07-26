// Port of oqmc/pmjbn.h — the blue-noise variant of the PMJ sampler. Same
// sequence as `PmjSampler`, plus spatial blue-noise dithering between pixels. It
// shares the PMJ sample cache and adds the blue-noise key/rank tables. State is
// not pixel-decorrelated (the table lookup does that).

use crate::bntables::{self, table_value};
use crate::lookup::shuffled_scrambled_lookup;
use crate::pcg;
use crate::pmj::pmj_cache;
use crate::sampler::{Sampler, SamplerImpl};
use crate::state::State64Bit;

#[derive(Clone, Copy, Debug)]
pub struct PmjBnImpl {
    state: State64Bit,
    samples: &'static [[u32; 4]],
    key_table: &'static [u32],
    rank_table: &'static [u32],
}

impl SamplerImpl for PmjBnImpl {
    #[inline]
    fn from_pixel(x: i32, y: i32, frame: i32, index: i32) -> Self {
        Self {
            state: State64Bit::new(x, y, frame, index),
            samples: pmj_cache(),
            key_table: bntables::pmj::key_table(),
            rank_table: bntables::pmj::rank_table(),
        }
    }

    #[inline]
    fn new_domain(&self, key: i32) -> Self {
        Self {
            state: self.state.new_domain(key),
            ..*self
        }
    }

    #[inline]
    fn new_domain_split(&self, key: i32, size: i32, index: i32) -> Self {
        Self {
            state: self.state.new_domain_split(key, size, index),
            ..*self
        }
    }

    #[inline]
    fn new_domain_distrib(&self, key: i32, index: i32) -> Self {
        Self {
            state: self.state.new_domain_distrib(key, index),
            ..*self
        }
    }

    #[inline]
    fn draw_block(&self) -> [u32; 4] {
        let t = table_value::<8, 8, 0>(
            self.state.pixel_id,
            pcg::output(self.state.pattern_id) as u16,
            self.key_table,
            self.rank_table,
        );
        shuffled_scrambled_lookup::<4, 4>(self.state.sample_id as u32 ^ t.rank, t.key, self.samples)
    }

    #[inline]
    fn draw_rnd_block(&self) -> [u32; 4] {
        self.state.new_domain(self.state.pixel_id as i32).draw_rnd::<4>()
    }

    #[inline]
    fn rng(&self) -> pcg::Rng {
        self.state.new_domain(self.state.pixel_id as i32).rng()
    }

    fn warm_cache() {
        pmj_cache();
        bntables::pmj::key_table();
        bntables::pmj::rank_table();
    }
}

/// Blue-noise variant of the PMJ sampler.
pub type PmjBnSampler = Sampler<PmjBnImpl>;
