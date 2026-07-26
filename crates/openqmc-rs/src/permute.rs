// Port of oqmc/permute.h — hash-based permutations. A Laine & Karras style
// permutation (from "Stratified Sampling for Stochastic Transparency"), with the
// improved constants from Nathan Vegdahl's "Building a Better LK Hash". Combined
// with bit reversal before and after, this forms an efficient hash-based Owen
// scramble.

use crate::reverse::reverse_bits32;

/// Laine & Karras style permutation: lower bits affect higher bits but not the
/// reverse, so paired with reversal it becomes a hash-based Owen scramble.
#[inline]
pub const fn laine_karras_permutation(mut value: u32, seed: u32) -> u32 {
    value ^= value.wrapping_mul(0x3d20adea);
    value = value.wrapping_add(seed);
    value = value.wrapping_mul((seed >> 16) | 1);
    value ^= value.wrapping_mul(0x05526c56);
    value ^= value.wrapping_mul(0x53a22864);
    value
}

/// Reverse the bits, then apply the permutation.
#[inline]
pub const fn reverse_and_shuffle(value: u32, seed: u32) -> u32 {
    laine_karras_permutation(reverse_bits32(value), seed)
}

/// A full hash-based Owen scramble (reverse, permute, reverse).
#[inline]
pub const fn shuffle(value: u32, seed: u32) -> u32 {
    reverse_bits32(reverse_and_shuffle(value, seed))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIMES: [u32; 20] = [
        2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71,
    ];

    // Mirrors src/tests/permute.cpp:Reverse — reverse_and_shuffle == LK(reverse).
    #[test]
    fn reverse_matches_composition() {
        let values: [u32; 3] = [
            0b01010101010101010011001100110011,
            0b11111111000000001111000011110000,
            0b11111111111111110000000011111111,
        ];
        for value in values {
            for prime in PRIMES {
                let reversed = reverse_bits32(value);
                let v2 = laine_karras_permutation(reversed, prime);
                assert_eq!(reverse_and_shuffle(value, prime), v2);
            }
        }
    }

    // Mirrors src/tests/permute.cpp:FullPermutation — over a 4-bit range the
    // low nibble of `shuffle` is a bijection.
    #[test]
    fn full_permutation_is_bijection() {
        const SIZE: usize = 1 << 4;
        for prime in PRIMES {
            let mut seen = [false; SIZE];
            for i in 0..SIZE as u32 {
                let permuted = reverse_bits32(reverse_and_shuffle(i, prime));
                assert_eq!(permuted, shuffle(i, prime));
                let index = (permuted & (SIZE as u32 - 1)) as usize;
                assert!(!seen[index]);
                seen[index] = true;
            }
            assert!(seen.iter().all(|&b| b));
        }
    }
}
