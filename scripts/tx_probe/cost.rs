//! Probe 5: where does the per-tile time actually go?
//! Splits seek_to_image (IFD re-parse) from read_chunk (inflate + unpack).

use std::fs::File;
use std::io::BufReader;
use std::time::Instant;
use tiff::decoder::{Decoder, DecodingResult};

const PATH: &str = "big_f32.tx";

fn open() -> Decoder<BufReader<File>> {
    Decoder::new(BufReader::with_capacity(1 << 16, File::open(PATH).unwrap())).unwrap()
}

fn sum(r: &DecodingResult) -> f64 {
    match r {
        DecodingResult::F32(v) => v.iter().map(|&x| x as f64).sum(),
        _ => 0.0,
    }
}

fn main() {
    let n = 400u32;

    // A: seek_to_image(0) + read_chunk, every iteration (the naive loop).
    let mut d = open();
    let t = Instant::now();
    let mut acc = 0.0;
    for i in 0..n {
        d.seek_to_image(0).unwrap();
        acc += sum(&d.read_chunk(i).unwrap());
    }
    let a = t.elapsed() / n;
    println!("A  seek_to_image + read_chunk : {a:?}/tile  ({:.0} tiles/s)", 1.0 / a.as_secs_f64());

    // B: seek once, then read_chunk only (level pinned).
    let mut d = open();
    d.seek_to_image(0).unwrap();
    let t = Instant::now();
    for i in 0..n {
        acc += sum(&d.read_chunk(i).unwrap());
    }
    let b = t.elapsed() / n;
    println!("B  read_chunk only            : {b:?}/tile  ({:.0} tiles/s)", 1.0 / b.as_secs_f64());

    // C: just the seek, no decode.
    let mut d = open();
    let t = Instant::now();
    for _ in 0..n {
        d.seek_to_image(0).unwrap();
    }
    let c = t.elapsed() / n;
    println!("C  seek_to_image(0) alone     : {c:?}      ({:.1}x the decode cost)", c.as_secs_f64() / b.as_secs_f64());

    // D: reuse the output buffer instead of allocating per tile.
    let mut d = open();
    d.seek_to_image(0).unwrap();
    let mut buf = vec![0u8; 64 * 64 * 3 * 4];
    let t = Instant::now();
    for i in 0..n {
        d.read_chunk_bytes(i, &mut buf).unwrap();
    }
    let e = t.elapsed() / n;
    println!("D  read_chunk_bytes (reused)  : {e:?}/tile  ({:.0} tiles/s)", 1.0 / e.as_secs_f64());

    // E: alternate levels, to show the cost of level switching in a real lookup.
    let mut d = open();
    let t = Instant::now();
    for i in 0..n {
        d.seek_to_image((i % 3) as usize).unwrap();
        let tiles = d.tile_count().unwrap();
        acc += sum(&d.read_chunk(i % tiles).unwrap());
    }
    let f = t.elapsed() / n;
    println!("E  alternating 3 mip levels   : {f:?}/tile  ({:.0} tiles/s)", 1.0 / f.as_secs_f64());

    println!("\n(checksum {acc:.1}) 64x64x3 f32 tile = 48 KiB raw");
    println!("B throughput: {:.0} MiB/s of decoded texels", 48.0 / 1024.0 / b.as_secs_f64());
}
