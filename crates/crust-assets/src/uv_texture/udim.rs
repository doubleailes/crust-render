//! Tile-set addressing: the `<UDIM>` / `<UVTILE>` filename tokens and the
//! UDIM numbering every tile is keyed by.

use std::path::PathBuf;

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

    /// The token as authored, for the "nothing found" message.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            TileToken::Udim => "<UDIM>",
            TileToken::UvTile => "<UVTILE>",
        }
    }
}

/// One tile of a `<UDIM>` / `<UVTILE>` set that exists on disk.
pub(crate) struct TileFile {
    /// Its UDIM number ([`udim_number`]), whichever token named the file.
    pub(crate) number: u32,
    pub(crate) path: PathBuf,
}

/// Every tile of the set `name` addresses that exists on disk, in UDIM order
/// (row by row) — or nothing when `name` carries no token.
///
/// The one sweep every reader of a tile set takes — the preload path, the
/// streaming path, the `.tx` conversion — so they discover the same set: a
/// sweep that disagreed about which files exist would make two texture
/// backends cover different parts of the chart. Only tiles present on disk
/// are returned, so a chart with holes costs nothing for the tiles it does
/// not use. 10x10 covers the 1001..1100 range every DCC writes, and is what
/// bounds the `<UVTILE>` sweep too, since the two tokens name the same grid.
pub(crate) fn existing_tiles(name: &str) -> Vec<TileFile> {
    let Some(token) = TileToken::detect(name) else {
        return Vec::new();
    };
    (0..10u32)
        .flat_map(|v| (0..10u32).map(move |u| (u, v)))
        .filter_map(|(u, v)| {
            let path = PathBuf::from(token.expand(name, u, v));
            path.exists().then(|| TileFile {
                number: udim_number(u, v),
                path,
            })
        })
        .collect()
}

/// The UDIM number of the tile at zero-based chart coordinates.
///
/// The internal tile key, whichever token named the file.
pub(crate) fn udim_number(u: u32, v: u32) -> u32 {
    1001 + u + 10 * v
}
