//! Probe 3: compatibility matrix over awkward real-world .tx flavours,
//! plus the OpenEXR-flavoured .tx via the `exr` crate's block reader.

use std::fs::File;
use std::io::{BufReader, Read};

use tiff::decoder::Decoder;

fn sniff(path: &str) -> &'static str {
    let mut m = [0u8; 4];
    File::open(path).unwrap().read_exact(&mut m).unwrap();
    match m {
        [0x49, 0x49, 0x2a, 0x00] | [0x4d, 0x4d, 0x00, 0x2a] => "TIFF (classic)",
        [0x49, 0x49, 0x2b, 0x00] | [0x4d, 0x4d, 0x00, 0x2b] => "BigTIFF",
        [0x76, 0x2f, 0x31, 0x01] => "OpenEXR",
        _ => "unknown",
    }
}

fn try_tiff(path: &str) {
    let mut d = match Decoder::new(BufReader::new(File::open(path).unwrap())) {
        Ok(d) => d,
        Err(e) => {
            println!("    tiff crate: OPEN FAILED: {e}");
            return;
        }
    };
    let (w, h) = d.dimensions().unwrap();
    let ct = d.colortype().unwrap();
    let (tw, th) = d.chunk_dimensions();
    let planar = d.get_tag_u32(tiff::tags::Tag::PlanarConfiguration).unwrap_or(1);
    let nlevels = {
        let mut n = 1;
        while d.more_images() {
            d.next_image().unwrap();
            n += 1;
        }
        n
    };
    let mut d = Decoder::new(BufReader::new(File::open(path).unwrap())).unwrap();
    let ntiles = d.tile_count().unwrap();
    print!(
        "    tiff crate: {w}x{h} {ct:?} tile {tw}x{th} planar={} levels={nlevels} tiles/L0={ntiles} -> ",
        if planar == 2 { "separate" } else { "contig" }
    );
    match d.read_chunk(0) {
        Ok(r) => {
            let n = match &r {
                tiff::decoder::DecodingResult::U8(v) => v.len(),
                tiff::decoder::DecodingResult::U16(v) => v.len(),
                tiff::decoder::DecodingResult::F16(v) => v.len(),
                tiff::decoder::DecodingResult::F32(v) => v.len(),
                _ => 0,
            };
            println!("read_chunk(0) OK, {n} samples");
        }
        Err(e) => println!("read_chunk(0) FAILED: {e}"),
    }
}

fn try_exr(path: &str) {
    use exr::prelude::*;

    // (a) high-level: all resolution levels at once
    match read()
        .no_deep_data()
        .all_resolution_levels()
        .all_channels()
        .all_layers()
        .all_attributes()
        .from_file(path)
    {
        Ok(img) => {
            for l in &img.layer_data {
                let lv = l.channel_data.list.first().map(|c| match &c.sample_data {
                    exr::image::Levels::Singular(_) => "singular".to_string(),
                    exr::image::Levels::Mip { level_data, .. } => {
                        format!("{} mip levels", level_data.len())
                    }
                    exr::image::Levels::Rip { level_data, .. } => {
                        format!("rip {:?}", level_data.level_count)
                    }
                });
                println!(
                    "    exr crate (all levels): {:?} channels={} {:?}",
                    l.size,
                    l.channel_data.list.len(),
                    lv
                );
            }
        }
        Err(e) => println!("    exr crate high-level FAILED: {e}"),
    }

    // (b) low-level: pull exactly ONE tile of ONE mip level via the chunk offset table
    let file = BufReader::new(File::open(path).unwrap());
    let reader = exr::block::read(file, false).unwrap();
    let headers_info: Vec<_> = reader
        .headers()
        .iter()
        .map(|h| (h.layer_size, h.blocks_increasing_y_order().count()))
        .collect();
    println!("    exr crate (blocks): headers={headers_info:?}");

    let want_level = 2usize;
    let filtered = reader
        .filter_chunks(false, |_meta, tile, _block| {
            tile.level_index == Vec2(want_level, want_level)
                && tile.tile_index == Vec2(0, 0)
        })
        .unwrap();
    let mut decompressor = exr::block::reader::ChunksReader::sequential_decompressor(filtered, false);

    let mut n = 0;
    while let Some(block) = decompressor.decompress_next_block() {
        let b = block.unwrap();
        println!(
            "      level {want_level} tile (0,0): pixel_size={:?} pos={:?} {} bytes decompressed",
            b.index.pixel_size,
            b.index.pixel_position,
            b.data.len()
        );
        n += 1;
    }
    println!("      -> {n} chunk(s) decoded for that one tile");
}

fn main() {
    for path in [
        "checker_u8.tx",
        "rgba_u8.tx",
        "sep_u8.tx",
        "ch8_f32.tx",
        "exrflavour.tx",
    ] {
        let kind = sniff(path);
        println!("\n=== {path}  [magic says: {kind}]");
        if kind.contains("TIFF") {
            try_tiff(path);
        } else {
            try_exr(path);
        }
    }
}
