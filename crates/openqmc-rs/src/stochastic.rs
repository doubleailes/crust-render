// Port of oqmc/stochastic.h — progressive multi-jittered (0,2) sequence
// construction (Helmer et al., "Stochastic Generation of (t,s) Sample
// Sequences"). Builds the first pair of dimensions; the second pair is a
// randomisation of the first. Used to fill the PMJ sampler's cache.

use crate::lookup::shuffled_scrambled_lookup;
use crate::pcg;

/// XOR tables for the progressive (0,2) construction, from stochastic.h.
#[rustfmt::skip]
const PMJ_XORS: [[u16; 16]; 2] = [
    [
        0b0000000000000000, 0b0000000000000000, 0b0000000000000010, 0b0000000000000110,
        0b0000000000000110, 0b0000000000001110, 0b0000000000110110, 0b0000000001001110,
        0b0000000000010110, 0b0000000000101110, 0b0000001001110110, 0b0000011011001110,
        0b0000011100010110, 0b0000110000101110, 0b0011000001110110, 0b0100000011001110,
    ],
    [
        0b0000000000000000, 0b0000000000000001, 0b0000000000000011, 0b0000000000000011,
        0b0000000000000111, 0b0000000000011011, 0b0000000000100111, 0b0000000000001011,
        0b0000000000010111, 0b0000000100111011, 0b0000001101100111, 0b0000001110001011,
        0b0000011000010111, 0b0001100000111011, 0b0010000001100111, 0b0000000010001011,
    ],
];

/// Fill `table` (length `nsamples`, each a 4D sample) with a progressive
/// multi-jittered (0,2) sequence.
///
/// `nsamples` must be `2^16`: the final randomised lookup shuffles indices across
/// the full 16-bit range, so a smaller table would be indexed out of bounds. This
/// matches upstream, which only ever calls it with `State64Bit::maxIndexSize`.
pub fn stochastic_pmj_init(nsamples: usize, table: &mut [[u32; 4]]) {
    assert_eq!(nsamples, 1 << 16, "PMJ init requires a full 2^16 table");
    assert!(table.len() >= nsamples);

    let mut buffer = vec![[0u32; 2]; nsamples];

    let mut state = pcg::init();

    for slot in &mut buffer[0] {
        *slot = pcg::rng(&mut state);
    }

    let mut prev_len = 1usize;
    let mut log_n = 0u32;
    while prev_len < nsamples {
        let mut i1 = 0usize;
        let mut i2 = prev_len;
        while i1 < prev_len && i2 < nsamples {
            for k in 0..2 {
                let swap_bit = 0x8000_0000u32 >> log_n;
                let bit_mask = swap_bit - 1;

                let j = i1 ^ PMJ_XORS[k][log_n as usize] as usize;

                let prev_stratum = buffer[j][k] & !bit_mask;
                let next_stratum = prev_stratum ^ swap_bit;

                buffer[i2][k] = next_stratum | (pcg::rng(&mut state) & bit_mask);
            }
            i1 += 1;
            i2 += 1;
        }
        prev_len *= 2;
        log_n += 1;
    }

    for (i, row) in table.iter_mut().enumerate().take(nsamples) {
        let a = shuffled_scrambled_lookup::<2, 2>(i as u32, pcg::hash(0), &buffer);
        let b = shuffled_scrambled_lookup::<2, 2>(i as u32, pcg::hash(1), &buffer);
        row[0] = a[0];
        row[1] = a[1];
        row[2] = b[0];
        row[3] = b[1];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: usize = 1 << 16;

    #[test]
    fn init_is_deterministic() {
        let mut a = vec![[0u32; 4]; FULL];
        let mut b = vec![[0u32; 4]; FULL];
        stochastic_pmj_init(FULL, &mut a);
        stochastic_pmj_init(FULL, &mut b);
        assert_eq!(a, b);
    }

    // The first pair of a PMJ(0,2) sequence is a (0,2)-net (mirrors the Sobol
    // 02Sequence test): for a 2^M prefix, every power-of-two stratification holds
    // exactly one point.
    #[test]
    fn first_pair_is_02_net() {
        const M: u32 = 8;
        const N: usize = 1 << M;
        let mut table = vec![[0u32; 4]; FULL];
        stochastic_pmj_init(FULL, &mut table);
        let table = &table[..N];

        for i in 0..=M {
            let x_res = 1u32 << i;
            let y_res = 1u32 << (M - i);
            let x_width = u32::MAX / x_res;
            let y_width = u32::MAX / y_res;

            let mut strata = vec![false; N];
            for s in table {
                let x = s[0] / x_width;
                let y = s[1] / y_width;
                let cell = (x + y * x_res) as usize;
                assert!(!strata[cell], "stratum {cell} occupied twice");
                strata[cell] = true;
            }
            assert!(strata.iter().all(|&b| b));
        }
    }
}
