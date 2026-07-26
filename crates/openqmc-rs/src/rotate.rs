// Port of oqmc/rotate.h — bit/byte rotation, used to extract fresh random
// values from an existing hash or RNG number.

/// Rotate the bits of a 32-bit integer right by `distance` (wrapping every 32).
///
/// Upstream computes `value >> (distance & 31) | value << ((-distance) & 31)`,
/// which is exactly a right-rotation; `u32::rotate_right` reduces the distance
/// modulo 32 identically.
#[inline]
pub const fn rotate_bits(value: u32, distance: u32) -> u32 {
    value.rotate_right(distance)
}

/// Rotate the bytes of a 4-byte integer by `distance` (wrapping every 4).
#[inline]
pub const fn rotate_bytes(value: u32, distance: i32) -> u32 {
    rotate_bits(value, (distance * 8) as u32)
}
