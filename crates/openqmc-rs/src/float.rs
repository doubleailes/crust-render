// Port of oqmc/float.h — integer to `[0, 1)` float conversion.
//
// Uses the "clear the guard bit" trick (option 4 in the upstream header): by
// masking off the bit 24 places below the leading one, the default
// round-to-nearest conversion always rounds *down*, giving the optimal uniform
// probability density of Keller/Wächter/Binder without changing the FPU rounding
// mode, and guaranteeing an output strictly less than one.

/// 2^-32.
pub const FLOAT_ONE_OVER_TWO_POWER_32: f32 = 1.0 / 4294967296.0;

/// Scale a 32-bit unsigned integer into a `[0, 1)` float.
#[inline]
pub fn uint_to_float(value: u32) -> f32 {
    let mask = value >> 24;
    let safe = value & !mask;
    (safe as f32) * FLOAT_ONE_OVER_TWO_POWER_32
}

#[cfg(test)]
mod tests {
    use super::*;

    // Largest f32 strictly below 1.0 == nextafterf(1.0, 0.0).
    const ONE_MINUS_EPSILON: f32 = f32::from_bits(0x3f7f_ffff);

    #[test]
    fn one_over_two_power_32() {
        assert_eq!(FLOAT_ONE_OVER_TWO_POWER_32, 1.0 / ((1u64 << 32) as f32));
    }

    #[test]
    fn minimum() {
        assert_eq!(uint_to_float(0), 0.0);
        assert!(uint_to_float(1) > 0.0);
        assert!(uint_to_float(1) < uint_to_float(2));
    }

    #[test]
    fn maximum() {
        assert_eq!(uint_to_float(u32::MAX), ONE_MINUS_EPSILON);
    }

    #[test]
    fn high() {
        // Exactly 256 inputs [0xffffff00, 0xffffffff] map to 1 - eps.
        assert!(uint_to_float(0xffff_feff) < ONE_MINUS_EPSILON);
        assert_eq!(uint_to_float(0xffff_ff00), ONE_MINUS_EPSILON);
        assert_eq!(uint_to_float(0xffff_ff01), ONE_MINUS_EPSILON);
        assert_eq!(uint_to_float(0xffff_ffff), ONE_MINUS_EPSILON);
    }

    #[test]
    fn half() {
        // Exactly 256 inputs [0x80000000, 0x800000ff] map to 0.5.
        assert!(uint_to_float(0x7fff_ffff) < 0.5);
        assert_eq!(uint_to_float(0x8000_0000), 0.5);
        assert_eq!(uint_to_float(0x8000_0001), 0.5);
        assert_eq!(uint_to_float(0x8000_00ff), 0.5);
        assert!(uint_to_float(0x8000_0100) > 0.5);
    }

    #[test]
    fn monotonic() {
        let mut last = 0.0f32;
        for i in 0..8u32 {
            let step_int = u32::MAX / 8 * (i + 1);
            let step = uint_to_float(step_int);
            assert!(step > last);
            last = step;
        }
    }
}
