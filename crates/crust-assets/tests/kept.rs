//! A loader that keeps its decodes (`FileAssets::keep_decoded`, the `crust
//! mcp` session's): a file decoded once is handed back by every later import
//! until it changes on disk, and then decoded afresh.

use crust_assets::FileAssets;
use crust_core::{AssetLoader, ColorSpace};
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("crust_kept_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A flat 8x8 PNG of `rgb`.
fn flat_png(path: &Path, rgb: [u8; 3]) {
    image::RgbImage::from_pixel(8, 8, image::Rgb(rgb))
        .save(path)
        .expect("write png");
}

fn red_of(assets: &FileAssets, path: &Path) -> f32 {
    let tex = assets
        .load_texture(path, ColorSpace::RAW)
        .expect("the png loads");
    tex.eval(0.5, 0.5, 0.0)[0]
}

/// The same file twice is decoded once; rewritten, it is decoded again and
/// the new texels are what comes back.
#[test]
fn a_rewritten_texture_is_decoded_again() {
    let dir = scratch("rewritten");
    let png = dir.join("flat.png");
    let other = dir.join("other.png");
    flat_png(&png, [51, 0, 0]);
    flat_png(&other, [0, 0, 0]);
    let assets = FileAssets::new().keep_decoded();

    let first = red_of(&assets, &png);
    red_of(&assets, &other);
    assert_eq!(assets.decodes(), 2);
    assert_eq!(assets.revalidate(), 0, "nothing changed");
    assert_eq!(red_of(&assets, &png), first);
    assert_eq!(assets.decodes(), 2, "a kept texture is not decoded again");

    // A different length, so the stamp moves even on a coarse-mtime
    // filesystem.
    image::RgbImage::from_fn(16, 8, |_, _| image::Rgb([204, 0, 0]))
        .save(&png)
        .expect("rewrite png");
    assert_eq!(assets.revalidate(), 1, "only the rewritten file is let go");
    let second = red_of(&assets, &png);
    assert_ne!(second, first, "the new texels");
    assert!((second - 0.8).abs() < 1e-6, "{second}");
    red_of(&assets, &other);
    assert_eq!(
        assets.decodes(),
        3,
        "the untouched file is not decoded twice"
    );
}

/// A loader that keeps nothing decodes on every call, as before.
#[test]
fn a_plain_loader_keeps_nothing() {
    let dir = scratch("plain");
    let png = dir.join("flat.png");
    flat_png(&png, [51, 0, 0]);
    let assets = FileAssets::new();
    red_of(&assets, &png);
    red_of(&assets, &png);
    assert_eq!(assets.decodes(), 0);
    assert_eq!(assets.revalidate(), 0);
}

/// A tile added to a `<UDIM>` set on disk is read: the set is the tiles
/// there, so the kept decode of it is let go.
#[test]
fn a_tile_added_to_a_udim_set_is_read() {
    let dir = scratch("udim");
    flat_png(&dir.join("tex.1001.png"), [51, 0, 0]);
    let set = dir.join("tex.<UDIM>.png");
    let assets = FileAssets::new().keep_decoded();
    assets
        .load_texture(&set, ColorSpace::RAW)
        .expect("the set loads");
    assert_eq!(assets.revalidate(), 0, "nothing changed");

    flat_png(&dir.join("tex.1002.png"), [0, 51, 0]);
    // A filesystem whose directory times are coarser than this test would
    // see no change; make the change unmistakable.
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::open(&dir)
        .and_then(|d| d.set_modified(later))
        .expect("touch the directory");
    assert_eq!(assets.revalidate(), 1, "the set gained a tile");
    let tex = assets.load_texture(&set, ColorSpace::RAW).expect("reloads");
    // u in [1, 2) is tile 1002.
    assert!((tex.eval(1.5, 0.5, 0.0)[1] - 0.2).abs() < 1e-6);
    assert_eq!(assets.decodes(), 2);
}

/// After a full import, what it did not ask for is let go; what it asked
/// for, kept or decoded, stays.
#[test]
fn a_sweep_lets_go_of_what_the_import_did_not_use() {
    let dir = scratch("sweep");
    let (used, unused) = (dir.join("used.png"), dir.join("unused.png"));
    flat_png(&used, [51, 0, 0]);
    flat_png(&unused, [0, 51, 0]);
    let assets = FileAssets::new().keep_decoded();
    red_of(&assets, &used);
    red_of(&assets, &unused);

    // The next import asks for `used` only.
    assets.revalidate();
    red_of(&assets, &used);
    assert_eq!(assets.sweep(), 1);
    assert_eq!(assets.sweep(), 0, "a second sweep finds nothing new");
    red_of(&assets, &used);
    assert_eq!(assets.decodes(), 2, "the used one stayed kept");
    red_of(&assets, &unused);
    assert_eq!(assets.decodes(), 3, "the swept one is decoded again");
}
