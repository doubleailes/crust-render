# Reading OIIO `.tx` textures — an evaluation

*Written 2026-07 against OpenImageIO 3.1.15, the `tiff` crate 0.11.3, the `exr` crate
1.74.2 (already a workspace dependency), and the current state of this repository.
Every claim in §1, §3 and §4 was measured in this container against real `.tx` files
produced by OIIO's own `make_texture` (the library entry point behind `maketx`).*

## TL;DR

1. **A `.tx` is not a new format.** By default it is a **tiled, MIP-mapped TIFF**:
   64×64 tiles, one TIFF sub-IFD per MIP level, `zip` (deflate) compressed, texture
   metadata in the Pixar private TIFF tags. OIIO simply associates the `.tx`, `.env`,
   `.sm`, `.vsm` extensions with its TIFF plugin. The container is chosen from the
   **output extension alone** — never from bit depth — so a 32-bit float `.tx` is still
   a TIFF (§1.4). But a tiled MIP-mapped **OpenEXR** is just as common in float
   pipelines, normally named `.exr`, and `--format exr` can put one behind a `.tx`
   name. Both containers need support, and dispatch must be on magic bytes (§1.5).
2. **You do not need OpenImageIO to read one.** The pure-Rust `tiff` crate already has
   exactly the API this needs: `seek_to_image(level)` selects a MIP level (each level is
   one IFD) and `read_chunk(tile_index)` decodes **one tile**, seeking straight to
   `TileOffsets[i]`. Verified bit-exact against OIIO's own `read_tiles` output.
3. **The RAM win is real and large.** On a 4096² RGB float `.tx` (164 MiB on disk),
   decoding the whole level 0 peaks at **194.7 MiB RSS**; sweeping all 4096 tiles
   one-at-a-time peaks at **2.8 MiB** — a ~70× reduction, with per-tile I/O of
   32–64 KiB (0.02–0.04 % of the file) that is O(tile), not O(image).
4. **Recommendation: pure Rust**, a new `crust-tex` crate (an OIIO-`ImageCache`-shaped
   tile cache + a small `TextureSystem` on top), `tiff` for the TIFF flavour and the
   already-present `exr` for the OpenEXR flavour — **both from the start**, since a
   float/half pipeline will hand you tiled MIP-mapped EXRs at least as often as TIFF
   `.tx` files. Do *not* take an FFI dependency on OIIO — it would cost this project
   the property that makes it what it is (§2).
5. **But reading tiles is the small part of the job.** This renderer currently has
   **no UVs anywhere** (`HitRecord` has no `uv`; `crust-rt` reports barycentrics only),
   no texturable material inputs, no `UsdUVTexture` import, and no ray differentials —
   and without a footprint estimate you cannot know *which* tile is "the right tile".
   §5 lays out that chain honestly; it is roughly 80 % of the work.

---

## 1. What a `.tx` file actually is

### 1.1 It's a TIFF (usually)

From OIIO's own documentation (`src/doc/builtinplugins.rst`):

> TIFF files commonly use the file extensions `.tif` or `.tiff`. Additionally,
> OpenImageIO associates the following extensions with TIFF files by default: `.tx`,
> `.env`, `.sm`, `.vsm`.

> It supports a wide variety of data formats (though unfortunately not `half`), an
> arbitrary number of channels, tiles and multiple subimages (which makes it our
> preferred texture format) […] **MIPmapping (using multi-subimage;** that means you
> can't use multiimage and MIPmaps simultaneously).

And from `src/doc/maketx.rst`:

> If `--format` is not used, `maketx` will guess based on the file extension of the
> output filename; if it is not a recognized format extension, **TIFF will be used by
> default**.

> If not specified, `maketx` will make **64 x 64 tiles**.

So the mental model is: **MIP level *n* ⇔ TIFF IFD *n*; tile ⇔ TIFF tile**. Nothing
exotic. This is exactly the structure the `tiff` crate's `seek_to_image` /
`read_chunk` pair addresses.

### 1.2 Measured structure of a real `.tx`

Generated with `oiio.ImageBufAlgo.make_texture(oiio.MakeTxTexture, …)` from a 512²
RGB float source, then read back with OIIO and independently with the `tiff` crate:

```
checker_u8.tx  (512x512 uint8 source, defaults)
  mip 0: 512x512  RGB(8)  chunk=Tile  tile=64x64  tiles=64   compression=zip
  mip 1: 256x256  RGB(8)  chunk=Tile  tile=64x64  tiles=16   compression=zip
  mip 2: 128x128  ...                             tiles=4
  mip 3:  64x64                                   tiles=1
  mip 4:  32x32   ...  (tile stays 64x64; the level is smaller than one tile)
  ...
  mip 9:   1x1
  -> 10 MIP levels; walking the whole IFD chain costs 2708 bytes of I/O
```

Notes that matter for an implementation:

- The chain runs all the way down to **1×1**. Levels below the tile size still report
  `tile=64x64` in the header; the *valid* data extent is what
  `Decoder::chunk_data_dimensions(i)` returns (e.g. `32x32` at level 4). Reading the
  reported tile size and trusting it would walk off the data.
- `tile_count()` is 1 for every level at or below the tile size.
- Default compression is `zip` (TIFF `Compression = 8`, deflate), planarconfig
  `contig`, and the data format is whatever you asked for — `uint8`, `uint16`, `float`.
  **`half` is not available in TIFF** by default (OIIO: "unfortunately not `half`"; it
  has a non-standard `tiff:half` opt-in that most tools cannot read).

### 1.3 Texture metadata lives in the Pixar private tags

TIFF has no arbitrary-metadata mechanism, so OIIO puts the texture parameters in the
Pixar private tag block (`src/tiff.imageio/tiffoutput.cpp`, tag numbers from libtiff's
`tiff.h`). Read directly with `Decoder::find_tag(Tag::Unknown(n))` — verified working:

| Tag | Name | Value in a default `maketx` output |
|---|---|---|
| 33300/33301 | `PIXAR_IMAGEFULLWIDTH` / `FULLLENGTH` | full (level 0) resolution |
| 33302 | `PIXAR_TEXTUREFORMAT` | `"Plain Texture"` (or `"Shadow"`, `"LatLong Environment"`, `"CubeFace Environment"`) |
| 33303 | `PIXAR_WRAPMODES` | `"black,black"` — the s,t wrap modes |
| 33304 | `PIXAR_FOVCOT` | `1.0` — cot(fov), for environment maps |
| 33305/33306 | `PIXAR_MATRIX_WORLDTOSCREEN` / `WORLDTOCAMERA` | shadow-map matrices |

A few OIIO-specific hints are instead smuggled into the standard `ImageDescription`
tag as `key=value` text and excised on read (`tiffinput.cpp`): `oiio:ConstantColor=`,
`oiio:AverageColor=`, `oiio:SHA-1=`, `oiio:handed=`. `oiio:SHA-1` is worth keeping —
it is the natural cache key for deduplicating identical textures.

`wrapmodes` matters: honour it (`black` | `clamp` | `periodic` | `mirror`) or textures
will differ from every other renderer's interpretation of the same file.

### 1.4 The container is chosen by extension only — *never* by bit depth

It is natural to assume that high-bit-depth textures switch to OpenEXR, since EXR is
the VFX float format and TIFF cannot carry `half`. **They do not.** The decision is
made in one place, `maketexture.cpp`:

```cpp
std::string outformat
    = configspec.get_string_attribute("maketx:fileformatname", outputfilename);
auto out = ImageOutput::create(outformat.c_str());
if (!out) { /* error */ }
if (!out->supports("tiles")) { /* error */ }
```

The only inputs are `maketx:fileformatname` (i.e. `maketx --format`) and the **output
filename**. The source's data type, bit depth, and channel count play no part.
`ImageOutput::create` resolves `.tx` to the TIFF plugin because OIIO associates that
extension with TIFF (§1.1). Measured, by writing `.tx` files from various sources and
checking the first four bytes:

| Source / request | Magic | Container | Data type in the file |
|---|---|---|---|
| `float` source, defaults | `49 49 2a 00` | TIFF | `float` |
| `half` source, defaults | `49 49 2a 00` | TIFF | **`float`** |
| `half` explicitly requested | `49 49 2a 00` | TIFF | **`float`** |
| `half` EXR read from disk | `49 49 2a 00` | TIFF | **`float`** |
| `half` + `tiff:half=1` | `49 49 2a 00` | TIFF | `half` |
| `maketx:fileformatname=openexr` | `76 2f 31 01` | **OpenEXR** | as requested |

So **a 32-bit float `.tx` is a TIFF**, and TIFF handles it natively
(`SampleFormat = IEEEFP`, `BitsPerSample = 32`) — the `tiff` crate decodes it as
`DecodingResult::F32`, which is what every measurement in §3 was taken on.

Two consequences worth knowing:

- **`half` is silently promoted to `float`.** `tiffoutput.cpp` comments
  *"Silently change requests for unsupported 'half' to 'float'"* and sets
  `m_bitspersample = 32` unless `tiff:half` is nonzero (the default is off because,
  per the code comment, "Nuke 9.0, and probably many other apps we care about, cannot
  read 16 bit float TIFFs correctly"). Ask for a half `.tx` and you get a float one at
  **twice the data size**, with no warning. This is a good reason for a pipeline to
  prefer `.exr` for float-ish textures — but that is a *choice about the filename*, not
  something `.tx` does on its own.
- **With `tiff:half=1` a genuine `half` TIFF `.tx` does exist** (verified: magic
  `II*\0`, format `half`, 64×64 tiles, 9 levels), and the `tiff` crate reads it
  correctly — but **widened to `DecodingResult::F32`**, not `F16`
  (`SampleFormat = [3,3,3]`, `BitsPerSample = [16,16,16]`, 12 288 samples for a
  64×64×3 tile, values and tile sum identical to OIIO's `read_tiles`). Note also that
  `colortype()` reports `RGB(16)` for this file: **the bit depth in `ColorType` does
  not tell you the buffer variant.** Match on the `DecodingResult` variant, and be
  ready for `F16` as well since the crate does have that variant for other inputs.

### 1.5 …but the EXR flavour is not exotic, and that changes the plan

The corollary of the above is that a *tiled, MIP-mapped OpenEXR* is the normal way to
ship float and half textures — it is just usually named `.exr` rather than `.tx`.
Verified: requesting `half` with an `.exr` output name yields OpenEXR, 64×64 tiles,
9 MIP levels, `half` preserved. OIIO's `TextureSystem` consumes that file exactly as
it consumes a `.tx`; the extension carries no meaning beyond plugin selection.

For this renderer — which is float-throughout and already writes EXR — **the EXR path
should be treated as a first-class input, not a later addition.** See §7.

**Dispatch on the magic bytes, never the extension**, in both directions: a `.tx` may
be an OpenEXR, and the tiled MIP-mapped EXR you actually want to read may be named
`.exr`. Four bytes is enough: `II*\0` / `MM\0*` → classic TIFF, `II+\0` / `MM\0+` →
BigTIFF, `76 2f 31 01` → OpenEXR.

---

## 2. The options, and why pure Rust wins

### Option A — FFI to OpenImageIO

Bindings exist in some form: [`anderslanglands/oiio-rs`](https://github.com/anderslanglands/oiio-rs),
[`phyrondev/openimageio`](https://github.com/phyrondev/openimageio), and the
[`openimageio`](https://crates.io/crates/openimageio) name on crates.io is a
placeholder reserved for the project. You would get OIIO's `ImageCache` **and**
`TextureSystem` — i.e. the tile cache, the LOD selection, anisotropic filtering, wrap
modes, UDIM, and thirty years of production hardening — essentially for free.

Why not, for *this* project:

- It contradicts the repository's defining constraint. `CLAUDE.md` opens with "a toy,
  physically-based path tracer written in **safe Rust**", scenes load "via the
  **pure-Rust** `openusd` crate", and `openqmc-rs` exists specifically as a
  *from-scratch Rust port* of a C++ library rather than a binding to it. A C++ OIIO
  dependency (which drags in libtiff, OpenEXR, Imath, boost-ish build machinery) is a
  different kind of project.
- CI is currently `cargo build && cargo test`. OIIO means a system dependency, a
  build-time C++ toolchain, and platform-specific packaging.
- The bindings are third-party and of varying maturity; none is an ASWF-blessed API.

It remains the right answer if the goal ever becomes "production texture pipeline with
UDIM, texture atlases, and OIIO-identical filtering". It is the wrong answer for
"read the right tile out of a `.tx` and keep RAM flat".

### Option B — pure Rust (recommended)

- **TIFF flavour**: the `tiff` crate (0.11.3, image-rs). Has tile-level random access,
  multi-IFD navigation, deflate + LZW + PackBits + ZSTD (feature) decompression, all
  the sample formats a `.tx` can carry (`u8`/`u16`/`i8`/`i16`/`u32`/`f16`/`f32`/`f64`),
  BigTIFF, and private-tag access. Pure Rust, no `unsafe` needed at our call sites.
- **OpenEXR flavour**: the `exr` crate — **already a workspace dependency** (used by
  `crust-render` to write the output EXR). It reads MIP levels and, via
  `block::read(…).filter_chunks(…)`, individual tiles (§4.6).

This is the recommendation. §3 is the evidence that it actually works.

### Option C — convert `.tx` to a bespoke format at load time

Rejected. It throws away the entire point of `.tx` (that the mip pyramid and tiling
are *already* prepared, and that artists' pipelines already produce them), and a
conversion pass would have to read the whole texture — the exact thing we are trying
to avoid.

---

## 3. The pure-Rust path, measured

### 3.1 The API mapping

```rust
use tiff::decoder::Decoder;

let mut d = Decoder::new(BufReader::new(File::open(path)?))?;

d.seek_to_image(level)?;              // MIP level  <-> TIFF IFD
let (w, h)   = d.dimensions()?;       // this level's resolution
let (tw, th) = d.chunk_dimensions();  // 64x64
assert_eq!(d.get_chunk_type(), ChunkType::Tile);

// tile containing texel (x, y) at this level:
let index = (y / th) * w.div_ceil(tw) + (x / tw);
let (vw, vh) = d.chunk_data_dimensions(index);  // valid extent (edge tiles are partial)
let tile = d.read_chunk(index)?;                // <- seeks to TileOffsets[index]
```

Internally `read_chunk` → `read_chunk_to_bytes` does
`self.goto_offset_u64(offset)?` straight to `chunk_offsets[chunk_index]` and expands
only that chunk. There is no whole-image path involved.

### 3.2 Correctness: bit-exact against OIIO

Tiles were read with the `tiff` crate and compared against
`ImageInput.read_tiles(...)` for the same level and tile, across `uint8`/`uint16`/
`float` and `zip`/`lzw` files. Corner texels and the whole-tile sum match exactly:

| File | Level | Tile | OIIO tile sum | `tiff` crate tile sum |
|---|---|---|---|---|
| `checker_u8.tx` | 0 | (128,192) | 5115.9844970703125 | 5115.9844 |
| `checker_u8.tx` | 1 | (64,0) | 4087.9688518941402 | 4087.9687 |
| `checker_u8.tx` | 3 | (0,0) | 6144.0003127753735 | 6144.0001 |
| `checker_f32.tx` | 0 | (128,192) | 5117.99609375 | 5117.9961 |
| `checker_f32.tx` | 1 | (64,0) | 4091.991215765476 | 4091.9912 |
| `checker_f32.tx` | 3 | (0,0) | 6144.000654742122 | 6144.0007 |
| `checker_u16_lzw.tx` | 0 | (128,192) | 5117.9949951171875 | 5117.9950 |
| `checker_u16_lzw.tx` | 1 | (64,0) | 4091.9957917779684 | 4091.9958 |
| `checker_u16_lzw.tx` | 3 | (0,0) | 6143.999019026756 | 6143.9990 |

(Rust sums printed to 4 decimals; individual corner texels compared exactly too. The
LZW file also exercises §4.3 — its level 0 is LZW while levels 1+ are zip.)

### 3.3 The RAM result — the actual question

4096×4096 RGB float `.tx`, 163.9 MiB on disk, 192 MiB of level-0 texels:

| Strategy | Peak RSS | File bytes read |
|---|---|---|
| `read_image()` — decode all of level 0 | **194.7 MiB** | 135.2 MiB |
| Sweep all 4096 tiles one at a time | **2.8 MiB** | 135.1 MiB |

Same total I/O when you eventually touch everything, but the resident set is ~70×
smaller because only one tile is alive at a time. And when you *don't* touch
everything — the normal case, since a texture is only sampled where it is visible and
at the MIP level the footprint calls for — the I/O drops proportionally:

| Access | File bytes read | % of file |
|---|---|---|
| Cold open (parse IFD 0, incl. 4096-entry offset table) | 33 022 | 0.019 % |
| `seek_to_image(2)` (walk IFD chain) | 2 618 | 0.002 % |
| One 64×64×3 float tile at level 2 | 32 768 – 65 536 | **0.02 – 0.04 %** |

(The per-tile figure is 1.33× the 48 KiB raw tile because compressed tiles average
31 KiB and the `BufReader` rounds reads to its 64 KiB capacity. It does not grow with
image size — that is the whole point.)

### 3.4 Cost per tile, and where it goes

Single-threaded, warm page cache, 4-core container, `--release`, 400 tiles:

| Operation | Per tile | Rate |
|---|---|---|
| `seek_to_image` + `read_chunk` (naive loop) | 539 µs | 1 856 /s |
| `read_chunk` only, level pinned | **341 µs** | 2 932 /s |
| `seek_to_image(0)` alone | 143 µs | — |
| `read_chunk_bytes` into a reused buffer | 332 µs | 3 016 /s |
| Alternating between 3 MIP levels per lookup | 621 µs | 1 609 /s |

Two lessons: **never `seek_to_image` per lookup** (it re-parses the IFD and its 32 KiB
offset table — 40 % overhead), and **the cost is inflate, not allocation** (reusing the
output buffer saves 3 %).

Compression choice dominates decode cost far more than anything in our control:

| `.tx` variant | File size | Per tile | Rate |
|---|---|---|---|
| float / `zip` (the `maketx` default) | 163.9 MiB | 326 µs | 3 067 /s |
| float / `none` | 233.5 MiB | **46 µs** | 21 751 /s |
| uint8 / `zip` | 35.2 MiB | 88 µs | 11 414 /s |

**Uncompressed `.tx` decodes 7× faster for 42 % more disk.** If texture I/O ever shows
up in a profile, `maketx --compression none` is the cheapest available win — worth
documenting for users rather than engineering around.

### 3.5 Parallel access works

One `Decoder` per thread over the same file, 200 scattered tiles each, with a known
tile's checksum verified identical on every thread:

| Threads | Tiles/s total | Peak RSS |
|---|---|---|
| 1 | 1 064 | 3.3 MiB |
| 4 | 4 252 | 4.5 MiB |
| 8 (on 4 cores) | 6 114 | 6.2 MiB |

Scales linearly to core count; memory stays flat. `Decoder::new` costs ~1 ms (it
parses the level-0 offset table), so decoders should be created once per thread and
kept, not per lookup.

At ~3 000 tile decodes/s/thread, a cold miss is expensive relative to a shading
event — which is precisely why OIIO has an `ImageCache` and why we need one too (§6).
The cache, not the decoder, is what makes this fast.

---

## 4. Caveats found (each one is a real bug waiting to happen)

### 4.1 More than 4 channels is silently mis-strided — **guard against this**

For an 8-channel float `.tx`, the `tiff` crate reports the wrong shape and returns
half a tile's worth of samples *without erroring*:

```
SamplesPerPixel tag   = Ok(8)
ExtraSamples tag      = Ok([1, 0, 0, 0, 0])
colortype()           = RGBA(32)      <- wrong; num_samples() == 4
chunk_buffer_layout   = len: 65536    <- 64*64*4*4, half the tile
read_chunk(0)         -> 16384 f32 (4 per pixel over 64x64)
```

The values returned are real data, but mis-strided: what the crate presents as pixel
1's RGBA is actually pixel 0's channels 4–7 (verified against OIIO — pixel (0,0) is
`[0.125231, 0.974213, 0.700333, 0.297271, 0.526511, 0.118902, 0.894079, 0.016789]`,
and the crate returns those eight values as "pixels 0 and 1"). Passing a correctly
sized 8-channel buffer to `read_chunk_bytes` does not help — it fills the first
65 536 bytes and leaves the rest zero.

The mechanism is in `decoder/image.rs::colortype()`: for `PhotometricInterpretation::RGB`
it matches on `photometric_samples` (samples minus extra samples = 3 here) and, seeing
`ExtraSamples[0] == AssociatedAlpha`, returns `RGBA`, discarding the other four
samples. `ColorType::Multiband` exists but is only used for non-RGB interpretations.

**Mitigation:** read the `SamplesPerPixel` tag yourself and reject (or handle by raw
byte unpacking) anything > 4. Do not trust `colortype()`.

### 4.2 `planarconfig separate` splits every tile into per-channel chunks

`maketx --separate` / `--prman` writes planar files. Then `tile_count()` returns
`tiles × channels` (48 for a 16-tile 3-channel level) and each `read_chunk` returns
**one channel plane** (4096 samples for a 64×64 tile). Correct, documented crate
behaviour — but a reader that assumes interleaved RGB will silently produce garbage.
Detect via the `PlanarConfiguration` tag and de-interleave, or reject.

### 4.3 Compression can differ *per MIP level*

Observed on a file requested as LZW: level 0 came out `lzw`, levels 1+ came out `zip`.
So compression must be read per IFD, not once from level 0. `seek_to_image` re-parses
the directory so the `tiff` crate handles this correctly — but any hand-rolled fast
path that caches "the" compression method for a file would break.

### 4.4 The deflate path is unbounded (harmless, but know why)

`decoder/image.rs` builds `DeflateReader::new(reader)` **without** limiting it to
`compressed_length` (unlike LZW and PackBits, which are given the length). flate2 will
therefore pull ahead past the tile's compressed bytes. Consequences: the over-read is
bounded by the `BufReader` capacity and is transient — steady-state RAM is unaffected
(measured 2.8 MiB while sweeping 4096 tiles). Keep a `BufReader` of 64 KiB or so; do
not set it tiny (which makes it read-syscall-bound) or huge (which inflates the
over-read).

### 4.5 A requested `half` `.tx` silently becomes `float` — but a `half` TIFF is legal

Covered in detail in §1.4. Summary for implementers: by default OIIO promotes `half` to
`float` when writing TIFF (silently, doubling the data), so most float-ish `.tx` files
in the wild are `f32`. With `tiff:half=1` a real `half` TIFF exists; the `tiff` crate
decodes it correctly but hands it back as `F32`, and `colortype()` reports `RGB(16)`
while the buffer is `f32`. **Dispatch on the `DecodingResult` variant, never on
`ColorType`'s bit depth** — and cover `F16` too, since the variant exists.

### 4.6 The EXR flavour needs a different, clumsier access pattern

The `exr` crate reads the OpenEXR-flavoured `.tx` correctly, including per-tile,
per-level access — verified pulling exactly one tile:

```
exr crate (blocks): headers=[(Vec2(256, 256), 27 blocks)]
  level 2 tile (0,0): pixel_size=Vec2(64, 64) pos=Vec2(0, 0) 24576 bytes decompressed
  -> 1 chunk(s) decoded for that one tile        (= 64*64*3*2, exactly one half tile)
```

But the ergonomics are worse than TIFF's. The high-level builder only offers
`largest_resolution_level()` / `all_resolution_levels()` — a
`specific_resolution_level` is an explicit upstream TODO
(`src/image/read/samples.rs:47`). Per-tile access means the low-level route:

```rust
let reader = exr::block::read(BufReader::new(File::open(path)?), false)?;
let filtered = reader.filter_chunks(false, |_meta, tile, _block| {
    tile.level_index == Vec2(level, level) && tile.tile_index == Vec2(tx, ty)
})?;
let mut dec = ChunksReader::sequential_decompressor(filtered, false);
while let Some(block) = dec.decompress_next_block() { /* block.data */ }
```

`filter_chunks` reads and sorts the whole offset table on every call, so it is a
*streaming* filter, not random access. For a texture cache you would parse `MetaData`
once, keep the offset tables, and drive `Chunk` reads yourself — everything needed for
that is public (`exr::block`, `MetaData::read_offset_tables`, `UncompressedBlock`), it
is just more code than TIFF's one-line `read_chunk`.

Do not defer this on the assumption that EXR is the rare case: per §1.4/§1.5 it is the
*normal* container for float and half textures. Budget the offset-table work as part of
the first implementation rather than as a follow-up, and expect the EXR backend to be
the more expensive of the two to write (the TIFF one is nearly free).

### 4.7 Things not tested here

BigTIFF decoding is present in the crate (`bigtiff` handling in `Decoder::new` and
the offset readers) but no >4 GiB `.tx` was exercised. UDIM tile sets
(`tex.<UDIM>.tx`) are a naming convention above the file reader and are entirely
unimplemented. `textureformat` values other than `"Plain Texture"` (shadow maps,
lat-long and cube-face environments) parse fine but mean nothing to this renderer yet.

---

## 5. What this renderer is missing before any of it matters

This is the honest part. `.tx` tile reading is maybe 20 % of "use MIP-mapped
textures". The repository currently has, verified by inspection:

| Need | Current state |
|---|---|
| **UV coordinates** | None. `HitRecord` (`hittable.rs`) is `{p, normal, t, front_face}`. `crust_rt::RayHit` carries `u`/`v` but those are **barycentrics**, not texture coordinates. |
| **Per-mesh UV data** | None. `TriangleMesh` in `crust-rt` holds positions + optional shading normals only. `usd_import.rs` never reads `primvars:st`. |
| **Texturable material inputs** | None. Every `OpenPBR` field is a constant (`Vec3A`/`f32`). There is no `Constant | Texture` input abstraction. |
| **USD texture binding** | None. The shader dispatch in `usd_import.rs` handles `UsdPreviewSurface` and `crust:openpbr`; `UsdUVTexture` nodes and their `file`/`st`/`wrapS` inputs are not consumed. |
| **A footprint / LOD estimate** | None. No ray differentials, no ray cones, no `dpdx`/`dpdy` on the hit. |

That last one is the conceptual crux, and it is worth being explicit: **"reading the
right tile" presupposes knowing the right MIP level.** Without a footprint estimate
you would either always read level 0 (no RAM win, and aliasing) or guess. The standard
options, in ascending cost:

- **Distance-based heuristic** — level from `log2(t · pixel_spread / |dpdu|)`. Crude,
  but it is a handful of lines, needs no new state on the ray, and captures most of
  the RAM benefit. A reasonable first cut.
- **Ray differentials** (Igehy 1999) — track `∂P/∂x`, `∂D/∂x` alongside the ray;
  correct for primary and specular paths, awkward through diffuse bounces.
- **Ray cones** — a scalar width + spread per ray, cheap to carry and to widen at
  rough bounces. Probably the best fit for a path tracer that already threads a
  `Ray` struct through everything and would rather not carry two extra `Vec3A`s.

For a path tracer, the pragmatic production answer is: differentials (or a cone) for
primary rays, and after the first non-specular bounce, clamp to a coarser level driven
by roughness. Blurrier-than-ideal indirect texture lookups are a well-accepted trade;
they are also *cheaper*, because coarse levels are tiny and stay cache-resident.

**Ordering consequence:** the `.tx` reader is genuinely independent and can be built
and tested first (it needs nothing from the renderer, and `samples/` can gain a `.tx`
fixture). But it delivers nothing visible until UVs, a texturable material input, and
a LOD estimate land. Plan for that, and don't let the reader's easiness set the
expectation for the feature.

---

## 6. Proposed architecture

Mirror the precedent this repository already set with `crust-rt`: factor the
self-contained, swappable subsystem into its own crate behind an API shaped like the
industry-standard one — so that an OIIO FFI backend could later be dropped in behind
the same seam if the project's constraints ever change.

```
crates/crust-tex/          (new; depends on: tiff, exr, glam — no crust-core)
  src/
    lib.rs        TextureSystem: the only thing crust-core sees
    file.rs       TextureFile: magic sniff -> Tiff | Exr; per-level metadata
                  (dims, tile size, channels, sample format, compression),
                  wrap modes + textureformat from the Pixar tags, oiio:SHA-1
    tile.rs       TileKey { file: FileId, level: u8, index: u32 }, Tile { texels }
    cache.rs      TileCache: bounded, sharded LRU; per-thread Decoder pool
    filter.rs     bilinear within a level, trilinear across two levels,
                  wrap-mode handling, channel fill rules (1->grey, 3->rgb, 4->rgba)
```

Design points that the measurements in §3 dictate:

- **`TextureSystem::texture(handle, s, t, footprint) -> [f32; 4]`** — an
  OIIO-`TextureSystem`-shaped call. `crust-core` never sees tiles, decoders, or MIP
  levels, exactly as it never sees BVH nodes.
- **The cache is the performance story, not the decoder.** A miss is ~330 µs (§3.4);
  a hit must be a hash lookup and a few flops. Shard by `TileKey` hash to avoid a
  global lock across Rayon workers; `RwLock` per shard, or a lock-free map.
- **One `Decoder` per (thread, file, level)**, created once. `Decoder::new` is ~1 ms
  and `seek_to_image` is 143 µs, so both must be amortised out of the lookup path.
  A `thread_local!` pool of open decoders is the simple, safe answer — no `mmap`, no
  `unsafe`, which keeps the safe-Rust property intact.
- **Store tiles in the cache as `f32` (or `f16`) RGBA**, converted once at decode
  time. Filtering then has one code path instead of one per sample format, and
  `uint8` textures cost 4× their file size in cache — accept it, or store native and
  convert per lookup if memory matters more than ALU.
- **Bound the cache in bytes, not tiles**, and make it a render setting
  (`crust:textureCacheMiB`, defaulting to something like 512 MiB). This is the knob
  that actually delivers the brief: memory becomes a *configured ceiling* independent
  of how many textures the scene binds.

Memory arithmetic, for the 4096² float texture measured above:

| Approach | Resident |
|---|---|
| Whole level 0 in RAM | 192 MiB **per texture** |
| Whole MIP pyramid in RAM | ~256 MiB **per texture** |
| Tile cache, 48 KiB/tile, 1024 tiles | **48 MiB total, for all textures** |

A scene with 50 such textures: ~12.8 GiB versus a 48 MiB ceiling. That is the win, and
it is the reason every production renderer works this way.

Integration points in the existing code, in dependency order:

1. `crust-tex` standalone, with **both** a TIFF `.tx` and a tiled MIP-mapped `.exr`
   fixture in `samples/` and unit tests that pin tile values (the golden-value style
   already used by `openqmc-rs`). Cover `u8`/`u16`/`f32`/`f16` sample types and assert
   the guards of §4.1/§4.2 fire rather than returning mis-strided data.
2. `crust-rt`: optional per-vertex UVs on `TriangleMesh`, interpolated by the hit
   barycentrics. Alternatively keep UVs entirely in `crust-core` and look them up by
   `geom_id`/`prim_id` — which fits `rt_world.rs`'s existing `geom_id`-indexed
   material table and keeps the kernel free of shading data. **Prefer the latter**;
   it matches the "the kernel never sees materials" invariant.
3. `HitRecord`: add `uv: Vec2` (and, when LOD lands, a footprint scalar).
4. `Material`: a `TexInput<T>` enum (`Constant(T) | Texture(TextureHandle, …)`) for
   `OpenPBR`'s inputs, resolved in `scatter_importance`/`eval`. Note this touches the
   MIS-critical paths — the `eval` contract ("the `None` decision must never depend on
   `wi`") still has to hold once inputs vary spatially.
5. `usd_import.rs`: handle `UsdUVTexture` in the shader dispatch, resolve `file`
   (asset paths relative to the stage), `wrapS`/`wrapT`, `scale`/`bias`,
   `sourceColorSpace`, and `primvars:st` on the mesh.
6. LOD: differentials or cones (§5), gated behind a setting so it can be compared
   against always-level-0.

---

## 7. Recommendation

Build **Option B**: a `crust-tex` crate with magic-byte dispatch over **two backends** —
`tiff` for the TIFF flavour (true per-tile/per-level reads, nearly free to write) and
`exr` for the OpenEXR flavour (per-tile reads via the block/offset-table route, the
more expensive of the two). Treat EXR as first-class from the start: the container is
selected by extension and never by bit depth (§1.4), so a float or half pipeline yields
tiled MIP-mapped EXRs at least as often as TIFF `.tx` files (§1.5).

Guard hard against the >4-channel and `planarconfig separate` cases (§4.1, §4.2) — both
are silent-wrong-data bugs, not errors — and support `F16` as well as
`u8`/`u16`/`f32` samples (§4.5). Size the cache in bytes and expose it as a render
setting.

Sequence the work so the reader lands first (it is independently testable), but budget
realistically: UVs, texturable material inputs, USD texture binding, and a LOD
estimate are the bulk of the feature, and the RAM benefit only materialises once
something can ask for a level other than 0.

## 8. Open questions

- **Scope of the first cut** — this doc now recommends both containers up front (§7).
  If that is too much for one pass, which single one matches your actual texture
  pipeline: TIFF `.tx` from `maketx`, or tiled MIP-mapped `.exr`?
- **Which LOD mechanism** — distance heuristic (cheap, approximate), ray cones
  (a scalar on `Ray`), or full differentials (two `Vec3A`s on `Ray`, and touching
  every ray-spawning site)?
- **Colour management** — `sourceColorSpace` on `UsdUVTexture` implies at minimum an
  sRGB→linear decode for 8-bit textures. In-scope or deliberately ignored for now?
- **UDIM** — needed, or explicitly out of scope?
- **Where UVs live** — on `crust-rt`'s `TriangleMesh` (kernel interpolates) or in a
  `crust-core` side table keyed by `geom_id`/`prim_id` (kernel stays shading-free)?
  This doc recommends the latter; it is a design call worth making explicitly.
