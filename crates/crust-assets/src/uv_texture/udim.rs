//! Tile-set addressing: the `<UDIM>` / `<UVTILE>` filename tokens and the
//! UDIM numbering every tile is keyed by.

use std::path::{Path, PathBuf};

/// Chart coordinates swept on each axis: 10x10 covers the 1001..1100 range
/// every DCC writes, and bounds the `<UVTILE>` sweep too, since the two
/// tokens name the same grid.
const GRID: u32 = 10;

/// The filename token that addresses a tile set, and how it spells a tile.
///
/// Two spellings, one grid: a document may use either, and both index the
/// same `(u, v)` chart coordinates. Keeping the distinction in one place is
/// what lets everything downstream — the tile key, the sampler, the 10x10
/// bound — stay written in UDIM numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum TileToken {
    /// `<UDIM>` → `1001 + u + 10·v`, e.g. `Albedo.1012.png`.
    Udim,
    /// `<UVTILE>` → `u<u+1>_v<v+1>`, e.g. `Albedo.u2_v2.png`. Both indices
    /// are 1-based, so `u1_v1` is the same tile as UDIM 1001.
    UvTile,
}

impl TileToken {
    /// The token this file name carries, or `None` for a single image.
    ///
    /// `<UDIM>` is tested first only to be deterministic about a name that
    /// carries both, which no conformant document writes.
    pub(super) fn detect(name: &str) -> Option<TileToken> {
        if name.contains("<UDIM>") {
            Some(TileToken::Udim)
        } else if name.contains("<UVTILE>") {
            Some(TileToken::UvTile)
        } else {
            None
        }
    }

    /// The file name of the tile at zero-based chart coordinates `(u, v)`.
    pub(super) fn expand(self, name: &str, u: u32, v: u32) -> String {
        match self {
            TileToken::Udim => name.replace("<UDIM>", &udim_number(u, v).to_string()),
            TileToken::UvTile => name.replace("<UVTILE>", &format!("u{}_v{}", u + 1, v + 1)),
        }
    }

    /// Every tile of the set `name` addresses that exists on disk, with its
    /// UDIM number.
    ///
    /// Only tiles present on disk are listed, so a chart with holes costs
    /// nothing for the tiles it does not use. The order is `v`-major, then
    /// `u` — ascending UDIM number — and callers rely on it: the streaming
    /// texture interns its files in this order, and the first tile listed is
    /// the one that settles an `auto` colour space.
    pub(super) fn existing_tiles(self, name: &str) -> Vec<(u32, PathBuf)> {
        (0..GRID)
            .flat_map(|v| (0..GRID).map(move |u| (u, v)))
            .filter_map(|(u, v)| {
                let p = PathBuf::from(self.expand(name, u, v));
                p.exists().then_some((udim_number(u, v), p))
            })
            .collect()
    }

    /// The token as authored, for the "nothing found" message.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            TileToken::Udim => "<UDIM>",
            TileToken::UvTile => "<UVTILE>",
        }
    }
}

/// The tiles a `<UDIM>` / `<UVTILE>` path names that exist on disk, each
/// with its UDIM number, in ascending UDIM order; `None` when the path names
/// a single image rather than a tile set.
///
/// The one sweep every consumer of a tile set uses — the preload and
/// streaming textures, `--auto-tx` and the `maketx` example — so they
/// discover the same set of tiles: a sweep that disagreed about which files
/// exist would make the two texture backends cover different parts of the
/// chart.
pub fn existing_tiles(path: &Path) -> Option<Vec<(u32, PathBuf)>> {
    let name = path.to_string_lossy();
    TileToken::detect(&name).map(|t| t.existing_tiles(&name))
}

/// The UDIM number of the tile at zero-based chart coordinates.
///
/// The internal tile key, whichever token named the file.
pub(crate) fn udim_number(u: u32, v: u32) -> u32 {
    1001 + u + 10 * v
}
