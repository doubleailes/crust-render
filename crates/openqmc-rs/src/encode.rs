// Port of oqmc/encode.h — pack/unpack a 3-axis integer coordinate key into a
// single 16-bit value. Used to store pixel coordinates compactly. The sum of the
// per-axis bit precisions must not exceed 16.

/// A 3-dimensional integer coordinate key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeKey {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// Encode a coordinate key into 16 bits with the given per-axis precisions.
#[inline]
pub fn encode_bits16<const XB: u32, const YB: u32, const ZB: u32>(key: EncodeKey) -> u16 {
    const { assert!(XB + YB + ZB <= 16, "Precision sum must be <= 16") };

    let mask_x = (1u32 << XB) - 1;
    let mask_y = (1u32 << YB) - 1;
    let mask_z = (1u32 << ZB) - 1;

    let offset_x = 0;
    let offset_y = XB;
    let offset_z = XB + YB;

    let mut value = 0u32;
    value |= (key.x as u32 & mask_x) << offset_x;
    value |= (key.y as u32 & mask_y) << offset_y;
    value |= (key.z as u32 & mask_z) << offset_z;

    value as u16
}

/// Decode a 16-bit value back into a coordinate key with the given precisions.
#[inline]
pub fn decode_bits16<const XB: u32, const YB: u32, const ZB: u32>(value: u16) -> EncodeKey {
    const { assert!(XB + YB + ZB <= 16, "Precision sum must be <= 16") };

    let value = value as u32;
    let mask_x = (1u32 << XB) - 1;
    let mask_y = (1u32 << YB) - 1;
    let mask_z = (1u32 << ZB) - 1;

    let offset_x = 0;
    let offset_y = XB;
    let offset_z = XB + YB;

    EncodeKey {
        x: ((value >> offset_x) & mask_x) as i32,
        y: ((value >> offset_y) & mask_y) as i32,
        z: ((value >> offset_z) & mask_z) as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_within_precision() {
        // 8/8/0 split (the pixel encoding used by State64Bit), values < 256.
        for x in [0, 1, 5, 42, 200, 255] {
            for y in [0, 3, 128, 255] {
                let k = EncodeKey { x, y, z: 0 };
                let e = encode_bits16::<8, 8, 0>(k);
                assert_eq!(decode_bits16::<8, 8, 0>(e), k);
            }
        }
    }

    #[test]
    fn masks_out_of_range_bits() {
        // x is only 8 bits wide, so bit 8+ is dropped.
        let e = encode_bits16::<8, 8, 0>(EncodeKey { x: 256, y: 0, z: 0 });
        assert_eq!(e, 0);
    }
}
