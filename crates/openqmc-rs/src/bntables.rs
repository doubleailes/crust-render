// Port of oqmc/bntables.h — pre-computed, optimised blue-noise key/rank tables
// used to give the base samplers a spatial blue-noise error distribution
// (generalising Belcour & Heitz). A single table pair serves all domains: the
// lookup is shifted toroidally per domain by a random offset.
//
// The upstream tables ship as text headers; this port bundles them as
// little-endian binary blobs (converted 1:1 from include/oqmc/data/**) and
// decodes them lazily into process globals.

use std::sync::OnceLock;

use crate::encode::{decode_bits16, encode_bits16, EncodeKey};

/// Pixel-x precision for the tables (256 pixels).
pub const XBITS: u32 = 8;
/// Pixel-y precision for the tables (256 pixels).
pub const YBITS: u32 = 8;
/// Table length (2^16).
pub const SIZE: usize = 1 << (XBITS + YBITS);

/// A key/rank pair used to randomise a sequence.
#[derive(Clone, Copy, Debug)]
pub struct TableReturnValue {
    pub key: u32,
    pub rank: u32,
}

/// Look up a key/rank pair, shifting the pixel coordinate toroidally by `shift`.
#[inline]
pub fn table_value<const XB: u32, const YB: u32, const ZB: u32>(
    pixel: u16,
    shift: u16,
    key_table: &[u32],
    rank_table: &[u32],
) -> TableReturnValue {
    let p = decode_bits16::<XB, YB, ZB>(pixel);
    let s = decode_bits16::<XB, YB, ZB>(shift);
    let index = encode_bits16::<XB, YB, ZB>(EncodeKey {
        x: p.x + s.x,
        y: p.y + s.y,
        z: p.z + s.z,
    }) as usize;

    TableReturnValue {
        key: key_table[index],
        rank: rank_table[index],
    }
}

fn decode_table(bytes: &'static [u8]) -> Box<[u32]> {
    debug_assert_eq!(bytes.len(), SIZE * 4);
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

macro_rules! table_pair {
    ($module:ident, $keys:literal, $ranks:literal) => {
        pub mod $module {
            use super::{decode_table, OnceLock};

            /// Optimised blue-noise key table.
            pub fn key_table() -> &'static [u32] {
                static T: OnceLock<Box<[u32]>> = OnceLock::new();
                T.get_or_init(|| decode_table(include_bytes!($keys)))
            }

            /// Optimised blue-noise rank table.
            pub fn rank_table() -> &'static [u32] {
                static T: OnceLock<Box<[u32]>> = OnceLock::new();
                T.get_or_init(|| decode_table(include_bytes!($ranks)))
            }
        }
    };
}

table_pair!(sobol, "data/sobol_keys.bin", "data/sobol_ranks.bin");
table_pair!(lattice, "data/lattice_keys.bin", "data/lattice_ranks.bin");
table_pair!(pmj, "data/pmj_keys.bin", "data/pmj_ranks.bin");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_decode_to_full_length() {
        assert_eq!(sobol::key_table().len(), SIZE);
        assert_eq!(sobol::rank_table().len(), SIZE);
        assert_eq!(lattice::key_table().len(), SIZE);
        assert_eq!(pmj::rank_table().len(), SIZE);
    }
}
