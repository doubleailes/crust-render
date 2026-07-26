// Port of oqmc/reverse.h — bit reversal (radical inversion / Van der Corput).
//
// Reverses the order of bits in an integer, so the most significant bit becomes
// the least significant and vice versa. The upstream header hand-rolls the swap
// with masks and a byte-swap; Rust's `reverse_bits` compiles to the same
// `rbit`/`bswap`-style sequence, so we use it directly.

/// Reverse the bits of a 32-bit unsigned integer.
#[inline]
pub const fn reverse_bits32(value: u32) -> u32 {
    value.reverse_bits()
}

/// Reverse the bits of a 16-bit unsigned integer.
#[inline]
pub const fn reverse_bits16(value: u16) -> u16 {
    value.reverse_bits()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mirrors src/tests/reverse.cpp — reversing `out` yields `in`.
    #[test]
    fn reverse_32bit() {
        let inputs: [u32; 5] = [
            0b01010101010101010011001100110011,
            0b11111111000000001111000011110000,
            0b11111111111111110000000011111111,
            0b11111111111111111111111111111111,
            0b00000000000000000000000000000000,
        ];
        let outputs: [u32; 5] = [
            0b11001100110011001010101010101010,
            0b00001111000011110000000011111111,
            0b11111111000000001111111111111111,
            0b11111111111111111111111111111111,
            0b00000000000000000000000000000000,
        ];
        for i in 0..5 {
            assert_eq!(inputs[i], reverse_bits32(outputs[i]));
        }
    }

    #[test]
    fn reverse_16bit() {
        let inputs: [u16; 5] = [
            0b0101010100110011,
            0b0000000011110000,
            0b1111111100000000,
            0b1111111111111111,
            0b0000000000000000,
        ];
        let outputs: [u16; 5] = [
            0b1100110010101010,
            0b0000111100000000,
            0b0000000011111111,
            0b1111111111111111,
            0b0000000000000000,
        ];
        for i in 0..5 {
            assert_eq!(inputs[i], reverse_bits16(outputs[i]));
        }
    }
}
