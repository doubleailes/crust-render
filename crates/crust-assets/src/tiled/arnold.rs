//! Crust's conversion against Arnold 7.5's, file by file: every source the
//! oracle (`scripts/arnold_tx_oracle.py`) had Arnold convert is converted by
//! crust for the same binding, and the two `.tx` files are compared level by
//! level (design decision 7: per level a mean absolute difference of at most
//! 0.002 and a maximum of at most 0.02, relative above 1). Nothing here runs
//! Arnold; the files it wrote are in `tests/data/arnold_tx`.

use super::{TILE_EDGE, TiledFile, make_tx_atomic};
use crust_core::ColorSpace;
use crust_core::color::Space;
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/arnold_tx")
}

fn cases() -> serde_json::Value {
    let text = std::fs::read_to_string(fixtures().join("cases.json")).expect("cases.json");
    serde_json::from_str(&text).expect("json")
}

fn acescg() -> Space {
    Space::named("ACEScg").expect("ACEScg")
}

/// How crust reads an Arnold `image` node's `color_space`: as a MaterialX
/// colour image with that `colorspace`, or `auto`.
fn binding(color_space: &str) -> ColorSpace {
    match color_space {
        "auto" => ColorSpace::AUTO,
        name => ColorSpace::from_mtlx_image(
            crust_core::mtlx::TexelSpace::Named(name),
            Space::LIN_REC709,
        ),
    }
}

/// One level of a `.tx`, as the tile reader hands it back: RGBA a texel (a
/// `Y` file replicated to RGB, a file without alpha reading 1.0).
struct Level {
    width: usize,
    height: usize,
    texels: Vec<[f32; 4]>,
}

/// Every level of `path`, read tile by tile as the renderer reads it.
fn read_levels(path: &Path) -> (Vec<Level>, bool) {
    let file = TiledFile::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut r = file.reader().expect("reader");
    let mut levels = Vec::new();
    for (n, li) in file.levels().iter().enumerate() {
        let mut texels = vec![[0.0f32; 4]; li.width * li.height];
        for index in 0..(li.across * li.down) as u32 {
            let tile = file.read_tile(&mut r, n, index).expect("tile");
            let (tw, th) = li.tile_size(index, TILE_EDGE);
            let (tx, ty) = (index as usize % li.across, index as usize / li.across);
            let k = tile.channels();
            let values: Vec<f32> = match tile.kind {
                super::cache::TileKind::U8 => {
                    tile.bytes.iter().map(|&b| b as f32 / 255.0).collect()
                }
                super::cache::TileKind::Half => tile
                    .bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| half::f16::from_bits(u16::from_le_bytes(*b)).to_f32())
                    .collect(),
            };
            for ly in 0..th {
                for lx in 0..tw {
                    let o = (ly * tw + lx) * k;
                    let t = &values[o..o + k];
                    let texel = [t[0], t[1], t[2], if k == 4 { t[3] } else { 1.0 }];
                    texels[(ty * TILE_EDGE + ly) * li.width + tx * TILE_EDGE + lx] = texel;
                }
            }
        }
        levels.push(Level {
            width: li.width,
            height: li.height,
            texels,
        });
    }
    (levels, file.has_alpha())
}

/// **Every level of every file, crust's against Arnold's.** For each source
/// Arnold converted through an `image` node, crust converts the same source
/// for the same binding, and the two agree: the same name, file type, sample
/// type and compression, the same level sizes (rounded down), alpha where
/// Arnold kept it, and every level's texels within decision 7's bounds.
#[test]
fn crust_writes_what_arnold_writes() {
    let cases = cases();
    let dir = std::env::temp_dir().join("crust_arnold_tx_levels");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let mut checked = 0;
    let mut worst = (0.0f64, 0.0f32, String::new());
    for case in cases["image"].as_array().expect("cases") {
        let name = case["case"].as_str().expect("case");
        // Arnold reads the `sRGB` token through a config that does not
        // define it, unconverted; crust decodes it (design decision 1).
        if name == "srgb_word" {
            continue;
        }
        let arnold = &case["tx"][0];
        let source = case["source"].as_str().expect("source");
        let src = dir.join(source);
        std::fs::copy(fixtures().join(source), &src).expect("copy source");
        let made = make_tx_atomic(
            &src,
            binding(case["color_space"].as_str().unwrap()),
            acescg(),
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            made.dst.file_name().unwrap().to_string_lossy(),
            arnold["tx"].as_str().unwrap(),
            "{name}: name"
        );

        // File type, sample type, compression.
        let unquote =
            |v: &serde_json::Value| v.as_str().unwrap_or("").trim_matches('"').to_string();
        let want_kind = match (
            arnold["type"].as_str().unwrap(),
            arnold["format"].as_str().unwrap(),
        ) {
            ("uint8", "tiff") => "8-bit, tiff",
            ("uint16", "tiff") => "16-bit, tiff",
            ("half", "openexr") if unquote(&arnold["attributes"]["compression"]) == "dwaa" => {
                "half, exr, dwaa"
            }
            ("half", "openexr") => "half, exr",
            ("float", "openexr") => "float, exr",
            other => panic!("{name}: {other:?}"),
        };
        assert_eq!(made.kind, want_kind, "{name}: file type");

        let (ours, our_alpha) = read_levels(&made.dst);
        let (theirs, their_alpha) = read_levels(&fixtures().join(arnold["tx"].as_str().unwrap()));
        assert_eq!(our_alpha, their_alpha, "{name}: alpha");
        let sizes = |l: &[Level]| l.iter().map(|l| (l.width, l.height)).collect::<Vec<_>>();
        assert_eq!(sizes(&ours), sizes(&theirs), "{name}: level sizes");
        for (n, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            let mut sum = 0.0f64;
            let mut max = 0.0f32;
            for (p, q) in a.texels.iter().zip(&b.texels) {
                for k in 0..4 {
                    let d = (p[k] - q[k]).abs() / q[k].abs().max(1.0);
                    sum += f64::from(d);
                    max = max.max(d);
                }
            }
            let mean = sum / (a.texels.len() * 4) as f64;
            assert!(
                mean <= 0.002 && max <= 0.02,
                "{name} level {n}: mean {mean}, max {max}"
            );
            if max > worst.1 {
                worst = (mean, max, format!("{name} level {n}"));
            }
        }
        checked += 1;
    }
    assert!(checked >= 30, "only {checked} cases");
    eprintln!(
        "{checked} files agree; the worst level is {} (mean {:.6}, max {:.6})",
        worst.2, worst.0, worst.1
    );
}

/// One of Arnold's tools, when Arnold is installed: from `ARNOLD_BIN`, else
/// Arnold 7.5's default install directory. `None` skips the tests that run
/// one, as the oracle scripts are skipped where their tools are missing.
pub(crate) fn arnold_tool(name: &str) -> Option<PathBuf> {
    let bin = std::env::var_os("ARNOLD_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files\Arnold-7.5.0.0-windows\bin"));
    let exe = bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    exe.is_file().then_some(exe)
}

/// `oiiotool --info -v` of `path`, as the oracle records it: the summary
/// line's type and format, then every `key: value`, `DateTime` left out.
fn oiio_info(oiiotool: &Path, path: &Path) -> std::collections::BTreeMap<String, String> {
    let out = std::process::Command::new(oiiotool)
        .args(["--info", "-v"])
        .arg(path)
        .output()
        .expect("run oiiotool");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut info = std::collections::BTreeMap::new();
    let mut lines = text.lines().skip_while(|l| !l.contains(" : "));
    let first = lines.next().expect("summary line");
    let summary = first.rsplit(" channel, ").next().expect("type and format");
    info.insert("type format".to_string(), summary.trim().to_string());
    for line in lines {
        if let Some((key, value)) = line.trim().split_once(": ")
            && key != "DateTime"
        {
            info.insert(key.to_string(), value.to_string());
        }
    }
    info
}

/// **OIIO reads crust's header as Arnold's** (Arnold-backed, skipped
/// without Arnold): for every case, `oiiotool --info -v` reports the same
/// keys with the same values on crust's file as on Arnold's, but for
/// `oiio:SHA-1` (a recorded gap) and `DateTime`. `Software` is compared with
/// its directory and quotes taken out, since the two files sit in different
/// directories.
#[test]
fn oiio_reads_crusts_header_as_arnolds() {
    let Some(oiiotool) = arnold_tool("oiiotool") else {
        eprintln!("skipped: Arnold's oiiotool is not installed (ARNOLD_BIN)");
        return;
    };
    let dir = std::env::temp_dir().join("crust_arnold_tx_headers");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let here = std::path::absolute(&dir)
        .expect("absolute")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    for case in cases()["image"].as_array().expect("cases") {
        let name = case["case"].as_str().expect("case");
        if name == "srgb_word" {
            continue;
        }
        let source = case["source"].as_str().expect("source");
        let src = dir.join(source);
        std::fs::copy(fixtures().join(source), &src).expect("copy source");
        let made = make_tx_atomic(
            &src,
            binding(case["color_space"].as_str().unwrap()),
            acescg(),
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut ours = oiio_info(&oiiotool, &made.dst);
        let mut theirs = oiio_info(
            &oiiotool,
            &fixtures().join(case["tx"][0]["tx"].as_str().unwrap()),
        );
        // `oiio:SHA-1` is a recorded gap; `openexr:chunkCount` is structure
        // the `exr` crate always writes ("not actually required, but always
        // computed"), not an OIIO attribute, and nothing compares it.
        ours.remove("openexr:chunkCount");
        let normalise = |info: &mut std::collections::BTreeMap<String, String>, dir: &str| {
            for key in ["Software", "Exif:ImageHistory"] {
                if let Some(v) = info.get_mut(key) {
                    *v = v.replace(dir, "<dir>").replace('"', "");
                }
            }
            info.remove("oiio:SHA-1");
        };
        normalise(&mut ours, &here);
        // Arnold's `Software` names the oracle's own temporary directory: the
        // directory of the first path in it, the source's.
        let theirs_dir = theirs
            .get("Software")
            .and_then(|s| s.split(" maketx ").nth(1))
            .map(|rest| {
                let source = match rest.strip_prefix("\\\"").or(rest.strip_prefix('"')) {
                    Some(quoted) => quoted.split('"').next().unwrap_or(quoted),
                    None => rest.split(' ').next().unwrap_or(rest),
                };
                source[..source.rfind('/').expect("a directory")].to_string()
            })
            .expect("Arnold's Software");
        normalise(&mut theirs, &theirs_dir);
        // Colours are crust's own arithmetic, so they agree to tolerance.
        for key in ["oiio:AverageColor", "oiio:ConstantColor"] {
            let numbers = |info: &mut std::collections::BTreeMap<String, String>| {
                info.remove(key).map(|v| {
                    v.trim_matches('"')
                        .split(',')
                        .map(|n| n.parse::<f32>().expect("a number"))
                        .collect::<Vec<_>>()
                })
            };
            let (a, b) = (numbers(&mut ours), numbers(&mut theirs));
            assert_eq!(a.is_some(), b.is_some(), "{name}: {key}");
            for (x, y) in a.unwrap_or_default().iter().zip(b.unwrap_or_default()) {
                assert!(
                    (x - y).abs() <= 1e-3 * y.abs().max(1.0),
                    "{name}: {key} {x} against {y}"
                );
            }
        }
        assert_eq!(ours, theirs, "{name}");
    }
}
