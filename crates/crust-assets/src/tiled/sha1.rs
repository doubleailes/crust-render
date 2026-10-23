//! SHA-1 (FIPS 180-4), for the `oiio:SHA-1` a `.tx` records.
//!
//! In-crate rather than a dependency: this path is cold (one hash per
//! conversion), and the algorithm is a page of safe code checked against the
//! standard's own test vectors below. OIIO uses the hash only to share
//! identical textures in its cache, so nothing here is security-relevant.

/// The SHA-1 digest of `data`, as 40 upper-case hexadecimal digits — how
/// OIIO writes `oiio:SHA-1`.
pub(crate) fn sha1_hex(data: &[u8]) -> String {
    sha1(data).iter().map(|b| format!("{b:02X}")).collect()
}

/// The SHA-1 digest of `data`.
pub(crate) fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    // The message, a single 1 bit, zeros, and its length in bits: a whole
    // number of 64-byte blocks.
    let bits = (data.len() as u64).wrapping_mul(8);
    let mut tail = data[data.len() - data.len() % 64..].to_vec();
    tail.push(0x80);
    while tail.len() % 64 != 56 {
        tail.push(0);
    }
    tail.extend_from_slice(&bits.to_be_bytes());
    let (whole, _) = data.as_chunks::<64>();
    let (padded, _) = tail.as_chunks::<64>();
    for block in whole.iter().chain(padded) {
        let mut w = [0u32; 80];
        for (t, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[t] = u32::from_be_bytes(*word);
        }
        for t in 16..80 {
            w[t] = (w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (t, &wt) in w.iter().enumerate() {
            let (f, k) = match t {
                0..20 => ((b & c) | (!b & d), 0x5A82_7999),
                20..40 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..60 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wt);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        for (s, v) in h.iter_mut().zip([a, b, c, d, e]) {
            *s = s.wrapping_add(v);
        }
    }
    let mut out = [0u8; 20];
    for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *o = s.to_be_bytes();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The FIPS 180-4 (and RFC 3174) test vectors: one block, the empty
    /// message, a message whose padding spills into a second block, and a
    /// million bytes.
    #[test]
    fn the_standard_test_vectors() {
        assert_eq!(sha1_hex(b"abc"), "A9993E364706816ABA3E25717850C26C9CD0D89D");
        assert_eq!(sha1_hex(b""), "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709");
        assert_eq!(
            sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983E441C3BD26EBAAE4AA1F95129E5E54670F1"
        );
        assert_eq!(
            sha1_hex(&vec![b'a'; 1_000_000]),
            "34AA973CD4C4DAA4F61EEB2BDBAD27316534016F"
        );
        // Exactly one block of message, so the padding is a block of its own.
        assert_eq!(
            sha1_hex(&[b'x'; 64]),
            "BB2FA3EE7AFB9F54C6DFB5D021F14B1FFE40C163"
        );
    }
}
