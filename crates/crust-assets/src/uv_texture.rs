//!! UV-addressed textures, single image or UDIM set, decoded at load.
//!!
//!! The host's implementation of `crust_core::Texture2D`. It answers the same
//!! shape of problem as the Ptex decoder — a path tracer asks for texels from
//!! every worker in an unpredictable order, so everything is decoded once at
//!! load into an immutable buffer — but the addressing is different, and so is
//!! the memory arithmetic.
//!!
//! **UDIM.** MaterialX addresses a tile set as `Albedo.<UDIM>.png`, where the
//! tile number is `1001 + u + 10·v` over integer chart coordinates. The teapot's
//! ceramic body spans 14 such tiles at 4K. Expanding the token and deciding how
//! many tiles to hold is the host's job, which is why `Texture2D::eval` takes
//! *unwrapped* coordinates: `u = 3.4` has to still know it is the fourth tile.
//!
//! **And `<UVTILE>`.** MaterialX defines a second spelling of the same grid:
//! `Albedo.<UVTILE>.png`, expanding to `u1_v1` for the tile `<UDIM>` calls
//! 1001 (both indices 1-based, the Mari/Mudbox convention). It addresses the
//! same tiles by the same coordinates, so only the name on disk differs and
//! [`TileToken`] is the whole of the difference — tiles are keyed by UDIM
//! number internally whichever token named them. The document parser already
//! carries both tokens through intact (`crust-mtlx`'s `escape_udim_tokens`),
//! so failing to expand one here meant a set that loaded *no* tiles at all
//! rather than one that loaded them wrongly.
//!
//! **Why a resolution cap is not an optimisation.** Fourteen 4096² tiles is
//! 235 M texels; at 3 bytes each that is 674 MiB for one map, and the ceramic
//! alone binds four maps across two materials — before the metal's 8K handle
//! textures. Decoding them at full resolution is several gigabytes for a 640×360
//! render that resolves nothing near it. `DEFAULT_MAX_EDGE` caps each tile's
//! edge length, box-filtering down by an integer factor at load, which brings
//! the same set to tens of MiB. `CRUST_TEX_MAX` overrides it.
//!
//! **Why 8-bit storage.** These are PNGs: the file has 8 bits a channel and
//! nothing recovers precision that was never there. Keeping them as `u8` and
//! converting on lookup through a 256-entry table costs one indexed load per
//! channel and is 4x smaller than `f32`, which at this scale is the difference
//! between fitting in cache and not.
//!
//! **Except EXR, which is `f32`.** An `.exr` holds float samples, and they
//! are the reason the file is an EXR: a linear albedo quantised to 8 bits
//! bands in the shadows, and anything above 1.0 would clip. So an EXR tile is
//! decoded through the workspace's one EXR reader ([`crate::read_exr_rgb`])
//! and kept as linear `f32` RGB, 12 bytes a texel, with no decode table at
//! all. The two storages share every addressing and filtering decision; only
//! the texel fetch differs, and it is chosen once per `eval` by monomorphising
//! over [`Texel`] rather than matched per texel. The streaming path measured
//! what a per-texel match costs (`CLAUDE.md`, "the second backing must cost
//! the first one nothing").

use crust_core::{ColorSpace, Texture2D};
use std::path::Path;
use tracing::error;

/// One resolution of one tile: row-major RGB, three samples a texel —
/// display-encoded bytes (`u8`) or linear floats (`f32`).
struct Level<T> {
    pixels: Vec<T>,
    width: usize,
    height: usize,
}

/// A stored sample type, and how three of them become linear RGB.
///
/// The `u8` arm is the table lookup the preload path has always done; the
/// `f32` arm is the identity, because an `f32` tile is linear by construction.
trait Texel: Copy {
    fn linear(pixels: &[Self], o: usize, to_linear: &[f32; 256]) -> [f32; 3];
}

impl Texel for u8 {
    #[inline(always)]
    fn linear(pixels: &[u8], o: usize, to_linear: &[f32; 256]) -> [f32; 3] {
        [
            to_linear[pixels[o] as usize],
            to_linear[pixels[o + 1] as usize],
            to_linear[pixels[o + 2] as usize],
        ]
    }
}

impl Texel for f32 {
    #[inline(always)]
    fn linear(pixels: &[f32], o: usize, _: &[f32; 256]) -> [f32; 3] {
        [pixels[o], pixels[o + 1], pixels[o + 2]]
    }
}

/// One decoded UDIM tile, as a mip pyramid.
///
/// `levels[0]` is the tile as `decode_tile` produced it (already under the
/// `CRUST_TEX_MAX` cap); each further level halves both axes until both reach
/// one texel. A tile with a single level is the pre-pyramid behaviour exactly:
/// no level to select between, so `width` is ignored structurally rather than
/// by a branch, which is what `CRUST_TEX_MIP=0` relies on.
struct Tile<T> {
    /// UDIM number, `1001 + u + 10·v`.
    number: u32,
    levels: Vec<Level<T>>,
}

impl<T> Tile<T> {
    /// The tile as authored, with no coarser levels.
    fn unmipped(number: u32, pixels: Vec<T>, width: usize, height: usize) -> Tile<T> {
        Tile {
            number,
            levels: vec![Level {
                pixels,
                width,
                height,
            }],
        }
    }
}

impl Tile<f32> {
    /// [`Tile::build_pyramid`] for a linear tile: the same area-weighted
    /// reduction ([`reduce_half_linear`] shares `axis_taps` with
    /// [`reduce_half`]) with no decode or re-encode, since there is no
    /// encoding.
    fn build_pyramid_linear(&mut self) {
        loop {
            let src = self.levels.last().expect("a tile always has level 0");
            if src.width <= 1 && src.height <= 1 {
                break;
            }
            let (pixels, width, height) = reduce_half_linear(&src.pixels, src.width, src.height);
            self.levels.push(Level {
                pixels,
                width,
                height,
            });
        }
    }
}

impl Tile<u8> {
    /// Appends halved levels until both axes reach one texel.
    ///
    /// Each level is a box average of its parent over each new texel's own
    /// footprint — a plain 2x2 whenever both axes are even — computed in
    /// **linear light** and re-encoded to `u8` through `encode`; averaging
    /// display-encoded values is not averaging light, and a mip chain built
    /// that way drifts darker at every level. (The `CRUST_TEX_MAX` reduction
    /// in `decode_tile` deliberately does average in the file's encoding, to
    /// keep a capped tile matching a DCC's preview of the same file; the two
    /// conventions are recorded in `docs/color_management.md`.)
    ///
    /// Axes halve by `div_ceil`, not by `>> 1`. `decode_tile` reduces by an
    /// arbitrary integer factor, so level 0 is routinely odd — a 3000x2000
    /// source under a 1024 cap is 1000x666 — and `sample_tile` maps
    /// `x = u·width − 0.5`, so every level has to cover the whole `[0, 1]`
    /// domain. Flooring an odd axis drops its last half-texel and the level's
    /// domain slips against level 0's, which shows up as a crawl across mip
    /// transitions on a slow camera move.
    fn build_pyramid(&mut self, to_linear: &[f32; 256], encode: fn(f32) -> f32) {
        loop {
            let src = self.levels.last().expect("a tile always has level 0");
            if src.width <= 1 && src.height <= 1 {
                break;
            }
            let (pixels, width, height) =
                reduce_half(&src.pixels, src.width, src.height, to_linear, encode);
            self.levels.push(Level {
                pixels,
                width,
                height,
            });
        }
    }
}

/// The filename token that addresses a tile set, and how it spells a tile.
///
/// Two spellings, one grid: a document may use either, and both index the
/// same `(u, v)` chart coordinates. Keeping the distinction in one place is
/// what lets everything downstream — the tile key, the sampler, the 10x10
/// bound — stay written in UDIM numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TileToken {
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
    fn detect(name: &str) -> Option<TileToken> {
        if name.contains("<UDIM>") {
            Some(TileToken::Udim)
        } else if name.contains("<UVTILE>") {
            Some(TileToken::UvTile)
        } else {
            None
        }
    }

    /// The file name of the tile at zero-based chart coordinates `(u, v)`.
    fn expand(self, name: &str, u: u32, v: u32) -> String {
        match self {
            TileToken::Udim => name.replace("<UDIM>", &udim_number(u, v).to_string()),
            TileToken::UvTile => name.replace("<UVTILE>", &format!("u{}_v{}", u + 1, v + 1)),
        }
    }

    /// The token as authored, for the "nothing found" message.
    fn as_str(self) -> &'static str {
        match self {
            TileToken::Udim => "<UDIM>",
            TileToken::UvTile => "<UVTILE>",
        }
    }
}

/// Expands a `<UDIM>` / `<UVTILE>` token in `name` for chart coordinates
/// `(u, v)`, or `None` when the name carries no token.
///
/// Shared with the streaming path so the two discover the same set of tiles:
/// a sweep that disagreed about which files exist would make the two texture
/// backends cover different parts of the chart.
pub(crate) fn expand_token(name: &str, u: u32, v: u32) -> Option<String> {
    TileToken::detect(name).map(|t| t.expand(name, u, v))
}

/// The UDIM number of the tile at zero-based chart coordinates.
///
/// The internal tile key, whichever token named the file.
fn udim_number(u: u32, v: u32) -> u32 {
    1001 + u + 10 * v
}

/// The decoded tiles, in whichever sample type the file warranted.
enum Storage {
    /// Display-encoded bytes, decoded through [`UvTexture::to_linear`].
    U8(Vec<Tile<u8>>),
    /// Linear floats — an EXR source.
    F32(Vec<Tile<f32>>),
}

/// A UV-addressed texture: one image, or a tile set.
pub struct UvTexture {
    storage: Storage,
    /// Per-channel decode table, `u8` → linear `f32`. Holds the inverse sRGB
    /// EOTF for a display-encoded file and a plain `/255` otherwise, so the
    /// colour-space decision is made once at load and the lookup does not
    /// branch on it. Unused by an `f32` tile.
    to_linear: [f32; 256],
    /// The colour space the texels were decoded under, with
    /// [`ColorSpace::Auto`] already resolved against the file.
    space: ColorSpace,
    /// Representative tile size, for the load message.
    width: usize,
    height: usize,
    /// True when the file name carried a tile token (`<UDIM>` or `<UVTILE>`).
    /// A tile set addresses tiles by the integer part of `(u, v)`; a single
    /// image wraps instead, which is MaterialX's default `periodic` address
    /// mode.
    tiled: bool,
}

/// Largest tile edge kept, unless `CRUST_TEX_MAX` says otherwise.
///
/// 1024 rather than full resolution for the reason in the module note above.
/// It is a *cap*, not a resize: a smaller tile is kept as authored.
pub const DEFAULT_MAX_EDGE: usize = 1024;

/// The edge cap actually in effect, `CRUST_TEX_MAX` included.
///
/// Public and named for the same reason [`max_log2_from_env`] is on the Ptex
/// side: the cap is what answers "why is this texture only 1024 wide", so the
/// probes and the residency log read it from here rather than each re-parsing
/// the variable.
///
/// [`max_log2_from_env`]: crate::max_log2_from_env
pub fn max_edge_from_env() -> usize {
    std::env::var("CRUST_TEX_MAX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v >= 1)
        .unwrap_or(DEFAULT_MAX_EDGE)
}

/// Are mip pyramids built? `CRUST_TEX_MIP=0` keeps each tile at its single
/// capped level, which is the pre-pyramid behaviour bit for bit — a one-level
/// tile has nothing to select between — and costs a third less memory. The
/// A/B for "did the pyramid change this, or did the footprint?", paired with
/// `CRUST_RAY_CONES=0` on the other side.
fn mip_enabled() -> bool {
    std::env::var("CRUST_TEX_MIP").as_deref() != Ok("0")
}

impl UvTexture {
    /// Opens `path` — a single image, or a tile set when the name carries a
    /// `<UDIM>` or `<UVTILE>` token — decoding every tile present on disk at
    /// or below the `CRUST_TEX_MAX` edge cap. `None` when nothing could be
    /// decoded.
    pub fn open(path: &Path, space: ColorSpace) -> Option<UvTexture> {
        UvTexture::open_with(path, space, mip_enabled())
    }

    /// [`UvTexture::open`] with the mip decision passed in rather than read
    /// from the environment.
    ///
    /// `mip = false` is exactly what `CRUST_TEX_MIP=0` produces: one level per
    /// tile, a third less memory, and `eval`'s `width` ignored structurally.
    /// Spelled out as an argument so a caller comparing the two — a test, a
    /// probe — does not have to mutate a process-global the rest of the
    /// program is reading.
    pub fn open_with(path: &Path, space: ColorSpace, mip: bool) -> Option<UvTexture> {
        let max_edge = max_edge_from_env();

        let name = path.to_string_lossy().into_owned();
        let token = TileToken::detect(&name);
        // Decided by the name, not per tile: a UDIM set is one format, and
        // letting each tile pick would need a storage per tile.
        let is_exr = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("exr"));
        if is_exr {
            return UvTexture::open_exr(path, token, space, mip, max_edge);
        }
        let mut tiles = Vec::new();
        // `Auto` is resolved from the first tile that decodes: the pixel
        // format is what the UsdUVTexture rule asks about, and `decode_tile`
        // is the last place that still knows it.
        let mut resolved = None;
        let mut decode = |p: &Path, number: u32| {
            let (tile, format) = decode_tile(p, number, max_edge)?;
            resolved.get_or_insert_with(|| space.resolve_auto(format.0, format.1));
            Some(tile)
        };
        if let Some(token) = token {
            // Only tiles that exist on disk are opened, so a chart with holes
            // costs nothing for the tiles it does not use. 10x10 covers the
            // 1001..1100 range every DCC writes — and is what bounds the
            // `<UVTILE>` sweep too, since the two tokens name the same grid.
            for v in 0..10u32 {
                for u in 0..10u32 {
                    let candidate = token.expand(&name, u, v);
                    let p = Path::new(&candidate);
                    if !p.exists() {
                        continue;
                    }
                    if let Some(t) = decode(p, udim_number(u, v)) {
                        tiles.push(t);
                    }
                }
            }
            if tiles.is_empty() {
                error!(
                    "No tiles found for {} ({} expanded over the 10x10 grid)",
                    path.display(),
                    token.as_str()
                );
                return None;
            }
        } else {
            tiles.push(decode(path, udim_number(0, 0))?);
        }
        let space = resolved.unwrap_or(space);

        let to_linear = to_linear_table(space);
        if mip {
            let encode = encode_fn(space);
            for t in &mut tiles {
                t.build_pyramid(&to_linear, encode);
            }
        }
        let (width, height) = (tiles[0].levels[0].width, tiles[0].levels[0].height);
        Some(UvTexture {
            storage: Storage::U8(tiles),
            to_linear,
            space,
            width,
            height,
            tiled: token.is_some(),
        })
    }

    /// The EXR half of [`UvTexture::open_with`]: the same sweep, into `f32`
    /// tiles.
    ///
    /// `Auto` resolves to raw — an EXR is float, and the UsdUVTexture rule
    /// marks only 8-bit images as sRGB. An *explicit* curve is still honoured
    /// (a float file of display-encoded values is unusual, not invalid), and
    /// applied here in `f32`, so a stored tile is linear whatever was asked
    /// for and the lookup needs no table.
    fn open_exr(
        path: &Path,
        token: Option<TileToken>,
        space: ColorSpace,
        mip: bool,
        max_edge: usize,
    ) -> Option<UvTexture> {
        let space = space.resolve_auto(false, 3);
        let mut tiles = Vec::new();
        if let Some(token) = token {
            let name = path.to_string_lossy().into_owned();
            for v in 0..10u32 {
                for u in 0..10u32 {
                    let candidate = token.expand(&name, u, v);
                    let p = Path::new(&candidate);
                    if !p.exists() {
                        continue;
                    }
                    if let Some(t) = decode_exr_tile(p, udim_number(u, v), max_edge, space) {
                        tiles.push(t);
                    }
                }
            }
            if tiles.is_empty() {
                error!(
                    "No tiles found for {} ({} expanded over the 10x10 grid)",
                    path.display(),
                    token.as_str()
                );
                return None;
            }
        } else {
            tiles.push(decode_exr_tile(path, udim_number(0, 0), max_edge, space)?);
        }
        if mip {
            for t in &mut tiles {
                t.build_pyramid_linear();
            }
        }
        let (width, height) = (tiles[0].levels[0].width, tiles[0].levels[0].height);
        Some(UvTexture {
            storage: Storage::F32(tiles),
            to_linear: to_linear_table(ColorSpace::Raw),
            space,
            width,
            height,
            tiled: token.is_some(),
        })
    }

    /// Bytes held, for the load-time report. Counts every mip level, so a
    /// full pyramid reports 4/3 of its base.
    pub fn bytes(&self) -> usize {
        fn sum<T>(tiles: &[Tile<T>]) -> usize {
            tiles
                .iter()
                .flat_map(|t| t.levels.iter())
                .map(|l| l.pixels.len() * std::mem::size_of::<T>())
                .sum()
        }
        match &self.storage {
            Storage::U8(t) => sum(t),
            Storage::F32(t) => sum(t),
        }
    }

    /// Mip levels the first tile holds — 1 when no pyramid was built.
    pub fn level_count(&self) -> usize {
        match &self.storage {
            Storage::U8(t) => t.first().map_or(0, |t| t.levels.len()),
            Storage::F32(t) => t.first().map_or(0, |t| t.levels.len()),
        }
    }

    /// Tiles decoded — 1 for a single image.
    pub fn tile_count(&self) -> usize {
        match &self.storage {
            Storage::U8(t) => t.len(),
            Storage::F32(t) => t.len(),
        }
    }

    /// Whether the texels are held as linear `f32` (an EXR source) rather
    /// than as decoded-on-lookup bytes.
    pub fn is_float(&self) -> bool {
        matches!(self.storage, Storage::F32(_))
    }

    /// The colour space the texels were decoded under, `Auto` resolved.
    pub fn color_space(&self) -> ColorSpace {
        self.space
    }

    /// Size of the first tile, representative of the set.
    pub fn tile_size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Trilinear lookup inside one tile, at coordinates already reduced to
    /// `[0, 1)` and over a footprint `width` wide *in tile units*.
    ///
    /// The level is `log2(width · texels_across)`: a footprint covering one
    /// texel of level 0 reads level 0, one covering two reads level 1, and so
    /// on. The two bracketing levels are sampled bilinearly and blended, so a
    /// surface receding from the camera crosses mip levels smoothly instead
    /// of stepping.
    ///
    /// `width <= 0` — a caller with no derivatives, or `CRUST_RAY_CONES=0` —
    /// and a tile with no pyramid both short-circuit to a single bilinear tap
    /// on level 0, which is bit-identical to what this did before it had
    /// levels at all.
    fn sample_tile<T: Texel>(&self, t: &Tile<T>, u: f32, v: f32, width: f32) -> [f32; 4] {
        if t.levels.len() == 1 || width <= 0.0 {
            return self.sample_level(&t.levels[0], u, v);
        }
        // Measured against the widest axis: an isotropic footprint over an
        // anisotropic tile is minified most where the texels are densest, and
        // reading the coarser of the two is the choice that does not alias.
        let across = t.levels[0].width.max(t.levels[0].height) as f32;
        let texels = width * across;
        // Magnification — the footprint fits inside one texel — is the common
        // case in practice and its answer is level 0 whatever the `log2` says.
        // Taking it here rather than through the clamp is worth having: the
        // `log2` is otherwise paid on every fetch of every texture that is
        // being magnified, which measured ~9% of render on a scene whose
        // output does not change at all.
        if texels <= 1.0 {
            return self.sample_level(&t.levels[0], u, v);
        }
        let lod = texels.log2();
        let top = (t.levels.len() - 1) as f32;
        let lod = lod.clamp(0.0, top);
        let lo = lod.floor();
        let frac = lod - lo;
        let a = self.sample_level(&t.levels[lo as usize], u, v);
        if frac <= 0.0 {
            return a;
        }
        let b = self.sample_level(&t.levels[(lo as usize + 1).min(t.levels.len() - 1)], u, v);
        let mut out = [0.0f32; 4];
        for k in 0..3 {
            out[k] = a[k] + (b[k] - a[k]) * frac;
        }
        out[3] = 1.0;
        out
    }

    /// Bilinear lookup inside one mip level, at coordinates already reduced
    /// to `[0, 1)`.
    ///
    /// Bilinear rather than nearest because these charts are magnified: a 4K
    /// tile capped to 1024 covers a few hundred pixels of the framing, so the
    /// texel grid is plainly visible under point sampling — the artefact the
    /// dome light's own nearest-texel sampling is still criticised for in
    /// `CLAUDE.md`.
    fn sample_level<T: Texel>(&self, t: &Level<T>, u: f32, v: f32) -> [f32; 4] {
        // Image rows run top-down while `v` grows upward, the same flip the
        // rest of the graphics world applies between UV and raster space.
        let x = u * t.width as f32 - 0.5;
        let y = (1.0 - v) * t.height as f32 - 0.5;
        let x0 = x.floor();
        let y0 = y.floor();
        let (fx, fy) = (x - x0, y - y0);
        let clampi = |i: f32, n: usize| (i.max(0.0) as usize).min(n.saturating_sub(1));
        let (x0i, y0i) = (clampi(x0, t.width), clampi(y0, t.height));
        let (x1i, y1i) = (clampi(x0 + 1.0, t.width), clampi(y0 + 1.0, t.height));
        let texel = |xi: usize, yi: usize| {
            let o = (yi * t.width + xi) * 3;
            T::linear(&t.pixels, o, &self.to_linear)
        };
        let (a, b, c, d) = (
            texel(x0i, y0i),
            texel(x1i, y0i),
            texel(x0i, y1i),
            texel(x1i, y1i),
        );
        let mut out = [0.0f32; 4];
        for k in 0..3 {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bot = c[k] + (d[k] - c[k]) * fx;
            out[k] = top + (bot - top) * fy;
        }
        out[3] = 1.0;
        out
    }
}

impl UvTexture {
    /// [`Texture2D::eval`] over one storage's tiles, monomorphised per
    /// sample type so the storage is matched once per lookup, not per texel.
    #[inline(always)]
    fn eval_tiles<T: Texel>(&self, tiles: &[Tile<T>], u: f32, v: f32, width: f32) -> [f32; 4] {
        if self.tiled {
            let (tu, tv) = (u.floor(), v.floor());
            // Outside the 10x10 tile grid there is no tile by definition;
            // black rather than a wrapped guess, so a mis-scaled chart looks
            // wrong instead of plausibly tiled.
            if !(0.0..10.0).contains(&tu) || !(0.0..10.0).contains(&tv) {
                return [0.0, 0.0, 0.0, 1.0];
            }
            let number = udim_number(tu as u32, tv as u32);
            match tiles.iter().find(|t| t.number == number) {
                Some(t) => self.sample_tile(t, u - tu, v - tv, width),
                None => [0.0, 0.0, 0.0, 1.0],
            }
        } else {
            // MaterialX's default address mode is `periodic`.
            let wrap = |x: f32| x - x.floor();
            self.sample_tile(&tiles[0], wrap(u), wrap(v), width)
        }
    }
}

impl Texture2D for UvTexture {
    fn eval(&self, u: f32, v: f32, width: f32) -> [f32; 4] {
        if !u.is_finite() || !v.is_finite() {
            return [0.0, 0.0, 0.0, 1.0];
        }
        // A non-finite width is the caller's bug; point-sample rather than
        // propagate a NaN into a level index.
        let width = if width.is_finite() { width } else { 0.0 };
        match &self.storage {
            Storage::U8(tiles) => self.eval_tiles(tiles, u, v, width),
            Storage::F32(tiles) => self.eval_tiles(tiles, u, v, width),
        }
    }
}

/// Decodes one image file, box-filtering it down so neither edge exceeds
/// `max_edge`.
///
/// The reduction factor is an integer chosen so the result is the largest
/// power-of-two-ish fit under the cap, and every source texel contributes
/// exactly once — a straight box average, not a subsample. Point-decimating
/// a 4K albedo to 1024 would alias its high-frequency detail into the render
/// as fixed-pattern noise that no amount of spp removes.
///
/// Also returns the source's pixel format as `(eight_bit, channels)` — what
/// [`ColorSpace::resolve_auto`] asks about, and gone once `to_rgb8` has run.
fn decode_tile(path: &Path, number: u32, max_edge: usize) -> Option<(Tile<u8>, (bool, u8))> {
    let mut reader = image::ImageReader::open(path)
        .map_err(|e| error!("Texture decode failed for {}: {e}", path.display()))
        .ok()?
        .with_guessed_format()
        .map_err(|e| error!("Texture decode failed for {}: {e}", path.display()))
        .ok()?;
    // Same reasoning as the environment decoder: these are trusted, locally
    // authored assets and an 8K texture exceeds the default allocation limit.
    reader.no_limits();
    let decoded = reader
        .decode()
        .map_err(|e| error!("Texture decode failed for {}: {e}", path.display()))
        .ok()?;
    let color = decoded.color();
    let format = (
        color.bytes_per_pixel() == color.channel_count(),
        color.channel_count(),
    );
    let img = decoded.to_rgb8();
    let (sw, sh) = (img.width() as usize, img.height() as usize);
    if sw == 0 || sh == 0 {
        return None;
    }
    let factor = (sw.div_ceil(max_edge)).max(sh.div_ceil(max_edge)).max(1);
    if factor == 1 {
        return Some((Tile::unmipped(number, img.into_raw(), sw, sh), format));
    }
    let (w, h) = ((sw / factor).max(1), (sh / factor).max(1));
    let src = img.as_raw();
    let mut pixels = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 3];
            let mut n = 0u32;
            for dy in 0..factor {
                let sy = y * factor + dy;
                if sy >= sh {
                    break;
                }
                for dx in 0..factor {
                    let sx = x * factor + dx;
                    if sx >= sw {
                        break;
                    }
                    let o = (sy * sw + sx) * 3;
                    acc[0] += src[o] as u32;
                    acc[1] += src[o + 1] as u32;
                    acc[2] += src[o + 2] as u32;
                    n += 1;
                }
            }
            let o = (y * w + x) * 3;
            // Averaged in the file's own encoding, not in linear light. That
            // is what a mip pyramid in an 8-bit pipeline does, and matching it
            // keeps a downsampled albedo the same brightness as the DCC's
            // preview of the same file.
            for k in 0..3 {
                pixels[o + k] = (acc[k] / n.max(1)) as u8;
            }
        }
    }
    Some((Tile::unmipped(number, pixels, w, h), format))
}

/// [`decode_tile`] for an EXR: linear `f32` RGB, box-filtered under the same
/// `max_edge` cap by the same integer factor.
///
/// The reduction averages in the file's own encoding exactly as the `u8` one
/// does — which for a float file *is* linear light, so here the resize and the
/// mip chain agree. An explicit non-raw `space` is applied once, before the
/// reduction, leaving every stored value linear.
fn decode_exr_tile(
    path: &Path,
    number: u32,
    max_edge: usize,
    space: ColorSpace,
) -> Option<Tile<f32>> {
    let (mut src, sw, sh) = crate::read_exr_rgb(path)?;
    if sw == 0 || sh == 0 {
        return None;
    }
    if space != ColorSpace::Raw {
        for c in &mut src {
            *c = crate::to_linear(space, *c);
        }
    }
    let factor = (sw.div_ceil(max_edge)).max(sh.div_ceil(max_edge)).max(1);
    if factor == 1 {
        return Some(Tile::unmipped(number, src, sw, sh));
    }
    let (w, h) = ((sw / factor).max(1), (sh / factor).max(1));
    let mut pixels = vec![0.0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 3];
            let mut n = 0u32;
            for dy in 0..factor {
                let sy = y * factor + dy;
                if sy >= sh {
                    break;
                }
                for dx in 0..factor {
                    let sx = x * factor + dx;
                    if sx >= sw {
                        break;
                    }
                    let o = (sy * sw + sx) * 3;
                    for k in 0..3 {
                        acc[k] += src[o + k];
                    }
                    n += 1;
                }
            }
            let o = (y * w + x) * 3;
            for k in 0..3 {
                pixels[o + k] = acc[k] / n.max(1) as f32;
            }
        }
    }
    Some(Tile::unmipped(number, pixels, w, h))
}

/// Which source texels one destination texel of an axis reduction covers, and
/// how much of each it covers.
///
/// A destination texel is an equal-width slice of the `[0, 1]` domain — that
/// is exactly how `sample_level` reconstructs it, mapping
/// `x = u * width - 0.5` — so texel `j` of a `src -> dst` reduction owns the
/// source interval `[j·s, (j+1)·s]` for `s = src/dst`, and its value is the
/// average of the source *over that interval*. Weights are the overlaps.
///
/// Three taps is the ceiling, not a guess: `dst` is `src.div_ceil(2)`, so
/// `s <= 2`, and an interval of length at most 2 meets at most three unit
/// cells (worst case `src = 5`, whose middle texel spans `[5/3, 10/3]` and
/// touches source texels 1, 2 and 3). Checked for every `src` up to 9000.
#[derive(Clone, Copy, Debug)]
struct Tap {
    /// First source texel this destination texel overlaps.
    start: usize,
    /// How many it overlaps: 1, 2 or 3.
    count: usize,
    /// Overlap per source texel, **unnormalised** — see [`weighted`].
    weight: [f32; 3],
}

/// The taps for one axis, and the weight each destination texel totals.
///
/// Built once per axis per level rather than per texel: every row of a level
/// resamples its columns identically, and there are only `dst` of them.
fn axis_taps(src: usize, dst: usize) -> (Vec<Tap>, f32) {
    // **The bounds are `j * src / dst`, not `lo + s`, and they are `f64`.**
    // Accumulating `lo + s` in `f32` lets the interval drift off the end of
    // the axis: at `src = 1795` the last texel came out covering 1.99878
    // source texels against the 1.99889 every other texel covered, so
    // dividing them all by one total biased that texel by 5e-5. Computing
    // each bound from its own integers instead makes `hi(j)` and `lo(j + 1)`
    // the same expression — the taps tile with no gap and no overlap — and
    // makes the last `hi` exactly `src`, since the quotient is an integer and
    // IEEE division returns it exactly. `f64` because `j * src` passes 2^24
    // for a large level, which is where `f32` stops counting integers.
    let (fsrc, fdst) = (src as f64, dst as f64);
    let mut taps = Vec::with_capacity(dst);
    for j in 0..dst {
        let lo = j as f64 * fsrc / fdst;
        let hi = (j + 1) as f64 * fsrc / fdst;
        let mut tap = Tap {
            start: (lo as usize).min(src - 1),
            count: 0,
            weight: [0.0; 3],
        };
        let mut i = tap.start;
        while i < src && (i as f64) < hi {
            let overlap = hi.min((i + 1) as f64) - lo.max(i as f64);
            if overlap > 0.0 {
                debug_assert!(tap.count < 3, "an axis tap cannot reach four texels");
                if tap.count < 3 {
                    tap.weight[tap.count] = overlap as f32;
                    tap.count += 1;
                }
            }
            i += 1;
        }
        // Unreachable while `dst <= src` — but a zero-weight texel would
        // divide by zero below, so it falls back to its own start texel whole.
        if tap.count == 0 {
            tap.weight[0] = 1.0;
            tap.count = 1;
        }
        taps.push(tap);
    }
    // The same for every destination texel, the taps tiling the axis. Summed
    // from the first texel's weights rather than from `src / dst` so it is
    // the `f32` sum the inner loop actually accumulates against — on an even
    // axis that is exactly `2.0`, which is what keeps the even case exact.
    let sum: f32 = taps[0].weight[..taps[0].count].iter().sum();
    (taps, sum)
}

/// One destination texel: the source over its footprint, area-weighted.
///
/// **`total` divides once, at the end.** On an even axis every overlap is
/// exactly `1.0` and the axis sum exactly `2.0`, so `total` is exactly `4.0`
/// and this reduces to `(((a + b) + c) + d) / 4.0` in row-major order — bit
/// for bit what the old `0.25 * (a + b + c + d)` produced. That is what keeps
/// every power-of-two texture, and with it every checked-in golden and the
/// streamed-versus-preloaded invariant, exactly where it was. Pre-normalising
/// the weights instead would spend four roundings where this spends one, and
/// the even case would drift.
#[inline]
fn weighted(row: &Tap, col: &Tap, total: f32, at: impl Fn(usize, usize) -> f32) -> f32 {
    let mut acc = 0.0;
    for dy in 0..row.count {
        let wy = row.weight[dy];
        for dx in 0..col.count {
            acc += wy * col.weight[dx] * at(col.start + dx, row.start + dy);
        }
    }
    acc / total
}

/// One mip level from the one above it: a box average over each destination
/// texel's own footprint, in linear light, re-encoded to `u8` in the file's
/// own space. On an even axis that footprint is exactly 2x2.
///
/// Shared by the in-memory pyramid ([`Tile::build_pyramid`]) and the `.tx`
/// writer ([`crate::tiled::write_tx`]) **on purpose**. A streamed render and a
/// preloaded one are supposed to agree texel for texel, and the only way to be
/// sure of that is for the two paths to run the same code rather than two
/// copies of the same intent.
///
/// Averaging in linear and not in the file's encoding is the point: summing
/// display-encoded values is not summing light, and a chain built that way
/// drifts darker at every level (a black/white checker comes out at 0.21
/// instead of 0.5). Note that the `CRUST_TEX_MAX` reduction in `decode_tile`
/// deliberately does the opposite — it is a *resize*, meant to match a DCC's
/// preview of the same file, not a filter. `docs/color_management.md` records
/// both conventions.
///
/// Axes halve by `div_ceil`, never `>> 1`: level 0 is routinely odd (an
/// arbitrary integer `CRUST_TEX_MAX` factor, or an odd authored size), and the
/// samplers map `x = u * width - 0.5`, so every level has to span the whole
/// `[0, 1]` domain. Flooring an odd axis drops its last half-texel and that
/// level's domain slips against level 0's — a crawl across mip transitions on
/// a slow camera move.
///
/// **On an odd axis that makes the reduction a resample, not a 2x2 box**, and
/// it is weighted by area — see [`axis_taps`]. Taking the 2x2 with the source
/// index clamped to the last column instead, which is what this did before,
/// hands the trailing column a third of the level's weight where it is owed a
/// fifth: a 5-wide row of `[250, 200, 150, 100, 50]` came out
/// `[225, 125, 50]`, mean 133 against the source's 150, and the drift
/// compounds at every level. A 25-wide tile lit only at its right edge
/// bottomed out **6x too bright**, and the same tile lit at its *left* edge
/// too dark — the clamp is at one end, so the two disagreed by 8x.
pub(crate) fn reduce_half(
    src: &[u8],
    sw: usize,
    sh: usize,
    to_linear: &[f32; 256],
    encode: fn(f32) -> f32,
) -> (Vec<u8>, usize, usize) {
    let (w, h) = (sw.div_ceil(2), sh.div_ceil(2));
    let (cols, xsum) = axis_taps(sw, w);
    let (rows, ysum) = axis_taps(sh, h);
    // One divisor for the whole level: the taps tile each axis exactly, so
    // every destination texel carries the same total weight.
    let total = ysum * xsum;
    let mut pixels = vec![0u8; w * h * 3];
    for (y, row) in rows.iter().enumerate() {
        for (x, col) in cols.iter().enumerate() {
            let o = (y * w + x) * 3;
            for k in 0..3 {
                let at = |xi: usize, yi: usize| to_linear[src[(yi * sw + xi) * 3 + k] as usize];
                let mean = weighted(row, col, total, at);
                pixels[o + k] = (encode(mean) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            }
        }
    }
    (pixels, w, h)
}

/// The same reduction for data that is **already linear**: the same
/// area-weighted box average of `f32` RGB, with no decode and no re-encode
/// because there is no encoding.
///
/// Deliberately written next to [`reduce_half`] rather than generalised over
/// the sample type. The two have to agree on everything *except* the transfer
/// curve — the `div_ceil` halving, the [`axis_taps`] weighting, the order the
/// taps are summed in — and an EXR-backed `.tx` and a TIFF-backed one that
/// disagreed on odd levels would each be internally consistent and produce
/// different images, which is the hardest kind of disagreement to see. Keeping
/// them adjacent is what makes a change to one an obvious omission in the
/// other; `the_two_reducers_agree_on_an_odd_level` is what makes it a failing
/// test rather than a reading exercise.
///
/// Axes halve by `div_ceil`, never `>> 1`, for the reason [`reduce_half`]
/// records: the samplers map `x = u * width - 0.5`, so flooring an odd axis
/// drops its last half-texel and that level's domain slips against level 0's.
pub(crate) fn reduce_half_linear(src: &[f32], sw: usize, sh: usize) -> (Vec<f32>, usize, usize) {
    let (w, h) = (sw.div_ceil(2), sh.div_ceil(2));
    let (cols, xsum) = axis_taps(sw, w);
    let (rows, ysum) = axis_taps(sh, h);
    let total = ysum * xsum;
    let mut pixels = vec![0.0f32; w * h * 3];
    for (y, row) in rows.iter().enumerate() {
        for (x, col) in cols.iter().enumerate() {
            let o = (y * w + x) * 3;
            for k in 0..3 {
                let at = |xi: usize, yi: usize| src[(yi * sw + xi) * 3 + k];
                pixels[o + k] = weighted(row, col, total, at);
            }
        }
    }
    (pixels, w, h)
}

/// The transfer function that re-encodes a linear value back to the file's
/// own space — the inverse of [`to_linear_table`], used only when averaging a
/// mip level.
///
/// A function rather than a table because the input is a continuous average,
/// not one of 256 stored values; it runs once per texel of levels 1 and up,
/// which is a third of the base and only at load.
pub(crate) fn encode_fn(space: ColorSpace) -> fn(f32) -> f32 {
    // Matched on the variant rather than on `gamma()`, so a new colour space
    // is a compile error here instead of silently taking the `Raw` arm and
    // storing linear values in a display-encoded table.
    match space {
        ColorSpace::Srgb => crate::linear_to_srgb,
        ColorSpace::Gamma22 => |c: f32| c.max(0.0).powf(1.0 / 2.2),
        ColorSpace::Gamma18 => |c: f32| c.max(0.0).powf(1.0 / 1.8),
        // `Auto` is resolved before a pyramid is built; unresolved, it is raw.
        ColorSpace::Raw | ColorSpace::Auto => |c: f32| c,
    }
}

/// The 256-entry decode table for one colour space.
///
/// The files are 8-bit, so every possible stored value is one of 256 — the
/// transfer function is evaluated once per level at load rather than per
/// texel fetch, and nothing recovers precision that was never in the file.
///
/// The three curves are deliberately distinct. MaterialX's `g22_rec709` and
/// `g18_rec709` are pure power laws; sRGB's EOTF is piecewise, with a linear
/// toe that keeps near-black values well above the power law (up to 19x at
/// 0.01 — `docs/color_management.md` tabulates it). Collapsing them into one
/// curve is wrong in the shadows for 2.2 and wrong everywhere for 1.8.
pub(crate) fn to_linear_table(space: ColorSpace) -> [f32; 256] {
    let mut table = [0.0f32; 256];
    for (i, v) in table.iter_mut().enumerate() {
        *v = crate::to_linear(space, i as f32 / 255.0);
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a 1x1 PNG of a flat colour, creating parent directories.
    ///
    /// One texel is enough: these tests are about *which file* a coordinate
    /// reaches, so the colour is the tile's identity and bilinear filtering
    /// across a uniform tile returns it exactly.
    fn write_tile(path: &Path, rgb: [u8; 3]) {
        std::fs::create_dir_all(path.parent().unwrap()).expect("temp dir");
        image::RgbImage::from_pixel(1, 1, image::Rgb(rgb))
            .save(path)
            .expect("write png");
    }

    /// A scratch directory of its own per test, so the parallel test runner
    /// cannot have one test's tiles satisfy another's glob.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("crust_uv_texture_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// The channel values a tile is written with, recovered from a lookup.
    /// Raw decode, so the round trip is exact.
    fn sampled(tex: &UvTexture, u: f32, v: f32) -> [u8; 3] {
        let px = tex.eval(u, v, 0.0);
        [
            (px[0] * 255.0).round() as u8,
            (px[1] * 255.0).round() as u8,
            (px[2] * 255.0).round() as u8,
        ]
    }

    #[test]
    fn a_udim_set_loads_and_addresses_by_tile() {
        let dir = scratch("udim");
        // 1001 is (0,0), 1002 is (1,0), 1011 is (0,1) — and 1012 is left off
        // disk, so the set has a hole.
        write_tile(&dir.join("a.1001.png"), [10, 0, 0]);
        write_tile(&dir.join("a.1002.png"), [20, 0, 0]);
        write_tile(&dir.join("a.1011.png"), [30, 0, 0]);

        let tex = UvTexture::open(&dir.join("a.<UDIM>.png"), ColorSpace::Raw)
            .expect("a <UDIM> set must load the tiles that exist");
        assert_eq!(tex.tile_count(), 3, "only the tiles on disk are decoded");
        assert_eq!(sampled(&tex, 0.5, 0.5), [10, 0, 0]);
        assert_eq!(sampled(&tex, 1.5, 0.5), [20, 0, 0]);
        assert_eq!(sampled(&tex, 0.5, 1.5), [30, 0, 0]);
    }

    #[test]
    fn a_uvtile_set_loads_and_addresses_by_the_same_tile() {
        // The regression this pins: `<UVTILE>` reached `open` intact (the
        // document parser escapes and restores it) and was then never
        // expanded, so the literal name was opened as a single image, failed,
        // and the whole set loaded *nothing*.
        let dir = scratch("uvtile");
        // Both indices are 1-based: u1_v1 is the tile <UDIM> numbers 1001.
        write_tile(&dir.join("a.u1_v1.png"), [10, 0, 0]);
        write_tile(&dir.join("a.u2_v1.png"), [20, 0, 0]);
        write_tile(&dir.join("a.u1_v2.png"), [30, 0, 0]);

        let tex = UvTexture::open(&dir.join("a.<UVTILE>.png"), ColorSpace::Raw)
            .expect("a <UVTILE> set must load like a <UDIM> one");
        assert_eq!(tex.tile_count(), 3);
        // Identical coordinates to the <UDIM> test above: the two tokens
        // spell the same grid, so addressing must not depend on which named
        // the files.
        assert_eq!(sampled(&tex, 0.5, 0.5), [10, 0, 0]);
        assert_eq!(sampled(&tex, 1.5, 0.5), [20, 0, 0]);
        assert_eq!(sampled(&tex, 0.5, 1.5), [30, 0, 0]);
    }

    #[test]
    fn a_missing_tile_reads_black_rather_than_a_neighbour() {
        let dir = scratch("holes");
        write_tile(&dir.join("a.u1_v1.png"), [10, 20, 30]);
        let tex = UvTexture::open(&dir.join("a.<UVTILE>.png"), ColorSpace::Raw).expect("loads");

        // A hole inside the grid, and coordinates off the grid entirely.
        // Both are black, so a mis-scaled chart looks wrong instead of
        // plausibly tiled — the set must not wrap onto the tile it does have.
        assert_eq!(sampled(&tex, 3.5, 2.5), [0, 0, 0], "hole");
        assert_eq!(sampled(&tex, 10.5, 0.5), [0, 0, 0], "past the grid");
        assert_eq!(sampled(&tex, -0.5, 0.5), [0, 0, 0], "before the grid");
        assert_eq!(
            sampled(&tex, 0.5, 0.5),
            [10, 20, 30],
            "the tile that exists"
        );
    }

    #[test]
    fn an_empty_tile_set_declines_instead_of_loading_the_literal_name() {
        let dir = scratch("empty");
        assert!(UvTexture::open(&dir.join("gone.<UDIM>.png"), ColorSpace::Raw).is_none());
        assert!(UvTexture::open(&dir.join("gone.<UVTILE>.png"), ColorSpace::Raw).is_none());
    }

    #[test]
    fn a_single_image_wraps_instead_of_tiling() {
        let dir = scratch("single");
        write_tile(&dir.join("flat.png"), [7, 8, 9]);
        let tex = UvTexture::open(&dir.join("flat.png"), ColorSpace::Raw).expect("loads");
        // MaterialX's default address mode is `periodic`, so a coordinate
        // outside the unit square wraps back rather than reading black.
        assert_eq!(sampled(&tex, 0.5, 0.5), [7, 8, 9]);
        assert_eq!(sampled(&tex, 3.5, 2.5), [7, 8, 9]);
    }

    #[test]
    fn the_two_tokens_spell_the_same_grid() {
        for (u, v) in [(0, 0), (1, 0), (0, 1), (9, 9)] {
            let udim = TileToken::Udim.expand("a.<UDIM>.png", u, v);
            let uvtile = TileToken::UvTile.expand("a.<UVTILE>.png", u, v);
            assert_eq!(udim, format!("a.{}.png", udim_number(u, v)));
            assert_eq!(uvtile, format!("a.u{}_v{}.png", u + 1, v + 1));
        }
        assert_eq!(TileToken::detect("a.<UDIM>.png"), Some(TileToken::Udim));
        assert_eq!(TileToken::detect("a.<UVTILE>.png"), Some(TileToken::UvTile));
        assert_eq!(TileToken::detect("a.png"), None);
    }

    fn at(space: ColorSpace, encoded: u8) -> f32 {
        to_linear_table(space)[encoded as usize]
    }

    #[test]
    fn gamma_tables_are_pure_power_laws() {
        for encoded in [0u8, 3, 13, 26, 128, 255] {
            let c = encoded as f32 / 255.0;
            assert!((at(ColorSpace::Gamma22, encoded) - c.powf(2.2)).abs() < 1e-7);
            assert!((at(ColorSpace::Gamma18, encoded) - c.powf(1.8)).abs() < 1e-7);
        }
        // Both curves are anchored: black stays black, white stays white, so
        // a fully-lit albedo keeps its exposure whichever tag it carries.
        for space in [ColorSpace::Gamma22, ColorSpace::Gamma18, ColorSpace::Srgb] {
            assert_eq!(at(space, 0), 0.0);
            assert!((at(space, 255) - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn gamma_22_is_not_the_srgb_curve_in_the_shadows() {
        // The regression this pins. `g22_rec709` used to decode through the
        // piecewise sRGB curve, whose linear toe lifts near-black by an order
        // of magnitude — an albedo of 0.01 encoded reads 19x too bright.
        let (srgb, g22) = (at(ColorSpace::Srgb, 3), at(ColorSpace::Gamma22, 3));
        assert!(srgb > g22 * 10.0, "srgb {srgb} vs gamma22 {g22}");
        // And converges in the midtones, which is why the bug is invisible
        // on a look-dev turntable and only shows up in dark albedo.
        let (srgb, g22) = (at(ColorSpace::Srgb, 128), at(ColorSpace::Gamma22, 128));
        assert!((srgb - g22).abs() < 0.005, "srgb {srgb} vs gamma22 {g22}");
    }

    #[test]
    fn gamma_18_is_brighter_than_both_across_the_range() {
        // 1.8 is the shallower exponent, so it decodes above 2.2 everywhere
        // strictly inside [0,1] — the error is not confined to the toe.
        for encoded in [13u8, 64, 128, 200] {
            let (g18, g22) = (
                at(ColorSpace::Gamma18, encoded),
                at(ColorSpace::Gamma22, encoded),
            );
            assert!(g18 > g22, "at {encoded}: g18 {g18} !> g22 {g22}");
            assert!(
                g18 > at(ColorSpace::Srgb, encoded),
                "at {encoded}: g18 {g18}"
            );
        }
    }

    #[test]
    fn raw_is_the_identity() {
        for encoded in [0u8, 1, 77, 255] {
            assert_eq!(at(ColorSpace::Raw, encoded), encoded as f32 / 255.0);
        }
    }

    /// Greyscale texels as a level: value `v` in all three channels.
    fn grey(values: &[u8]) -> Vec<u8> {
        values.iter().flat_map(|&v| [v, v, v]).collect()
    }

    /// The red channel of every texel of a level.
    fn reds(level: &[u8]) -> Vec<u8> {
        level.iter().step_by(3).copied().collect()
    }

    /// Reduces one row under `Raw`, where the decode table and `encode` are
    /// the identity and a level is its own u8 values back.
    fn reduce_row(values: &[u8]) -> Vec<u8> {
        let table = to_linear_table(ColorSpace::Raw);
        let (out, w, h) = reduce_half(
            &grey(values),
            values.len(),
            1,
            &table,
            encode_fn(ColorSpace::Raw),
        );
        assert_eq!((w, h), (values.len().div_ceil(2), 1));
        reds(&out)
    }

    /// **An odd axis is resampled by area, not by duplicating its last
    /// texel.**
    ///
    /// The sampler maps `x = u * width - 0.5`, so it reads each destination
    /// texel as an equal-width slice of the whole tile. Five source texels
    /// into three therefore means each new texel averages the source over
    /// exactly its own 5/3 of the row: `[1, 2/3]`, `[1/3, 1, 1/3]`,
    /// `[2/3, 1]`, normalised. Clamping the source index instead — which is
    /// what this did — makes the last texel the average of source column 4
    /// alone, a fifth of the domain stretched over a third of it.
    ///
    /// The numbers are chosen to land exactly, and the two schemes are 5, 25
    /// and 20 units apart, so the tolerance for the round trip through the
    /// 256-entry table cannot blur them together.
    #[test]
    fn an_odd_axis_is_area_weighted_not_edge_duplicated() {
        let got = reduce_row(&[250, 200, 150, 100, 50]);
        let want = [230u8, 150, 70]; // clamped gave [225, 125, 50]
        for (i, (&g, &w)) in got.iter().zip(want.iter()).enumerate() {
            assert!(
                (g as i32 - w as i32).abs() <= 1,
                "texel {i}: got {g}, want {w} (the whole level is {got:?})"
            );
        }
    }

    /// And because the taps tile the axis, the level's mean is the source's.
    ///
    /// This is the half that shows up in a render: the clamp gave the
    /// trailing column a third of the level's weight where it is owed a
    /// fifth, so the mean drifted at *every* level and the drift compounded
    /// down the chain.
    #[test]
    fn an_odd_level_preserves_the_mean() {
        let src = [250u8, 200, 150, 100, 50];
        let got = reduce_row(&src);
        let mean = |v: &[u8]| v.iter().map(|&x| x as f32).sum::<f32>() / v.len() as f32;
        assert!(
            (mean(&got) - mean(&src)).abs() < 1.0,
            "level mean {} against source mean {} (the clamp gave 133.3)",
            mean(&got),
            mean(&src),
        );
    }

    /// **An even axis must reduce exactly as it always has**, bit for bit.
    ///
    /// Every overlap on an even axis is exactly `1.0` and the axis total
    /// exactly `2.0`, so `weighted` divides by exactly `4.0` and the result is
    /// the old `0.25 * (a + b + c + d)` unchanged. Every checked-in texture is
    /// 64x64, so this is what says the sample goldens cannot move — and with
    /// them the streamed-versus-preloaded invariant, which compares a `.tx`
    /// chain against an in-memory one.
    #[test]
    fn an_even_axis_reduces_exactly_as_it_did() {
        let (sw, sh) = (8usize, 6usize);
        let src: Vec<u8> = (0..sw * sh * 3).map(|i| (i * 7 % 251) as u8).collect();
        let table = to_linear_table(ColorSpace::Srgb);
        let encode = encode_fn(ColorSpace::Srgb);
        let (got, w, h) = reduce_half(&src, sw, sh, &table, encode);
        assert_eq!((w, h), (4, 3));
        for y in 0..h {
            for x in 0..w {
                for k in 0..3 {
                    let at = |xi: usize, yi: usize| table[src[(yi * sw + xi) * 3 + k] as usize];
                    let mean = 0.25
                        * (at(2 * x, 2 * y)
                            + at(2 * x + 1, 2 * y)
                            + at(2 * x, 2 * y + 1)
                            + at(2 * x + 1, 2 * y + 1));
                    let want = (encode(mean) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
                    assert_eq!(got[(y * w + x) * 3 + k], want, "at ({x}, {y}) channel {k}");
                }
            }
        }
    }

    /// The two reducers back the TIFF and the EXR `.tx` writer, and their doc
    /// comments promise they agree on everything but the transfer curve.
    ///
    /// Left to prose, that promise is exactly the kind a change keeps half of:
    /// an EXR-backed `.tx` and a TIFF-backed one that disagreed on odd levels
    /// would each be internally consistent, and nothing would look wrong until
    /// two renders of the same texture were diffed against each other.
    #[test]
    fn the_two_reducers_agree_on_an_odd_level() {
        let (sw, sh) = (7usize, 5usize);
        let bytes: Vec<u8> = (0..sw * sh * 3).map(|i| (i * 11 % 251) as u8).collect();
        let floats: Vec<f32> = bytes.iter().map(|&b| b as f32 / 255.0).collect();

        let (from_u8, w, h) = reduce_half(
            &bytes,
            sw,
            sh,
            &to_linear_table(ColorSpace::Raw),
            encode_fn(ColorSpace::Raw),
        );
        let (from_f32, lw, lh) = reduce_half_linear(&floats, sw, sh);
        assert_eq!((w, h), (lw, lh), "the two disagree on level size");
        for (i, (&b, &f)) in from_u8.iter().zip(from_f32.iter()).enumerate() {
            let quantised = (f * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            assert_eq!(b, quantised, "component {i}");
        }
    }

    /// Three taps is the ceiling `Tap` is sized for, and it is arithmetic
    /// rather than an observation — but the arithmetic is easy to get wrong,
    /// and a fourth would be silently dropped in release.
    #[test]
    fn no_destination_texel_reaches_more_than_three_source_texels() {
        // Dense up to 2000 — the drift this caught first was at 1795 — plus
        // sizes past where `f32` stops counting `j * src` exactly.
        let sizes = (1..2000usize).chain([4095, 4096, 8191, 8192, 16383, 16384]);
        for src in sizes {
            let dst = src.div_ceil(2);
            let (taps, sum) = axis_taps(src, dst);
            assert_eq!(taps.len(), dst);
            for (j, t) in taps.iter().enumerate() {
                assert!((1..=3).contains(&t.count), "src {src} texel {j}: {t:?}");
                assert!(t.start + t.count <= src, "src {src} texel {j} runs past");
                let total: f32 = t.weight[..t.count].iter().sum();
                assert!(
                    (total - sum).abs() < 1e-5,
                    "src {src} texel {j} totals {total}, not {sum} — the taps \
                     must tile the axis or the level's mean drifts"
                );
            }
            // An even axis is the plain 2x2, exactly: this is the property
            // `an_even_axis_reduces_exactly_as_it_did` rests on.
            if src % 2 == 0 {
                assert_eq!(sum, 2.0);
                assert!(
                    taps.iter()
                        .all(|t| t.count == 2 && t.weight[..2] == [1.0, 1.0])
                );
            }
        }
    }

    /// Writes an EXR of `w x h` texels from a per-texel colour.
    fn write_exr(
        path: &Path,
        w: usize,
        h: usize,
        f: impl Fn(usize, usize) -> (f32, f32, f32) + Sync,
    ) {
        std::fs::create_dir_all(path.parent().unwrap()).expect("temp dir");
        exr::prelude::write_rgb_file(path, w, h, f).expect("write exr");
    }

    #[test]
    fn an_exr_preloads_at_full_float_precision_and_range() {
        let dir = scratch("exr_f32");
        let p = dir.join("hdr.exr");
        // A dark value 8-bit linear would round away and a bright one it
        // would clip: both have to come back exactly.
        write_exr(&p, 2, 1, |x, _| {
            if x == 0 {
                (0.02, 0.003, 0.5)
            } else {
                (4.0, 1.5, 0.25)
            }
        });
        let tex = UvTexture::open_with(&p, ColorSpace::Auto, false).expect("loads");
        assert!(tex.is_float());
        assert_eq!(
            tex.color_space(),
            ColorSpace::Raw,
            "auto on a float file is raw"
        );
        assert_eq!(tex.bytes(), 2 * 3 * 4);
        // Texel centres: x = u * 2 - 0.5 lands exactly on 0 and 1.
        assert_eq!(tex.eval(0.25, 0.5, 0.0), [0.02, 0.003, 0.5, 1.0]);
        assert_eq!(tex.eval(0.75, 0.5, 0.0), [4.0, 1.5, 0.25, 1.0]);
    }

    #[test]
    fn an_explicit_curve_on_an_exr_is_applied_once_at_load() {
        let dir = scratch("exr_srgb");
        let p = dir.join("enc.exr");
        write_exr(&p, 1, 1, |_, _| (0.5, 0.5, 0.5));
        let tex = UvTexture::open_with(&p, ColorSpace::Srgb, false).expect("loads");
        let want = crate::srgb_to_linear(0.5);
        assert!((tex.eval(0.5, 0.5, 0.0)[0] - want).abs() < 1e-6);
    }

    #[test]
    fn an_exr_udim_set_addresses_by_tile() {
        let dir = scratch("exr_udim");
        write_exr(&dir.join("a.1001.exr"), 1, 1, |_, _| (1.0, 0.0, 0.0));
        write_exr(&dir.join("a.1002.exr"), 1, 1, |_, _| (0.0, 2.0, 0.0));
        let tex = UvTexture::open(&dir.join("a.<UDIM>.exr"), ColorSpace::Raw).expect("loads");
        assert_eq!(tex.tile_count(), 2);
        assert_eq!(tex.eval(0.5, 0.5, 0.0)[..3], [1.0, 0.0, 0.0]);
        assert_eq!(tex.eval(1.5, 0.5, 0.0)[..3], [0.0, 2.0, 0.0]);
        assert_eq!(
            tex.eval(2.5, 0.5, 0.0)[..3],
            [0.0, 0.0, 0.0],
            "no tile 1003"
        );
    }

    #[test]
    fn an_exr_pyramid_preserves_the_mean_on_an_odd_axis() {
        let dir = scratch("exr_mip");
        let p = dir.join("row.exr");
        let src = [2.5f32, 2.0, 1.5, 1.0, 0.5];
        write_exr(&p, 5, 1, |x, _| (src[x], src[x], src[x]));
        let tex = UvTexture::open_with(&p, ColorSpace::Raw, true).expect("loads");
        let Storage::F32(tiles) = &tex.storage else {
            panic!("an EXR is f32");
        };
        let l1 = &tiles[0].levels[1];
        assert_eq!(l1.width, 3);
        let mean = l1.pixels.iter().step_by(3).sum::<f32>() / 3.0;
        assert!(
            (mean - 1.5).abs() < 1e-5,
            "level-1 mean {mean}, source mean 1.5"
        );
        // The chain reaches one texel, and that texel is the source mean.
        let top = tiles[0].levels.last().unwrap();
        assert_eq!((top.width, top.height), (1, 1));
        assert!((top.pixels[0] - 1.5).abs() < 1e-5);
    }

    #[test]
    fn auto_decodes_an_rgb_png_and_leaves_a_grey_one_raw() {
        let dir = scratch("auto_png");
        let rgb = dir.join("rgb.png");
        write_tile(&rgb, [128, 128, 128]);
        let grey = dir.join("grey.png");
        image::GrayImage::from_pixel(1, 1, image::Luma([128]))
            .save(&grey)
            .expect("write png");

        let t = UvTexture::open(&rgb, ColorSpace::Auto).expect("loads");
        assert_eq!(t.color_space(), ColorSpace::Srgb);
        assert!((t.eval(0.5, 0.5, 0.0)[0] - crate::srgb_to_linear(128.0 / 255.0)).abs() < 1e-6);

        let t = UvTexture::open(&grey, ColorSpace::Auto).expect("loads");
        assert_eq!(t.color_space(), ColorSpace::Raw);
        assert_eq!(t.eval(0.5, 0.5, 0.0)[0], 128.0 / 255.0);
    }
}
