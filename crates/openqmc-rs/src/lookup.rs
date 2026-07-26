// Port of oqmc/lookup.h — randomised table lookups. A single pre-computed table
// (PMJ samples, blue-noise ranks) can be re-used across domains by shuffling the
// index and XOR-scrambling the value (random digit scramble, Kollig & Keller).

use crate::permute::shuffle;
use crate::rotate::rotate_bytes;

/// Random digit scramble: XOR the value with a random number (fast, structure-
/// preserving). The seed must be constant for a given sequence.
#[inline]
pub const fn random_digit_scramble(value: u32, hash: u32) -> u32 {
    value ^ hash
}

/// Compute a randomised value from a pre-computed table.
///
/// `TABLE` is the row width of the table, `DEPTH` (1..=4, `<= TABLE`) the output
/// count. The index is shuffled progressively; an index beyond the table length
/// wraps (mask to 16 bits) and reuses samples.
#[inline]
pub fn shuffled_scrambled_lookup<const TABLE: usize, const DEPTH: usize>(
    index: u32,
    hash: u32,
    table: &[[u32; TABLE]],
) -> [u32; DEPTH] {
    const { assert!(TABLE >= DEPTH, "Table width must be >= depth") };
    const { assert!(DEPTH >= 1 && DEPTH <= 4, "Pattern depth must be within [1, 4]") };

    let index = shuffle(index, hash);

    let mut sample = [0u32; DEPTH];
    let mut i = 0;
    while i < DEPTH {
        let row = &table[(index & 0xffff) as usize];
        sample[i] = random_digit_scramble(row[i], rotate_bytes(hash, i as i32));
        i += 1;
    }
    sample
}
