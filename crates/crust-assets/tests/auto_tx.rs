//! The `.tx` sibling lookup and `--auto-tx` (`FileAssets::with_auto_tx`), seen
//! through the public `AssetLoader` seam.
//!
//! A streamed texture and a preloaded one are indistinguishable by type from
//! out here, so the tests make them distinguishable by content: the `.tx` is
//! converted from a *different* image than the source beside it, and the
//! colour a lookup returns says which file answered.

use crust_assets::FileAssets;
use crust_assets::tiled::{TxFormat, make_tx};
use crust_core::{AssetLoader, ColorSpace};
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("crust_auto_tx_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn png(path: &Path, rgb: [u8; 3]) {
    image::RgbImage::from_pixel(4, 4, image::Rgb(rgb))
        .save(path)
        .expect("png");
}

/// A `.tx` at `tx` whose texels are `rgb` — not what its source holds.
fn decoy_tx(dir: &Path, tx: &Path, rgb: [u8; 3]) {
    let other = dir.join("decoy_src.png");
    png(&other, rgb);
    make_tx(&other, tx, ColorSpace::Raw, TxFormat::Tiff).expect("convert decoy");
    std::fs::remove_file(&other).unwrap();
}

fn red_at(assets: &FileAssets, path: &Path, u: f32) -> f32 {
    let tex = assets.load_texture(path, ColorSpace::Raw).expect("loads");
    tex.eval(u, 0.5, 0.0)[0]
}

#[test]
fn a_tx_beside_the_texture_is_used_without_any_flag() {
    let dir = scratch("sibling");
    let src = dir.join("a.png");
    png(&src, [255, 0, 0]);
    decoy_tx(&dir, &dir.join("a.tx"), [0, 0, 255]);
    let assets = FileAssets::new();
    assert_eq!(
        red_at(&assets, &src, 0.5),
        0.0,
        "the .tx answered, not the png"
    );
}

#[test]
fn an_incomplete_udim_set_of_tx_preloads_instead_of_streaming_holes() {
    let dir = scratch("partial");
    png(&dir.join("t.1001.png"), [255, 0, 0]);
    png(&dir.join("t.1002.png"), [255, 0, 0]);
    decoy_tx(&dir, &dir.join("t.1001.tx"), [0, 0, 255]);
    let assets = FileAssets::new();
    let set = dir.join("t.<UDIM>.png");
    // Streaming would read the decoy in 1001 and black in 1002; the preloaded
    // sources are red in both.
    assert_eq!(red_at(&assets, &set, 0.5), 1.0);
    assert_eq!(red_at(&assets, &set, 1.5), 1.0);
}

#[test]
fn auto_tx_converts_missing_tiles_beside_their_sources_and_streams_them() {
    let dir = scratch("convert");
    png(&dir.join("c.1001.png"), [255, 0, 0]);
    png(&dir.join("c.1002.png"), [0, 255, 0]);
    let assets = FileAssets::new().with_auto_tx(true);
    let tex = assets
        .load_texture(&dir.join("c.<UDIM>.png"), ColorSpace::Raw)
        .expect("loads");
    assert!(dir.join("c.1001.tx").exists() && dir.join("c.1002.tx").exists());
    assert_eq!(tex.eval(0.5, 0.5, 0.0)[..3], [1.0, 0.0, 0.0]);
    assert_eq!(tex.eval(1.5, 0.5, 0.0)[..3], [0.0, 1.0, 0.0]);
    assert_eq!(assets.tx_report().0, 2);

    // A second render finds them current and converts nothing.
    let again = FileAssets::new().with_auto_tx(true);
    again
        .load_texture(&dir.join("c.<UDIM>.png"), ColorSpace::Raw)
        .expect("loads");
    assert_eq!(again.tx_report().0, 0);
}

#[test]
fn auto_tx_reconverts_a_tx_older_than_its_source() {
    let dir = scratch("stale");
    let src = dir.join("s.png");
    decoy_tx(&dir, &dir.join("s.tx"), [0, 0, 255]);
    std::thread::sleep(std::time::Duration::from_millis(20));
    png(&src, [255, 0, 0]);
    // Without the flag the stale file is still what is read (and warned about).
    assert_eq!(red_at(&FileAssets::new(), &src, 0.5), 0.0);
    // With it, the texture is reconverted from the newer source.
    let assets = FileAssets::new().with_auto_tx(true);
    assert_eq!(red_at(&assets, &src, 0.5), 1.0);
    assert_eq!(assets.tx_report().0, 1);
}

#[test]
fn auto_tx_never_converts_or_reads_a_tx_beside_a_ptex() {
    let dir = scratch("ptex");
    let ptx = dir.join("rock.ptx");
    std::fs::write(&ptx, b"not really ptex").unwrap();
    // A stray `rock.tx` must not answer for the Ptex either.
    decoy_tx(&dir, &dir.join("stray.tx"), [0, 0, 255]);
    std::fs::rename(dir.join("stray.tx"), dir.join("rock.tx")).unwrap();
    let before = std::fs::metadata(dir.join("rock.tx"))
        .unwrap()
        .modified()
        .unwrap();

    let assets = FileAssets::new().with_auto_tx(true);
    assert!(
        assets.load_texture(&ptx, ColorSpace::Raw).is_none(),
        "neither the stray .tx nor a conversion stands in for a .ptx"
    );
    assert_eq!(
        assets.tx_report(),
        (0, 0, assets.tx_report().2),
        "nothing converted or failed"
    );
    let after = std::fs::metadata(dir.join("rock.tx"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after, "the stray .tx was not rewritten");
}

#[test]
fn auto_tx_leaves_a_source_that_already_streams_alone() {
    // A tiled, mip-mapped EXR is already what a .tx provides — every ALab
    // texture is one. It streams as it is and must not be copied beside itself.
    let dir = scratch("already_tiled");
    let src_png = dir.join("seed.png");
    png(&src_png, [255, 0, 0]);
    let tiled = dir.join("albedo.exr");
    make_tx(&src_png, &tiled, ColorSpace::Raw, TxFormat::Exr).expect("tiled exr");
    std::fs::remove_file(&src_png).unwrap();

    let assets = FileAssets::new().with_auto_tx(true);
    assert_eq!(red_at(&assets, &tiled, 0.5), 1.0);
    assert!(!dir.join("albedo.tx").exists(), "no copy written");
    assert_eq!(assets.tx_report().0, 0);
}
