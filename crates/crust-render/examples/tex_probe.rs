//! Diagnostic: summarise the tonal range of a Ptex file or an image.
//!
//! Exists to answer one question that eyeballing a render cannot: is a Ptex
//! colour file display-encoded or already linear? The two differ by a factor
//! of ~2.5 in mean brightness, which looks like a lighting error rather than a
//! colour-space error once it is in a render. Comparing the file's own texel
//! mean against a reference render's pixel mean tells them apart.
//!
//! ```sh
//! cargo run --release -p crust-render --example tex_probe -- file.ptx
//! cargo run --release -p crust-render --example tex_probe -- image.png [x0 y0 x1 y1]
//! ```

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: tex_probe <file.ptx | image.png | dir/> [x0 y0 x1 y1]");
        std::process::exit(2);
    };
    if std::path::Path::new(path).is_dir() {
        // Capacity planning: the renderer preloads every face of every bound
        // texture and holds them for the whole render, so the peak is the sum
        // over the set. Worth knowing before starting a production stage,
        // rather than after it has been swapping for ten minutes.
        let cap = args
            .get(1)
            .and_then(|s| s.parse::<i8>().ok())
            .unwrap_or(5);
        budget_dir(path, cap);
    } else if path.ends_with(".ptx") {
        probe_ptex(path);
    } else {
        let box_ = if args.len() >= 5 {
            Some((
                args[1].parse().unwrap(),
                args[2].parse().unwrap(),
                args[3].parse().unwrap(),
                args[4].parse().unwrap(),
            ))
        } else {
            None
        };
        probe_image(path, box_);
    }
}

/// Sums what every `.ptx` under `dir` would cost resident, at a per-face
/// resolution cap of `cap` (log2 edge length, as `CRUST_PTEX_MAX_LOG2`).
///
/// Only reads headers and face tables — no pixel data — so it is fast even
/// over thousands of files, and it accounts for faces *smaller* than the cap
/// rather than assuming every face pays the maximum.
fn budget_dir(dir: &str, cap: i8) {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect_ptx(std::path::Path::new(dir), &mut files);
    files.sort();
    println!("{} .ptx under {dir}, cap {cap} ({}x{} per face)\n", files.len(), 1 << cap, 1 << cap);

    // Per top-level directory under `dir`, so a production stage's elements
    // are individually attributable.
    let mut per_group: std::collections::BTreeMap<String, (u64, u64, u64)> = Default::default();
    let mut total_faces = 0u64;
    let mut total_bytes = 0u64;
    let mut total_full = 0u64;
    let mut failed = 0u64;

    for f in &files {
        let Ok(tx) = ptex::PtexReader::open(f) else {
            failed += 1;
            continue;
        };
        let mut faces = 0u64;
        let mut bytes = 0u64;
        let mut full = 0u64;
        for info in tx.face_infos() {
            faces += 1;
            let (w, h) = (
                1u64 << info.res.ulog2.min(cap).max(0),
                1u64 << info.res.vlog2.min(cap).max(0),
            );
            // The loader stores interleaved f32 RGB.
            bytes += w * h * 3 * 4;
            full += (1u64 << info.res.ulog2.max(0)) * (1u64 << info.res.vlog2.max(0)) * 3 * 4;
        }
        let group = f
            .strip_prefix(dir)
            .ok()
            .and_then(|p| p.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()))
            .unwrap_or_else(|| "?".into());
        let e = per_group.entry(group).or_default();
        e.0 += faces;
        e.1 += bytes;
        e.2 += 1;
        total_faces += faces;
        total_bytes += bytes;
        total_full += full;
    }

    println!("{:<22} {:>10} {:>7} {:>12}", "group", "faces", "files", "resident");
    for (g, (faces, bytes, n)) in &per_group {
        println!("{:<22} {:>10} {:>7} {:>12}", g, faces, n, mib(*bytes));
    }
    println!("{:<22} {:>10} {:>7} {:>12}", "", "", "", "");
    println!("{:<22} {:>10} {:>7} {:>12}", "TOTAL", total_faces, files.len(), mib(total_bytes));
    println!("\nat full resolution this set would be {} — the cap is what makes it fit", mib(total_full));
    if failed > 0 {
        println!("{failed} file(s) could not be opened");
    }
    println!("\nfor reference, other caps:");
    for c in [3i8, 4, 5, 6] {
        // Rescaling exactly needs the per-face table again; recompute cheaply.
        let mut b = 0u64;
        for f in &files {
            if let Ok(tx) = ptex::PtexReader::open(f) {
                for info in tx.face_infos() {
                    b += (1u64 << info.res.ulog2.min(c).max(0))
                        * (1u64 << info.res.vlog2.min(c).max(0))
                        * 3
                        * 4;
                }
            }
        }
        println!("  CRUST_PTEX_MAX_LOG2={c} ({:>4}x{:<4}) {:>12}", 1 << c, 1 << c, mib(b));
    }
}

fn collect_ptx(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        // AppleDouble sidecars litter this dataset and are not Ptex files.
        if name.starts_with("._") {
            continue;
        }
        if p.is_dir() {
            collect_ptx(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("ptx") {
            out.push(p);
        }
    }
}

fn mib(bytes: u64) -> String {
    let m = bytes as f64 / (1024.0 * 1024.0);
    if m >= 1024.0 {
        format!("{:.2} GiB", m / 1024.0)
    } else {
        format!("{m:.1} MiB")
    }
}

/// Mean and histogram of every texel, sampled a face at a time.
fn probe_ptex(path: &str) {
    let mut tx = match ptex::PtexReader::open(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("open failed: {e}");
            std::process::exit(1);
        }
    };
    let n_chan = tx.num_channels();
    let dt = tx.data_type();
    let scale = dt.one_value_inv();
    println!("faces {}  channels {}  type {}", tx.num_faces(), n_chan, dt.name());

    let mut sum = 0.0f64;
    let mut count = 0u64;
    let mut hist = [0u64; 10];
    // Sample rather than read all of it: a 631 MB file is many GB inflated,
    // and a mean over every 37th face is plenty to settle a 2.5x question.
    let stride = (tx.num_faces() / 300).max(1);
    for faceid in (0..tx.num_faces()).step_by(stride) {
        let Ok(info) = tx.face_info(faceid).copied() else {
            continue;
        };
        // Read small: the mean does not care about resolution.
        let res = ptex::Res::new(info.res.ulog2.min(3), info.res.vlog2.min(3));
        let Ok(raw) = tx.get_data_at_res(faceid, res) else {
            continue;
        };
        let px = dt.size() * n_chan;
        for i in 0..(res.u() * res.v()) {
            let src = &raw[i * px..];
            for ch in 0..n_chan.min(3) {
                let v = read_channel(&src[ch * dt.size()..], dt) * scale;
                sum += v as f64;
                count += 1;
                hist[((v * 10.0) as usize).min(9)] += 1;
            }
        }
    }
    if count == 0 {
        println!("no texels read");
        return;
    }
    let mean = sum / count as f64;
    println!("texels sampled {count}");
    println!("mean raw (as stored, 0..1)          = {mean:.4}");
    println!("  -> if file is sRGB, linear mean   = {:.4}", mean.powf(2.2));
    println!("  -> if file is linear, sRGB mean   = {:.4}", mean.powf(1.0 / 2.2));
    print!("decile histogram:");
    for h in hist {
        print!(" {}", h * 100 / count.max(1));
    }
    println!();
}

fn read_channel(src: &[u8], dt: ptex::DataType) -> f32 {
    match dt {
        ptex::DataType::UInt8 => src[0] as f32,
        ptex::DataType::UInt16 => u16::from_le_bytes([src[0], src[1]]) as f32,
        ptex::DataType::Half => ptex::half_to_float(u16::from_le_bytes([src[0], src[1]])),
        ptex::DataType::Float => f32::from_le_bytes([src[0], src[1], src[2], src[3]]),
    }
}

/// Mean of an image, over a box if given, else over non-black pixels (the
/// per-element reference renders are matted against black).
fn probe_image(path: &str, box_: Option<(u32, u32, u32, u32)>) {
    let img = match image::open(path) {
        Ok(i) => i.to_rgb8(),
        Err(e) => {
            eprintln!("open failed: {e}");
            std::process::exit(1);
        }
    };
    let (w, h) = (img.width(), img.height());
    println!("{path}: {w}x{h}");
    let (x0, y0, x1, y1) = box_.unwrap_or((0, 0, w, h));
    let mut sum = [0.0f64; 3];
    let mut lin = 0.0f64;
    let mut count = 0u64;
    for y in y0..y1.min(h) {
        for x in x0..x1.min(w) {
            let p = img.get_pixel(x, y).0;
            // Skip the black matte when no explicit box was given.
            if box_.is_none() && p[0] < 8 && p[1] < 8 && p[2] < 8 {
                continue;
            }
            for c in 0..3 {
                let v = p[c] as f64 / 255.0;
                sum[c] += v;
                lin += srgb_to_linear(v);
            }
            count += 1;
        }
    }
    if count == 0 {
        println!("no pixels selected");
        return;
    }
    let n = count as f64;
    println!("pixels {count}  ({:.1}% of box)", 100.0 * n / ((x1 - x0) as f64 * (y1 - y0) as f64));
    println!(
        "mean sRGB   = ({:.4}, {:.4}, {:.4})  grey {:.4}",
        sum[0] / n,
        sum[1] / n,
        sum[2] / n,
        (sum[0] + sum[1] + sum[2]) / (3.0 * n)
    );
    println!("mean linear = {:.4}", lin / (3.0 * n));
}

fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
