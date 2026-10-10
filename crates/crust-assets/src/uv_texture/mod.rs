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
//! what a per-texel match costs (`openspec/specs/textures/design.md`, "the
//! second backing must cost the first one nothing").
//!
//! **Alpha, when the file has one that cuts something.** A tile whose image
//! carries an authored alpha that is not opaque everywhere is held as RGBA,
//! four samples a texel; every other tile stays RGB, so a texture with no
//! alpha holds and reads exactly what it did before alpha was carried, and
//! reads 1.0 there as `UsdUVTexture` specifies. Alpha is coverage: never
//! colour-managed, `a / 255` from a byte. Whether to read a fourth sample is
//! decided once per lookup, in the same `match` that picks the storage, and
//! then per tile only for a texture that has any alpha at all.

use std::num::NonZeroUsize;
use std::path::Path;

use crust_core::{ColorSpace, ResolvedColorSpace, Texture2D};

use crate::TransferCurve;
use crust_core::warning;

use crate::error::AssetError;

mod decode;
mod mip;
mod tile;
mod udim;

use decode::{decode_exr_tile, decode_tile};
use tile::{Texel, Tile};
use udim::TileToken;
pub(crate) use udim::udim_number;

use crate::mip_filter::{MipSource, Taps, lerp_rgba, lerp_rgba_alpha, trilinear};

pub(crate) use mip::{channels, reduce_half, reduce_half_linear};
pub(crate) use udim::existing_tiles;

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
    /// The colour space the texels were decoded under, resolved against the
    /// file.
    space: ResolvedColorSpace,
    /// The change of primaries into the working space, applied once per
    /// lookup after filtering. Bytes keep the file's own primaries — the
    /// decode table is the curve alone — so a `u8` texture carries the matrix;
    /// an `f32` one was converted whole at load and carries `None`, except a
    /// marked `.tx` EXR, whose values are already linear on the source's
    /// primaries and which carries the matrix as its streamed twin does.
    gamut: Option<crust_core::Mat3A>,
    /// Representative tile size, for the load message.
    width: usize,
    height: usize,
    /// True when the file name carried a tile token (`<UDIM>` or `<UVTILE>`).
    /// A tile set addresses tiles by the integer part of `(u, v)`; a single
    /// image wraps instead, which is MaterialX's default `periodic` address
    /// mode.
    tiled: bool,
    /// Whether any tile holds alpha ([`Tile::alpha`]). Settles which sampler a
    /// lookup runs, with the storage: one with no alpha never asks a tile.
    alpha: bool,
}

/// Largest tile edge kept, unless `CRUST_TEX_MAX` says otherwise.
///
/// 1024 rather than full resolution for the reason in the module note above.
/// It is a *cap*, not a resize: a smaller tile is kept as authored.
pub const DEFAULT_MAX_EDGE: usize = crust_core::config::DEFAULT_TEX_MAX;

/// The edge cap actually in effect, `CRUST_TEX_MAX` included.
///
/// Public and named for the same reason [`max_log2_from_env`] is on the Ptex
/// side: the cap is what answers "why is this texture only 1024 wide", so the
/// probes and the residency log read it from here rather than each re-parsing
/// the variable.
///
/// [`max_log2_from_env`]: crate::max_log2_from_env
pub fn max_edge_from_env() -> NonZeroUsize {
    crust_core::config().tex_max
}

/// Are mip pyramids built? `CRUST_TEX_MIP=0` keeps each tile at its single
/// capped level, which is the pre-pyramid behaviour bit for bit — a one-level
/// tile has nothing to select between — and costs a third less memory. The
/// A/B for "did the pyramid change this, or did the footprint?", paired with
/// `CRUST_RAY_CONES=0` on the other side.
fn mip_enabled() -> bool {
    crust_core::config().tex_mip
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
        UvTexture::open_capped(path, space, mip, max_edge_from_env())
    }

    /// [`UvTexture::open_with`] with the `CRUST_TEX_MAX` edge cap passed in
    /// too. A failure is logged as a warning; [`UvTexture::try_open_capped`]
    /// returns it.
    pub fn open_capped(
        path: &Path,
        space: ColorSpace,
        mip: bool,
        max_edge: NonZeroUsize,
    ) -> Option<UvTexture> {
        UvTexture::try_open_capped(path, space, mip, max_edge)
            .map_err(|e| tracing::warn!("{e}"))
            .ok()
    }

    /// [`UvTexture::open_capped`], with the reason it failed: what
    /// [`FileAssets`](crate::FileAssets) calls with its own
    /// [`Config`](crust_core::Config), and logs once.
    ///
    /// A tile of a UDIM set that exists but does not decode is skipped with a
    /// warning of its own — the set still renders on the tiles it has — and a
    /// set with no tile at all is the error.
    pub fn try_open_capped(
        path: &Path,
        space: ColorSpace,
        mip: bool,
        max_edge: NonZeroUsize,
    ) -> Result<UvTexture, AssetError> {
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
            Ok::<_, AssetError>(tile)
        };
        if let Some(token) = token {
            for tile in existing_tiles(&name) {
                match decode(&tile.path, tile.number) {
                    Ok(t) => tiles.push(t),
                    Err(e) => warning!(TextureUdimTileMissing, "{e} — skipping that UDIM tile"),
                }
            }
            if tiles.is_empty() {
                return Err(AssetError::unusable(
                    path,
                    format!(
                        "no tiles found ({} expanded over the 10x10 grid)",
                        token.as_str()
                    ),
                ));
            }
        } else {
            tiles.push(decode(path, udim_number(0, 0))?);
        }
        // Some whenever a tile decoded, and one always has by here.
        let space = resolved.unwrap_or_else(|| space.resolve_auto(false, 0));

        let to_linear = space.to_linear_table();
        if mip {
            let steps = space.code_steps();
            for t in &mut tiles {
                t.build_pyramid(&to_linear, &steps);
            }
        }
        let (width, height) = (tiles[0].levels[0].width, tiles[0].levels[0].height);
        let alpha = tiles.iter().any(|t| t.alpha);
        Ok(UvTexture {
            storage: Storage::U8(tiles),
            to_linear,
            gamut: space.gamut(),
            space,
            width,
            height,
            tiled: token.is_some(),
            alpha,
        })
    }

    /// The EXR half of [`UvTexture::open_with`]: the same sweep, into `f32`
    /// tiles.
    ///
    /// `Auto` resolves as the streaming path resolves it, so a file binds the
    /// same whether it is streamed or preloaded: a `crust:mipspace` marker (a
    /// `.tx` EXR crust converted) names the source's space, and its values are
    /// already linear on that space's primaries, so only the change of
    /// primaries remains — applied per lookup, after filtering, as streaming
    /// applies it. Unmarked, `auto` is raw: an EXR is float, and the
    /// UsdUVTexture rule marks only 8-bit images as sRGB. An *explicit* curve
    /// is still honoured (a float file of display-encoded values is unusual,
    /// not invalid), and applied here in `f32`, so a stored tile is linear
    /// whatever was asked for and the lookup needs no table.
    fn open_exr(
        path: &Path,
        token: Option<TileToken>,
        space: ColorSpace,
        mip: bool,
        max_edge: NonZeroUsize,
    ) -> Result<UvTexture, AssetError> {
        let found = existing_tiles(&path.to_string_lossy());
        // Settled by the first file, as `StreamingTexture::open` settles it.
        let first = match token {
            Some(_) => found.first().map(|t| t.path.clone()),
            None => Some(path.to_path_buf()),
        };
        let marked = match space.resolved() {
            Some(_) => None,
            None => first
                .as_deref()
                .and_then(crate::tiled::exr_mip_space)
                .and_then(|m| crust_core::color::Space::named(&m))
                .map(|s| ResolvedColorSpace::new(s, space.working())),
        };
        let (space, decode, gamut) = match marked {
            Some(marked) => (marked, ResolvedColorSpace::RAW, marked.gamut()),
            None => {
                let space = space.resolve_auto(false, 3);
                (space, space, None)
            }
        };
        let mut tiles = Vec::new();
        if let Some(token) = token {
            for tile in &found {
                match decode_exr_tile(&tile.path, tile.number, max_edge, decode) {
                    Ok(t) => tiles.push(t),
                    Err(e) => warning!(TextureUdimTileMissing, "{e} — skipping that UDIM tile"),
                }
            }
            if tiles.is_empty() {
                return Err(AssetError::unusable(
                    path,
                    format!(
                        "no tiles found ({} expanded over the 10x10 grid)",
                        token.as_str()
                    ),
                ));
            }
        } else {
            tiles.push(decode_exr_tile(path, udim_number(0, 0), max_edge, decode)?);
        }
        if mip {
            for t in &mut tiles {
                t.build_pyramid_linear();
            }
        }
        let (width, height) = (tiles[0].levels[0].width, tiles[0].levels[0].height);
        let alpha = tiles.iter().any(|t| t.alpha);
        Ok(UvTexture {
            storage: Storage::F32(tiles),
            to_linear: ResolvedColorSpace::RAW.to_linear_table(),
            gamut,
            space,
            width,
            height,
            tiled: token.is_some(),
            alpha,
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

    /// The colour space the texels were decoded under.
    pub fn color_space(&self) -> ResolvedColorSpace {
        self.space
    }

    /// Whether any tile holds an alpha channel, which a lookup returns as its
    /// fourth component. `false` reads 1.0 there.
    pub fn has_alpha(&self) -> bool {
        self.alpha
    }

    /// Size of the first tile, representative of the set.
    pub fn tile_size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Trilinear lookup inside one tile, at coordinates already reduced to
    /// `[0, 1)` and over a footprint `width` wide *in tile units* — the
    /// shared [`trilinear`] over this tile's pyramid.
    ///
    /// `ALPHA` is the tile's own [`Tile::alpha`]: whether its texels are four
    /// samples, the fourth read as the lookup's alpha.
    #[inline(always)]
    fn sample_tile<T: Texel, const ALPHA: bool>(
        &self,
        t: &Tile<T>,
        u: f32,
        v: f32,
        width: f32,
    ) -> [f32; 4] {
        let source = TileSource::<T, ALPHA> { tex: self, tile: t };
        trilinear(&source, u, v, width).expect("a preloaded tile always answers")
    }
}

/// One preloaded tile as a [`MipSource`], RGBA when `ALPHA`.
struct TileSource<'a, T, const ALPHA: bool> {
    tex: &'a UvTexture,
    tile: &'a Tile<T>,
}

impl<T: Texel, const ALPHA: bool> MipSource for TileSource<'_, T, ALPHA> {
    type Texel = [f32; 4];

    #[inline(always)]
    fn level_count(&self) -> usize {
        self.tile.levels.len()
    }

    #[inline(always)]
    fn texels_across(&self) -> f32 {
        let l0 = &self.tile.levels[0];
        l0.width.max(l0.height) as f32
    }

    /// Bilinear lookup inside one mip level, at coordinates already reduced
    /// to `[0, 1)`.
    ///
    /// Bilinear rather than nearest because these charts are magnified: a 4K
    /// tile capped to 1024 covers a few hundred pixels of the framing, so the
    /// texel grid is plainly visible under point sampling — the artefact the
    /// dome light's own nearest-texel sampling is still criticised for in
    /// `openspec/specs/lighting/design.md`.
    #[inline(always)]
    fn bilinear(&self, level: usize, u: f32, v: f32) -> Option<[f32; 4]> {
        let t = &self.tile.levels[level];
        // Image rows run top-down while `v` grows upward, the same flip the
        // rest of the graphics world applies between UV and raster space.
        let taps = Taps::new(
            u * t.width as f32 - 0.5,
            (1.0 - v) * t.height as f32 - 0.5,
            t.width,
            t.height,
        );
        if ALPHA {
            let texel = |xi: usize, yi: usize| {
                let o = (yi * t.width + xi) * 4;
                let [r, g, b] = T::linear(&t.pixels, o, &self.tex.to_linear);
                [r, g, b, T::alpha(&t.pixels, o + 3)]
            };
            return Some(taps.blend_rgba(
                texel(taps.x0, taps.y0),
                texel(taps.x1, taps.y0),
                texel(taps.x0, taps.y1),
                texel(taps.x1, taps.y1),
            ));
        }
        let texel = |xi: usize, yi: usize| {
            let o = (yi * t.width + xi) * 3;
            T::linear(&t.pixels, o, &self.tex.to_linear)
        };
        Some(taps.blend_rgb(
            texel(taps.x0, taps.y0),
            texel(taps.x1, taps.y0),
            texel(taps.x0, taps.y1),
            texel(taps.x1, taps.y1),
        ))
    }

    #[inline(always)]
    fn blend(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
        if ALPHA {
            lerp_rgba_alpha(a, b, t)
        } else {
            lerp_rgba(a, b, t)
        }
    }
}

impl UvTexture {
    /// [`Texture2D::eval`] over one storage's tiles, monomorphised per
    /// sample type so the storage is matched once per lookup, not per texel.
    ///
    /// `ANY_ALPHA` is [`UvTexture::alpha`]. Without it every tile is RGB and
    /// none is asked; with it the tile found says which sampler reads it.
    #[inline(always)]
    fn eval_tiles<T: Texel, const ANY_ALPHA: bool>(
        &self,
        tiles: &[Tile<T>],
        u: f32,
        v: f32,
        width: f32,
    ) -> [f32; 4] {
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
                Some(t) => self.sample_one::<T, ANY_ALPHA>(t, u - tu, v - tv, width),
                None => [0.0, 0.0, 0.0, 1.0],
            }
        } else {
            // MaterialX's default address mode is `periodic`.
            let wrap = |x: f32| x - x.floor();
            self.sample_one::<T, ANY_ALPHA>(&tiles[0], wrap(u), wrap(v), width)
        }
    }

    /// [`UvTexture::sample_tile`] with the tile's alpha settled: compiled out
    /// for a texture with none.
    #[inline(always)]
    fn sample_one<T: Texel, const ANY_ALPHA: bool>(
        &self,
        t: &Tile<T>,
        u: f32,
        v: f32,
        width: f32,
    ) -> [f32; 4] {
        if ANY_ALPHA && t.alpha {
            self.sample_tile::<T, true>(t, u, v, width)
        } else {
            self.sample_tile::<T, false>(t, u, v, width)
        }
    }
}

impl Texture2D for UvTexture {
    fn eval(&self, u: f32, v: f32, width: f32) -> [f32; 4] {
        let _p = crust_core::profile::scope(crust_core::profile::Section::Texture);
        if !u.is_finite() || !v.is_finite() {
            return [0.0, 0.0, 0.0, 1.0];
        }
        // A non-finite width is the caller's bug; point-sample rather than
        // propagate a NaN into a level index.
        let width = if width.is_finite() { width } else { 0.0 };
        let rgba = match (&self.storage, self.alpha) {
            (Storage::U8(tiles), false) => self.eval_tiles::<u8, false>(tiles, u, v, width),
            (Storage::U8(tiles), true) => self.eval_tiles::<u8, true>(tiles, u, v, width),
            (Storage::F32(tiles), false) => self.eval_tiles::<f32, false>(tiles, u, v, width),
            (Storage::F32(tiles), true) => self.eval_tiles::<f32, true>(tiles, u, v, width),
        };
        crate::to_working(self.gamut.as_ref(), rgba)
    }
}

#[cfg(test)]
mod tests;
