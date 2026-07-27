//! Probe 2: per-phase I/O accounting + peak RSS, on a 4096^2 float .tx (164 MiB).
//! Question: does a per-tile read stay O(tile), or does it scale with the image?

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering};

use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;

static BYTES: AtomicU64 = AtomicU64::new(0);

struct Counting<R>(R);
impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.0.read(buf)?;
        BYTES.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}
impl<R: Seek> Seek for Counting<R> {
    fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(p)
    }
}

fn mark() -> u64 {
    BYTES.swap(0, Ordering::Relaxed)
}

fn rss_kb() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    for l in s.lines() {
        if let Some(v) = l.strip_prefix("VmHWM:") {
            return v.trim().trim_end_matches(" kB").trim().parse().unwrap();
        }
    }
    0
}

fn open(path: &str, cap: usize) -> Decoder<Counting<BufReader<File>>> {
    Decoder::new(Counting(BufReader::with_capacity(
        cap,
        File::open(path).unwrap(),
    )))
    .unwrap()
}

fn main() {
    let path = "big_f32.tx";
    let file_len = std::fs::metadata(path).unwrap().len();
    println!("{path}: {:.1} MiB on disk", file_len as f64 / 1048576.0);

    let mode = std::env::args().nth(1).unwrap_or_else(|| "tiles".into());

    if mode == "whole" {
        // Baseline: decode all of MIP 0 into RAM, the way a naive loader would.
        let mut d = open(path, 1 << 16);
        let img = d.read_image().unwrap();
        let n = match &img {
            DecodingResult::F32(v) => v.len(),
            _ => 0,
        };
        println!(
            "whole-image: decoded {n} samples ({:.1} MiB), file bytes read {:.1} MiB, peak RSS {:.1} MiB",
            (n * 4) as f64 / 1048576.0,
            mark() as f64 / 1048576.0,
            rss_kb() as f64 / 1024.0
        );
        return;
    }

    // ---- phase 1: cold open (reads IFD 0 only) ----
    let mut d = open(path, 1 << 16);
    println!("open (IFD0 parse): {} B", mark());

    // ---- phase 2: seek to a MIP level (walks the IFD chain) ----
    d.seek_to_image(2).unwrap();
    let seek_bytes = mark();
    let (w, h) = d.dimensions().unwrap();
    let (tw, th) = d.chunk_dimensions();
    println!(
        "seek_to_image(2): {seek_bytes} B  -> level is {w}x{h}, tile {tw}x{th}, {} tiles",
        d.tile_count().unwrap()
    );

    // ---- phase 3: individual tile reads, scattered ----
    let across = w.div_ceil(tw);
    let raw_tile = (tw * th * 3 * 4) as f64;
    for &(tx, ty) in &[(0u32, 0u32), (1024, 512), (960, 960), (64, 1000)] {
        let idx = (ty / th) * across + (tx / tw);
        let r = d.read_chunk(idx).unwrap();
        let b = mark();
        let len = match &r {
            DecodingResult::F32(v) => v.len(),
            _ => 0,
        };
        println!(
            "  tile idx {idx:3} @({tx},{ty}): file bytes {b:7} ({:.2}x raw tile, {:.4}% of file), decoded {len} f32",
            b as f64 / raw_tile,
            100.0 * b as f64 / file_len as f64
        );
    }

    // ---- phase 4: sweep every tile of level 0, one at a time ----
    d.seek_to_image(0).unwrap();
    mark();
    let n = d.tile_count().unwrap();
    let mut buf = None;
    for i in 0..n {
        buf = Some(d.read_chunk(i).unwrap()); // keep only the latest tile alive
    }
    let sweep = mark();
    println!(
        "level-0 sweep of {n} tiles one-at-a-time: {:.1} MiB read, peak RSS {:.1} MiB (last tile {} samples)",
        sweep as f64 / 1048576.0,
        rss_kb() as f64 / 1024.0,
        match &buf {
            Some(DecodingResult::F32(v)) => v.len(),
            _ => 0,
        }
    );

    // ---- phase 5: are TileOffsets/TileByteCounts directly readable? ----
    let mut d = open(path, 1 << 16);
    let offs = d.get_tag_u64_vec(Tag::TileOffsets).unwrap();
    let counts = d.get_tag_u64_vec(Tag::TileByteCounts).unwrap();
    println!(
        "TileOffsets/TileByteCounts readable: {} entries; tile 26 at byte {} len {} (raw tile = {})",
        offs.len(),
        offs[26],
        counts[26],
        raw_tile as u64
    );
    println!(
        "  -> compressed tile sizes: min {} max {} mean {}",
        counts.iter().min().unwrap(),
        counts.iter().max().unwrap(),
        counts.iter().sum::<u64>() / counts.len() as u64
    );
    println!("final peak RSS: {:.1} MiB", rss_kb() as f64 / 1024.0);
}
