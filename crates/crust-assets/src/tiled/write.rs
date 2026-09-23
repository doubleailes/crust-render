//! Writing a tiled, mip-mapped, Deflate-compressed TIFF — a `.tx`.
//!
//! **Why this is hand-rolled on top of `DirectoryEncoder`.** The `tiff` crate's
//! encoder is strips-only; tiled output has been an open request since 2023
//! (image-tiff#205) and is absent from the current release. But only the
//! *tiling* is missing, not the container: `DirectoryEncoder` still gives the
//! file header, the IFD chain and its back-patching, 12-byte entry
//! serialisation, the inline-versus-offset rule for values over four bytes, and
//! ascending tag order (its `Directory` is a `BTreeMap`, which TIFF6 requires).
//! So the writer here supplies three things and borrows the rest: the tile
//! grid, the per-tile zlib stream, and the tile tags.
//!
//! **The compression must be ours.** `tiff` applies its own only inside the
//! bulk strip writer, and `TiffWriter::set_compression` is private — driving
//! the encoder tile-wise with `with_compression(Deflate)` set would emit
//! *uncompressed* bytes under a tag claiming Deflate, which is a corrupt file
//! that reads as garbage. Deflating each tile here and leaving the encoder in
//! its default uncompressed mode is not a workaround, it is the only correct
//! arrangement.
//!
//! **Deflate rather than LZW**, deliberately: tiled LZW fails to decode in
//! `tiff` 0.11.3 (image-tiff#395, "no lzw end code found" when the compressed
//! bytes exactly fill the tile), and it is the compression `maketx` defaults to
//! anyway.

use crate::uv_texture::{encode_fn, reduce_half, to_linear_table};
use crust_core::ColorSpace;
use std::io::{self, Write};
use std::path::Path;
use tiff::tags::Tag;

/// Tile edge, in texels. 64 is OIIO's default for 8-bit data and therefore
/// what the `.tx` files a studio already has are cut to.
///
/// TIFF6 requires tile dimensions to be a multiple of 16; 64 satisfies that and
/// keeps one RGB tile at 12 KiB, which is the granularity the cache evicts at.
pub const TILE_EDGE: usize = 64;

/// Writes `src` (row-major RGB, 8 bits a channel) as a tiled, mip-mapped
/// `.tx`, returning each level's `(width, height)` in order.
///
/// `space` is the colour space the *file* is in, and it is needed even though
/// no pixel changes space here: the mip chain is reduced in linear light, so
/// each level is decoded, averaged and re-encoded. Passing the wrong one does
/// not corrupt level 0 — it darkens or brightens every level above it, which is
/// exactly the failure `docs/color_management.md` exists to prevent.
///
/// The chain runs to 1x1 rather than stopping early. The tail is negligible
/// (the whole pyramid is about 4/3 of level 0) and a cone that has been through
/// a diffuse bounce asks for the coarsest level there is, so the alternative is
/// a clamp that reads a level far sharper than the footprint wants.
pub fn write_tx(
    path: &Path,
    src: &[u8],
    width: usize,
    height: usize,
    space: ColorSpace,
) -> io::Result<Vec<(usize, usize)>> {
    if width == 0 || height == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to write a zero-sized texture",
        ));
    }
    let want = width * height * 3;
    if src.len() < want {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("expected {want} bytes of RGB, got {}", src.len()),
        ));
    }

    let to_linear = to_linear_table(space);
    let encode = encode_fn(space);

    // Every level materialised up front rather than streamed. The pyramid is
    // 4/3 of the source, which for anything a `.tx` is worth writing for is
    // already in memory — and each level is reduced from the one above, so they
    // have to exist in order anyway.
    let mut levels: Vec<(Vec<u8>, usize, usize)> = vec![(src[..want].to_vec(), width, height)];
    while {
        let (_, w, h) = levels.last().expect("level 0 always exists");
        *w > 1 || *h > 1
    } {
        let (pixels, w, h) = {
            let (p, w, h) = levels.last().expect("level 0 always exists");
            reduce_half(p, *w, *h, &to_linear, encode)
        };
        levels.push((pixels, w, h));
    }

    let file = std::fs::File::create(path)?;
    let mut enc = tiff::encoder::TiffEncoder::new(io::BufWriter::new(file))
        .map_err(|e| io::Error::other(format!("tiff header: {e}")))?;

    for (n, (pixels, w, h)) in levels.iter().enumerate() {
        write_level(&mut enc, pixels, *w, *h, n, space)?;
    }

    Ok(levels.iter().map(|(_, w, h)| (*w, *h)).collect())
}

/// The `ImageDescription` string recording which colour space a file's mip
/// chain was reduced in. Parsed back by [`crate::tiled::mip_space`].
pub(crate) fn mip_space_tag(space: ColorSpace) -> String {
    format!("crust:mipspace={}", space_name(space))
}

/// The stable spelling of a colour space in a `.tx`. Matched on the variant so
/// a new one is a compile error here rather than a silently unlabelled file.
pub(crate) fn space_name(space: ColorSpace) -> &'static str {
    match space {
        ColorSpace::Srgb => "srgb_texture",
        ColorSpace::Gamma22 => "g22_rec709",
        ColorSpace::Gamma18 => "g18_rec709",
        ColorSpace::Raw => "raw",
        // Never written: a conversion resolves `Auto` against its source
        // first. Spelled distinctly so an unresolved one matches no file.
        ColorSpace::Auto => "auto",
    }
}

/// One mip level as one IFD: every tile's compressed bytes, then the tags that
/// describe them.
fn write_level<W: Write + io::Seek>(
    enc: &mut tiff::encoder::TiffEncoder<W>,
    pixels: &[u8],
    w: usize,
    h: usize,
    level: usize,
    space: ColorSpace,
) -> io::Result<()> {
    let across = w.div_ceil(TILE_EDGE);
    let down = h.div_ceil(TILE_EDGE);

    let mut dir = enc
        .image_directory()
        .map_err(|e| io::Error::other(format!("tiff directory: {e}")))?;

    let mut offsets = Vec::with_capacity(across * down);
    let mut counts = Vec::with_capacity(across * down);
    // One scratch tile reused across the level: every tile is full-size by
    // TIFF6, so the allocation never changes shape.
    let mut tile = vec![0u8; TILE_EDGE * TILE_EDGE * 3];

    for ty in 0..down {
        for tx in 0..across {
            // Edge tiles are **padded, not clipped** — TIFF6 says a tile is
            // always `TileWidth x TileLength`, and every reader sizes its
            // buffer from the tags rather than from the image bounds. Clipping
            // here would make the last column of tiles short and every decode
            // of them a buffer-size error.
            tile.fill(0);
            for ly in 0..TILE_EDGE {
                let sy = ty * TILE_EDGE + ly;
                if sy >= h {
                    break;
                }
                let copy = TILE_EDGE.min(w - tx * TILE_EDGE);
                let s = (sy * w + tx * TILE_EDGE) * 3;
                let d = ly * TILE_EDGE * 3;
                tile[d..d + copy * 3].copy_from_slice(&pixels[s..s + copy * 3]);
            }

            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            z.write_all(&tile)?;
            let packed = z.finish()?;

            // `write_data` returns the offset it placed the bytes at, which is
            // the whole reason this can be done through the public API instead
            // of tracking file positions by hand.
            let at = dir
                .write_data(&packed[..])
                .map_err(|e| io::Error::other(format!("tile data: {e}")))?;
            offsets.push(at as u32);
            counts.push(packed.len() as u32);
        }
    }

    let mut tag = |t: Tag, v: &[u32]| -> io::Result<()> {
        if v.len() == 1 {
            dir.write_tag(t, v[0])
        } else {
            dir.write_tag(t, v)
        }
        .map_err(|e| io::Error::other(format!("tiff tag {t:?}: {e}")))
    };

    tag(Tag::ImageWidth, &[w as u32])?;
    tag(Tag::ImageLength, &[h as u32])?;
    // Emit *only* tile tags. `tiff` 0.11.3 matches on the whole
    // (StripByteCounts, StripOffsets, TileByteCounts, TileOffsets) tuple and
    // rejects any mix with `StripTileTagConflict`, so a stray RowsPerStrip
    // would make the file unreadable rather than merely redundant.
    tag(Tag::TileWidth, &[TILE_EDGE as u32])?;
    tag(Tag::TileLength, &[TILE_EDGE as u32])?;
    tag(Tag::TileOffsets, &offsets)?;
    tag(Tag::TileByteCounts, &counts)?;
    // BitsPerSample is per-channel and defaults to 1 (bilevel) when absent —
    // always written, never inferred.
    dir.write_tag(Tag::BitsPerSample, &[8u16, 8, 8][..])
        .map_err(|e| io::Error::other(format!("tiff BitsPerSample: {e}")))?;
    dir.write_tag(Tag::SamplesPerPixel, 3u16)
        .map_err(|e| io::Error::other(format!("tiff SamplesPerPixel: {e}")))?;
    dir.write_tag(Tag::PhotometricInterpretation, 2u16) // RGB
        .map_err(|e| io::Error::other(format!("tiff Photometric: {e}")))?;
    dir.write_tag(Tag::Compression, 8u16) // Adobe Deflate
        .map_err(|e| io::Error::other(format!("tiff Compression: {e}")))?;
    dir.write_tag(Tag::PlanarConfiguration, 1u16) // chunky
        .map_err(|e| io::Error::other(format!("tiff PlanarConfiguration: {e}")))?;
    dir.write_tag(Tag::SampleFormat, &[1u16, 1, 1][..]) // unsigned integer
        .map_err(|e| io::Error::other(format!("tiff SampleFormat: {e}")))?;
    if level > 0 {
        // Bit 0 = "reduced-resolution version of another image". `tiff`
        // ignores it entirely (it treats every IFD as an independent image,
        // which is what makes `seek_to_image` work for mips), but libtiff,
        // OIIO and Photoshop all read it, and without it they present a `.tx`
        // as a multi-page document rather than a mip pyramid.
        dir.write_tag(Tag::NewSubfileType, 1u32)
            .map_err(|e| io::Error::other(format!("tiff NewSubfileType: {e}")))?;
    }
    // OIIO's own marker for "this is a texture, not a picture", in Pixar's
    // private tag. Written so a file crust produced is not mistaken for an
    // unprepared image by a tool that checks.
    dir.write_tag(Tag::Unknown(33302), "Plain Texture")
        .map_err(|e| io::Error::other(format!("tiff textureformat: {e}")))?;
    dir.write_tag(Tag::Unknown(33303), "black,black")
        .map_err(|e| io::Error::other(format!("tiff wrapmodes: {e}")))?;
    dir.write_tag(Tag::Software, "crust-render")
        .map_err(|e| io::Error::other(format!("tiff Software: {e}")))?;
    // The colour space the *mip chain* was reduced in, recorded so a reader
    // can refuse a file whose pyramid does not match what it is about to
    // decode with.
    //
    // This is not redundant with the material's own `colorspace` attribute: the
    // levels are averaged in linear light and re-encoded, so a chain built for
    // sRGB and read as raw is wrong at every level above 0 while level 0 is
    // perfectly fine — a discrepancy that appears only under minification and
    // looks exactly like a filtering bug. OIIO puts its own provenance in
    // ImageDescription the same way (`oiio:SHA-1=`), so this follows the
    // convention rather than inventing a tag.
    dir.write_tag(Tag::ImageDescription, mip_space_tag(space).as_str())
        .map_err(|e| io::Error::other(format!("tiff ImageDescription: {e}")))?;

    dir.finish()
        .map_err(|e| io::Error::other(format!("tiff directory finish: {e}")))?;
    Ok(())
}
