// Port of oqmc/rank1.h — a rank-1 lattice (Hickernell et al., "Weighted Compound
// Integration Rules with Higher Order Convergence for all N") made progressive
// with a radical inversion of the sample index, randomised with toroidal shifts.

use crate::pcg;
use crate::permute::reverse_and_shuffle;

/// Generator vector from the Hickernell et al. publication.
const LATTICE: [u32; 4] = [1, 364981, 245389, 97823];

/// Toroidal shift: offset a value, relying on integer wraparound.
#[inline]
const fn rotate(value: u32, distance: u32) -> u32 {
    value.wrapping_add(distance)
}

/// Rank-1 lattice value at a bit-reversed index for a given dimension (0..4).
#[inline]
pub const fn lattice_reversed_index(index: u32, dimension: usize) -> u32 {
    debug_assert!(dimension <= 3);
    LATTICE[dimension].wrapping_mul(index)
}

/// Compute a randomised rank-1 lattice value.
///
/// `DEPTH` is the dimensional output count (1..=4). `pattern_id` seeds the
/// randomisation and must be constant for a given lattice.
#[inline]
pub fn shuffled_rotated_lattice<const DEPTH: usize>(
    index: u32,
    mut pattern_id: u32,
) -> [u32; DEPTH] {
    const { assert!(DEPTH >= 1 && DEPTH <= 4, "Pattern depth must be within [1, 4]") };

    let index = reverse_and_shuffle(index, pcg::output(pattern_id));

    let mut sample = [0u32; DEPTH];
    let mut i = 0;
    while i < DEPTH {
        // pcg::rng advances `pattern_id` and returns a fresh shift each iteration.
        sample[i] = rotate(lattice_reversed_index(index, i), pcg::rng(&mut pattern_id));
        i += 1;
    }
    sample
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let a = shuffled_rotated_lattice::<4>(7, pcg::hash(3));
        let b = shuffled_rotated_lattice::<4>(7, pcg::hash(3));
        assert_eq!(a, b);
    }

    #[test]
    fn dimension0_generator_is_one() {
        // LATTICE[0] == 1, so dim0 is a pure radical-inverse + shift.
        assert_eq!(lattice_reversed_index(12345, 0), 12345);
    }
}
