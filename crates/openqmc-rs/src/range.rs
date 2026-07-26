// Port of oqmc/range.h — map a full-range 32-bit integer into a bounded range
// using the high-bits-preserving multiplication method (Lemire 2019), which
// keeps the good high-order bits of QMC sequences and weak PRNGs alike. Prefer
// this over modulo. A small bias remains at large non-power-of-two ranges (no
// rejection loop, by design).

/// Map `value` into `[0, range)`.
#[inline]
pub const fn uint_to_range(value: u32, range: u32) -> u32 {
    debug_assert!(range > 0);
    ((value as u64 * range as u64) >> 32) as u32
}

/// Map `value` into `[begin, end)`.
#[inline]
pub const fn uint_to_range_between(value: u32, begin: u32, end: u32) -> u32 {
    debug_assert!(begin < end);
    uint_to_range(value, end - begin) + begin
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_in_range() {
        for &v in &[0u32, 1, 0x4000_0000, 0x8000_0000, 0xffff_ffff] {
            for range in [1u32, 2, 3, 6, 100, 65536] {
                assert!(uint_to_range(v, range) < range);
            }
        }
    }

    #[test]
    fn extremes() {
        assert_eq!(uint_to_range(0, 10), 0);
        assert_eq!(uint_to_range(u32::MAX, 10), 9);
        assert_eq!(uint_to_range_between(u32::MAX, 5, 15), 14);
    }
}
