// Port of oqmc/latticebn.h — the blue-noise variant of the rank-1 lattice
// sampler. Same sequence as `LatticeSampler`, plus spatial blue-noise dithering
// between pixels. State is not pixel-decorrelated (the table lookup does that).

use crate::bntables::{self, table_value};
use crate::pcg;
use crate::rank1::shuffled_rotated_lattice;
use crate::sampler::{Sampler, SamplerImpl};
use crate::state::State64Bit;

#[derive(Clone, Copy, Debug)]
pub struct LatticeBnImpl {
    state: State64Bit,
    key_table: &'static [u32],
    rank_table: &'static [u32],
}

impl SamplerImpl for LatticeBnImpl {
    #[inline]
    fn from_pixel(x: i32, y: i32, frame: i32, index: i32) -> Self {
        Self {
            state: State64Bit::new(x, y, frame, index),
            key_table: bntables::lattice::key_table(),
            rank_table: bntables::lattice::rank_table(),
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
        shuffled_rotated_lattice::<4>(self.state.sample_id as u32 ^ t.rank, t.key)
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
        bntables::lattice::key_table();
        bntables::lattice::rank_table();
    }
}

/// Blue-noise variant of the rank-1 lattice sampler.
pub type LatticeBnSampler = Sampler<LatticeBnImpl>;
