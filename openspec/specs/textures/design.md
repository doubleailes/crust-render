# textures — design record

> Design record for the **textures** capability: the reasoning, measurements and
> history behind the behaviour `spec.md` states. Moved out of `CLAUDE.md`, which
> now keeps only the rules and pointers. Section and path references such as
> "above" or "see X" may point to another capability's `design.md` —
> `openspec/specs/*/design.md` is the whole record; `docs/architecture.md` is the map.

## Crate: crust-assets

- **`crust-assets`** (lib name `crust_assets`) — the host side of
  `crust_core::AssetLoader` for a program that reads files: `FileAssets`
  implements the trait over `exr`, `image` and `ptex-rs`, and the decoders
  behind it are public — `load_exr_environment` / `load_image_environment`,
  `parse_ies` / `load_ies` (IES LM-63 → `crust_core::IesProfile`),
  `PtexColor` (+ `read_channel`, `max_log2_from_env`), `PtexStream` (the
  tile-paging backend behind `CRUST_PTEX_STREAM`), `UvTexture` (UDIM sets,
  the `CRUST_TEX_MAX` cap) and the decode / re-encode tables built from the
  OCIO curves in `crust_core::color` (`TransferCurve`). Everything that knows a
  file format lives here, so the probe examples decode a texture *exactly* the
  way the renderer does instead of carrying copies (`read_channel` used to
  exist three times). Owns the `CRUST_PTEX`, `CRUST_TEX`, `CRUST_PTEX_MAX_LOG2`,
  `CRUST_TEX_MAX`, `CRUST_PTEX_STREAM` and `CRUST_PTEX_CACHE_MB` switches.

## Texture residency switches

Texture *residency* has two more. A texture with a `.tx` beside it (same path, extension
swapped: `foo.1001.exr` → `foo.1001.tx`) is streamed instead of preloaded, paging 64x64
tiles under a byte budget set by `CRUST_TEX_CACHE_MB` (default 1024, matching OIIO's own).
That lookup is **always on**: a `.tx` exists only because someone converted the texture
for streaming. `CRUST_TEX_STREAM=0` turns it off and preloads everything, which is the A/B
(`=1` is accepted and changes nothing). It falls back to preloading for any texture it
declines — no `.tx`, a UDIM set only partly converted (it would stream with black holes),
a mip chain reduced in a different colour space, a file it cannot read — so a stray `.tx`
can make a render slower but never break it. **`--auto-tx`** creates the missing ones:
before a texture opens, every tile whose `.tx` is missing or older than its source is
converted beside it (`tiled::make_tx_atomic`, in parallel, written to a temporary and
renamed so an interrupted run leaves no truncated `.tx` to trust). A float source keeps
`half` tiles even when its values fit in `[0, 1]` (`TxFormat::FromSampleType` —
`maketx`'s by-range default would band linear data), and the colour space recorded is
the one the material binds with, `auto` resolved. A tile that fails to convert preloads
its texture. Without the flag a stale `.tx` is still used, with a warning. **Ptex is
excluded from all of it** (`tiled::is_ptex`): a `.ptx` is already a tiled per-face mip
pyramid that streams as it is (`CRUST_PTEX_STREAM`), so it is never converted — `make_tx`
refuses one outright — and a `foo.tx` beside a `foo.ptx` is never read in its place, even
when the `.ptx` reaches `load_texture` as a UV texture. A `.tx` is backed by either a tiled TIFF (`u8`
tiles) or a tiled mip EXR (`half` tiles), picked by magic number rather than extension.

Ptex has the same pair. `CRUST_PTEX_STREAM=1` swaps `PtexColor` for a `PtexStream` that
pages one tile of one level of one face out of the `.ptx` under `CRUST_PTEX_CACHE_MB`
(default 1024, matching the UV budget), and falls back to preloading for a file it cannot
open — same policy, same reason. It needs **no conversion step**, which is the whole
difference between the two: a `.ptx` is already a tiled per-face mip pyramid, so the
missing piece was never a format but a cache, and that cache now lives in `ptex-rs`
(`SharedReader`) rather than here. With it on, `CRUST_PTEX_MAX_LOG2` stops being load-bearing:
an unset cap means *uncapped*, and setting one is how the two backends are compared at a
resolution both hold.
**`CRUST_PTEX_STREAM_MIPSPACE` is the second gate, and it is on the correctness side of
the trade rather than the tuning side.** A `.ptx`'s stored mip levels were reduced in the
file's own encoding while `PtexColor` reduces in linear light from the decoded base, which
is the same mismatch `crust:mipspace` **refuses** for `.tx` — and refuses for the reason
it is dangerous, not for tidiness: level 0 stays perfectly correct and only coarser levels
are wrong, so it reads as a filtering bug rather than a colour one. So it is refused here
too. The default (`linear`) declines to stream a texture whose lookups could reach such a
level and preloads it instead, which under a mip pyramid means *every* mipmapped `.ptx`:
`CRUST_PTEX_STREAM=1` alone therefore streams nothing on a normal render, and `--stats`
says so on the `backend` line. `=file` takes the file's chain and the residency with it
(the island figures below are all `=file` figures), and `CRUST_PTEX_MIP=0` is the third
way out — no pyramid, so nothing to get wrong, exact and uncapped. See
`docs/ptex_streaming.md`.

## UV textures

- **UV textures** (`texture.rs`, host decoder in `crust-assets/src/uv_texture/`) —
  the chart a `primvars:st` primvar carries, as opposed to Ptex's per-face
  parameterisation. `Texture2D` is `crust_mtlx::Texture` re-exported — the
  standalone reader has to name the sampler it consumes and crust-core adopts
  that name, the same way it adopts `crust_rt::Geometry`; a separate crust-core
  trait plus adapter would put a second vtable hop on every texel fetch that fat
  LTO cannot remove. It is shaped like `PtexTexture` and for the same
  reason: a production UDIM set is fourteen 4K images per map, so the host
  decodes and hands back a **sampler**, and `Texture2D::eval` takes
  **unwrapped** coordinates — `u = 3.4` is the fourth UDIM tile, not `0.4` of
  the first, and tile selection is the host's addressing job. Both of
  MaterialX's tile tokens are expanded (`TileToken`): `<UDIM>` → `1001 + u +
  10·v`, `<UVTILE>` → `u<u+1>_v<v+1>`. They name the same 10x10 grid, so tiles
  are keyed by UDIM number whichever token named the file. `AssetLoader::load_texture`
  carries the colour space with the path, because the file does not say: an
  8-bit PNG holding albedo is display-encoded while the same encoding holding a
  normal, a roughness or a mask is raw data. MaterialX states it per input
  (`colorspace="srgb_texture"`), and **anything else, including an absent
  attribute, means raw**.
  - **Plain `.tif` sources decode through `image`'s `tiff` feature**, which is
    the same `tiff` 0.11 the `.tx` reader uses. Every `image` decode (UV tiles,
    environment maps, `maketx`) goes through `crust-assets/src/image_file.rs`,
    because `tiff` 0.11.3 scrambles an RGB file whose fourth sample is
    `ExtraSamples = 0` (unspecified): it strips that sample at the wrong stride
    under the horizontal predictor. The result is plausible-looking streaky
    noise, not an error. The OpenPBR playground's `walls_*`,
    `drapedfabric_*` and `OJfoam_Normal` are written that way, and the walls
    rendered as multicoloured speckle. `image_file` rewrites the tag to
    unassociated alpha before decoding, which is exact because every caller
    drops alpha. Only a TIFF is read into memory to be patched; other formats
    stream from the file. A canary test
    (`the_tiff_crate_still_needs_the_workaround`) fails once an upgrade fixes
    the crate. `Cargo.lock` is committed, so it fails in the change that bumps
    `tiff`, never on a fresh clone.
  - `UvMap` (`rt_world.rs`) carries per-triangle **corner** UVs, not per-vertex:
    USD's `st` is usually `faceVarying`, and a vertex on a UV seam has one
    position but two texture coordinates. Held as the chart's values plus one
    index per triangle corner (12 bytes a triangle, plus a 4-byte density) and
    built only when the bound material reports `uses_uv()` — a cost an
    untextured production stage should not pay. It was 36 bytes a triangle of
    expanded corners and a stored tangent until `compact-triangle-storage`.
  - **Tangents are derived at the hit.** A tangent frame is world-space, so it
    was once built per *baked* placement and stored per triangle, and a
    prototype shared by N instances — N transforms against one table — got
    none. Now `World` reads the hit triangle's vertices from the kernel
    (`Scene::triangle_vertices`: the top-level scene for a baked mesh, the
    prototype through its placement for a direct static instance) and derives
    the tangent with the same arithmetic (`tangent_of`), so directly instanced
    meshes shade normal maps too. What still has no frame, by construction
    (`VertexSource::Unresolved`, never a lookup against the top-level scene):
    a prototype part placed through an instancer's group, whose hit id is a
    forwarded slot shared by every placement of that prototype, so the hit
    names no transform (`forwarded_placements_sharing_a_slot_get_no_tangent`);
    and a motion-blurred instance, intersected through a transform
    interpolated at the ray's time that no retained start transform
    reproduces (`motion_blurred_instances_get_no_tangent`). Closing both needs
    the kernel to hand back the traversed instance's composed transform with
    the hit. A mirrored baked placement's stored
    tangent paired the swapped vertex order with unswapped corners; the derived
    one un-swaps first (`tangent_unswaps_a_mirrored_placement`).
  - **The resolution cap is not an optimisation.** Fourteen 4096² tiles is 674
    MiB for one map, and the teapot's ceramic binds four across two materials.
    `CRUST_TEX_MAX` (default 1024) box-filters each tile down at load; tiles are
    kept as `u8` and converted through a 256-entry table on lookup, since the
    files are 8-bit PNGs and nothing recovers precision that was never there.
    **An `.exr` is the exception: it preloads as linear `f32`** (12 B/texel,
    through `read_exr_rgb`, the workspace's one EXR reader), because a linear
    albedo quantised to 8 bits bands in the shadows and anything above 1.0
    would clip — and ALab's thousands of UDIM EXRs otherwise loaded as nothing
    (`image` has no EXR decoder). The storage is chosen once per `eval` by a
    `match` on the texture, then `sample_tile::<T: Texel>` is monomorphised, so
    the `u8` path is unchanged to the instruction (`sample_level::<u8>`
    279,025,964 before and after on `materialx_basic -s 2`; the match costs
    `eval` +4.7%, whole-render +0.15%). An explicit curve on an EXR is applied
    once, at load. `.hdr` still takes the `u8` path, which keeps the
    streamed-versus-preloaded emission A/B below as documented.
  - **Each tile carries a mip pyramid** below that cap, read trilinearly at the
    hit's footprint (see "Texture filtering" below). Two details that are easy
    to re-break. Levels average in **linear light** and re-encode through the
    colour space's inverse curve, because averaging display-encoded bytes is
    not averaging light: a black/white checker comes out at 0.21 linear instead
    of 0.5, and the chain drifts darker at every level. The `CRUST_TEX_MAX`
    reduction deliberately does *not* — it averages in the file's own encoding,
    to keep a capped tile matching a DCC's preview of the same file — so the
    two conventions differ on purpose; `docs/color_management.md` records both.
    And axes halve by **`div_ceil`**, not `>> 1`: `decode_tile` reduces by an
    arbitrary integer factor, so an odd level 0 is routine (3000×2000 under a
    1024 cap is 1000×666), and the lookup maps `x = u·width − 0.5`, so flooring
    an odd axis drops its last half-texel and that level's domain slips against
    level 0's — visible as a crawl across mip transitions on a slow camera move.
    **An odd axis is then a resample, not a 2×2 box**, and `axis_taps` weights
    it by area: the lookup reads each texel as an equal-width slice of the
    whole tile, so a destination texel is the average of the source over
    exactly its own `src/dst ≤ 2` texels (at most three of them). Clamping the
    source index instead — reading the trailing texel twice and averaging it
    as though two were there — hands that column a third of the level's weight
    where it is owed a fifth, at *every* level: a 5-wide row of
    `[250, 200, 150, 100, 50]` came out `[225, 125, 50]`, mean 133 against the
    source's 150, and a 25-wide tile lit only at its right edge bottomed out
    **6× too bright** while the same tile lit at its *left* edge came out too
    dark — an 8× disagreement decided by nothing but which end the clamp was
    at. On an **even** axis every overlap is exactly 1.0 and the divisor
    exactly 4.0, so the reduction is bit-identical to what it was; every
    checked-in texture is 64×64, so no sample golden moves and the
    streamed-versus-preloaded invariant is untouched. Both halves of that hold
    only because `reduce_half` and `reduce_half_linear` share `axis_taps` —
    they back the TIFF and EXR `.tx` writers, and one fixed without the other
    would leave two internally consistent chains that disagree. A `.tx`
    written by an older build still carries old-filter odd levels; they are
    gitignored artefacts `maketx` regenerates, so this is a note rather than a
    migration.

## Streaming textures

- **Streaming textures** (`crust-assets/src/tiled/`) — the residency half of the
  texture problem, as opposed to the filtering half above. Used whenever a `.tx`
  stands beside the texture (see the environment overrides above, and
  `--auto-tx`); the preloaded `UvTexture` remains the correctness oracle and the
  path for any texture without one.
  - **Why.** Preloading makes memory scale with the scene's total texture
    footprint, which is the only reason `CRUST_TEX_MAX` exists — and a cap is a
    poor residency policy, because it discards authored detail permanently and
    still cannot help a scene that binds more than fits. Production renderers
    convert once, offline, to a tiled mip-mapped file and stream tiles behind a
    bounded cache, so memory scales with the *cache* instead.
  - **Two backings, one seam.** A `.tx` is either a tiled mip **TIFF** with
    `u8` tiles or a tiled mip **EXR** with `half` ones, and `TiledFile` picks
    between them by **magic number** — which is what makes `maketx --format exr
    -o foo.tx`, an EXR inside a file named `.tx`, simply work. The split is by
    *sample type*, not container: unsigned integer samples (8- and 16-bit TIFF)
    page in as `TileKind::U8` exactly as before, float samples as
    `TileKind::Half`. An 8-bit texture pays nothing for HDR existing — its
    tiles, its bytes and its bit-identical agreement with the preload path are
    untouched — and an HDR one is not silently clipped to fit an 8-bit cache.
    A tile's payload is bytes plus that kind rather than an enum over two
    buffers, and the sampler reads it through `Tile::rgb_u8` or `Tile::rgb_half`
    chosen by a const generic — for the measured reason two bullets down.
  - **Why EXR and not float TIFF.** TIFF can hold `f32`, but the format is the
    smaller half of the question and the industry already answered it: OIIO's
    `maketx --format exr` writes 64x64-tiled, full-MIPMAP, zipped `half` with
    `textureformat`/`wrapmodes` as first-class header attributes (better
    provenance than TIFF, which has to smuggle them through `ImageDescription`),
    V-Ray's native streaming texture format *is* tiled mip EXR, and Karma
    recommends `.exr` or `.rat`. Adding EXR is the opposite of diverging from
    OIIO; a TIFF-only path was the narrower one. `half` rather than `f32`
    because it is what every streaming texture format stores and what keeps an
    HDR tile at twice a `u8` tile rather than four times — a texture is shading
    input, not a render target.
  - **The EXR reader is hand-rolled around the block API; the writer is not.**
    `exr` writes tiles and mip levels through its ordinary public API
    (`Blocks::Tiles` + `Levels::Mip`), so unlike TIFF there is no container
    code at all — `exr_write.rs` is the pyramid, the de-interleave to planar,
    and the attributes. Reading is the sharp part: `filter_chunks`, the one
    entry point that looks like random access, **consumes** the reader and
    sorts the offsets, so `exr_read.rs` composes the layer beneath it —
    `MetaData::read_from_buffered` → offset table → seek → `Chunk::read` →
    `UncompressedBlock::decompress_chunk` → `lines()`. Three details worth
    keeping: `enumerate_ordered_header_block_indices()` supplies the
    `(level, tile) → chunk index` map (EXR mandates no chunk order, so it must
    not be assumed row-major); `decompress_chunk` returns **native**-endian
    samples, so reading them is a reinterpret; and the level sizes come from
    the header's own `RoundingMode` rather than from `div_ceil`, because
    `maketx` writes `ROUND_DOWN` while crust writes `ROUND_UP` to match
    `reduce_half`.
  - **The offset table is probed, not trusted.** `MetaData` is read through a
    `PeekRead` that may hold one byte it has consumed and not handed back, so
    the reader's position afterwards is either the table's start or one past
    it. The table is self-describing — chunks begin immediately after it, so
    its smallest entry equals its own end — and that identity picks between the
    two candidates. The current `exr` happens to land exactly right, so the
    fallback is forced by a test (`the_offset_table_is_found_even_when_the_
    reader_is_a_byte_late`) rather than left to rot.
  - **`.tx` is a plain TIFF.** Tiled 64x64, mip levels as chained IFDs,
    Deflate. `tiff` 0.11.3 reads one tile at one level with a real seek
    (`seek_to_image` + `read_chunk`, which walks the cached `TileOffsets`
    table); it **cannot write** tiled, so `write.rs` supplies the tile grid, the
    per-tile zlib stream and the tile tags while borrowing `DirectoryEncoder`
    for the header, IFD chaining and entry serialisation. Three upstream
    hazards are designed around: tiled **LZW** fails to decode (#395) so only
    Deflate is ever written; `PlanarConfiguration = 2` **panics** inside
    `expand_chunk` (#403) so planar files are refused at open rather than
    allowed to abort a worker; and the right-edge fix (#400) is in the
    `read_image` assembly path, which is why only `read_chunk` is used.
  - **A tile is written padded and read back clipped.** TIFF6 says a tile is
    always `TileWidth x TileLength`, but `tiff` returns `chunk_data_dimensions`,
    so an edge tile is narrower. Indexing it by the nominal edge reads 64 texels
    of stride into a 22-texel row and shears the right-hand column of every
    texture whose size is not a multiple of 64.
  - **The cache is OIIO's algorithm, not an LRU.** `check_max_mem` there is a
    clock hand giving each entry one second chance, `try_lock`ed so a thread
    that finds a sweep in progress carries on rather than queueing behind it —
    the budget is a target, not an invariant, and is briefly exceeded by
    whatever other threads insert while one sweeps. That is a few hundred lines
    of `std::sync`, which is why there is no `moka`/`quick_cache` dependency:
    both carry internal `unsafe`, and the whole workspace is
    `forbid(unsafe_code)` (`crust-core` is `deny`, for one test-only
    `GlobalAlloc`; `crust-jit` is `deny`, for calling generated code).
  - **Three tiers, and the top one does the work.** A per-thread microcache,
    then 64 sharded maps, then a decode. Measured on the alias scene: **98.6% of
    8.7 M lookups never reach a lock**, because a bilinear tap reads one tile
    four times and trilinear alternates between two levels. `with_tile` hands the
    tile to a closure rather than returning an `Arc`, because one refcount pair
    per texel was the difference between streaming costing 4x a preloaded render
    and costing 2x (sequential timings, not an interleaved `bench_ab.sh` run — a
    ratio this size is far outside the ~15% noise band, but treat the exact
    figures as approximate).
  - **Nothing on the lookup path may write memory another thread reads.** This
    one was learned on ALab (`docs/alab_profile.md`), where texture lookups were
    89% of render time and 72 threads rendered an estimated ~1.25x faster than 8
    (two `--profile` runs, 72 threads at 32 spp and 8 threads at 8 spp scaled to 32 —
    an extrapolation, not an interleaved `bench_ab.sh` measurement; it shows the
    scaling was poor, not by exactly how much). There were
    two causes, and each fix is load-bearing:
    - **The per-lookup counters are striped** (`StripedCounter`: 128
      cache-line-aligned slots, one per thread, summed at report time). A plain
      `AtomicU64` bumped on each of 3.3 G lookups was one line bouncing between
      72 cores, and alone it was most of the cost. Ptex's `PtexStream` counters
      take the same type for the same reason (not measured there).
    - **The microcache is set-associative by file**: 16 sets x 4 ways, where it
      used to be 2 slots shared by every texture. That was OIIO's number, and
      right for one texture per shading point. An ALab material interleaves ~5,
      so each `eval` found the previous texture's tiles and missed at both
      levels. That was a 25% miss rate, every miss a shard lock.
    Result (`bench_ab.sh`, min / mean): ALab at 32 spp, Render
    **45.1 / 47.1 s -> 6.34 / 6.63 s (-85.9%)**. Texture went 23.4 -> 1.57 us
    per `eval`, and microcache hits 74.9% -> 84.9%. The single-texture alias
    scene went **0.743 -> 0.081 s (-89%)**: its microcache already hit 98.6%,
    so the shared counter alone was ~90% of that render. `materialx_basic`
    (preloaded) and cornellbox are unchanged, and images are bit-identical. The cost is up to 64 tiles per thread held outside the
    budget, 108 MiB at 72 threads for `half` tiles. A shard hit also sets `used`
    only when it is clear, so a hot tile is not rewritten on every hit.
  - **The second backing must cost the first one nothing, and twice it did
    not.** `bench_ab` against the pre-EXR binary on the 8-UDIM alias scene said
    **+21%** on an 8-bit streamed render — a path that gains nothing from HDR
    existing. Callgrind found both causes and the fixes are load-bearing, not
    tidying. First, a `TileData` **enum** read per texel: matching it inside the
    `with_tile` closure grew that closure past what LLVM would inline, so
    `texel` went 414.9 M → 471.5 M instructions *and* grew a 216.7 M
    out-of-line `texel::{closure#0}` that had not existed. The payload is
    therefore bytes plus a `TileKind`, and the sampler is monomorphised over a
    `const HALF: bool` decided once per `eval` from the file's own kind — the
    information is per *texture*, so it does not belong in a per-texel branch.
    Second, and larger, the `dyn Backend` facade itself: `texel` asks for
    `tile_edge()` and `level()`, and routing those through a vtable is an
    indirect call in the hottest loop in a textured render. `TiledFile` now
    **copies** the geometry out of the backing at open and touches `inner` only
    to read a tile. Together: `texel::<false>` is 414,851,273 instructions,
    equal to the pre-EXR binary's to the instruction, whole-render instructions
    are +0.017%, and wall clock lands at −5.4% min / −6.1% mean (i.e. noise).
  - **The colour space is recorded in the file** (`crust:mipspace=`, in
    ImageDescription for TIFF and as a header attribute for EXR) and a mismatch
    is refused. It means "the space this file is to be bound with", and the two
    backings reach that from opposite directions. A TIFF `.tx` stores
    display-encoded texels and reduces its levels in *linear light*, so the
    space is baked into every level above 0: read an sRGB chain as raw and
    level 0 is perfectly correct while every coarser level is wrong — visible
    only under minification and, by eye, indistinguishable from a filtering
    bug. An EXR `.tx` stores **linear** texels, decoded once at conversion
    because EXR has no transfer curve of its own, so binding it under another
    space would apply a curve to data that has already had one removed. A file
    with no marker (anything `maketx` wrote) is accepted, since its chain came
    from OIIO's filter and there is nothing to match against.
  - **Which backing a conversion produces is decided by the source's range**,
    not its extension: `maketx` writes EXR for a source that actually carries
    values above 1.0 and TIFF otherwise, so a `.hdr` of an overcast sky does
    not pay double for a range it never uses. `--format tiff|exr` overrides
    either way, and a TIFF conversion that clips is warned about rather than
    done quietly.
  - **The invariant.** For any texture at or below the preload cap, streamed
    and preloaded renders must be **bit-identical** — same level 0, same
    `reduce_half`, same level selection. `samples/materialx_basic` at 16 spp:
    0 of 230 400 pixels differ. Measured on 8 UDIM tiles of 2048² (96 MiB
    authored), 640x360 at 4 spp: preload capped 54.16 MiB / 0.223s with detail
    discarded, preload uncapped 142.46 MiB / 0.231s, **streamed at a 16 MiB
    budget 15.94 MiB / 0.449s and bit-identical to the uncapped preload**. The
    ~2x render cost is the honest worst case — one textured plane at depth 2,
    so nearly every shading call is a fetch. That invariant is about the `u8`
    path and stays exactly as it was; the `half` path is where the two paths
    are *supposed* to disagree. Measured at the seam rather than in a render: a
    Radiance source whose white checks are at 8.0 comes back at 8.0 streamed
    and at exactly 1.0 preloaded (`to_rgb8()` clips it), while below 1.0 the
    two agree to within the 8 bits the preload path keeps — so the divergence
    is the range and not a different lookup.
  - **Conversion is explicit, or opt-in automatic.** `examples/maketx` converts by
    hand, and `--auto-tx` converts on first use (Arnold's `autotx`). Both run one
    conversion, `crust_assets::tiled::make_tx`. Automatic conversion stays behind a flag
    because a renderer that silently writes multi-gigabyte files next to a read-only
    asset library is a surprise nobody asked for.
  - **The microcache is keyed by cache as well as tile.** A `TileId`'s file index is
    unique only within one `TileCache`, while the per-thread microcache outlives any one
    cache. So a second `FileAssets` in one process (tests, a host rendering twice) was
    handed the first one's tiles on the same thread; the `clear_microcache()` calls in
    the cache tests were working around exactly that. Each cache now carries a
    process-unique `id` in the key. Measured free: `texel::<false>` is 446,589,173
    instructions before and after.
  - **Open files are bounded: one reader pool, capped, never blocking.** A miss needs a
    reader (a `tiff` decoder or a `BufReader<File>`, one open descriptor each), and a
    reader cannot be shared because every read takes `&mut`. Readers used to be pooled
    per file and never closed until the cache dropped, after the outputs were written,
    so descriptors grew as (files touched) x (threads that missed on a file together):
    24 files on 8 threads ended a unit test holding 187. ALab binds 6 832 `.tx`; frame
    1004 rendered for 22 minutes against the 1 024 soft limit and then failed its EXR
    write with `Too many open files`, after an unknown number of tile reads had already
    failed into their fallback colour at DEBUG. Now `TileCache` holds **one**
    `ReaderPool` for all files — the idle readers keyed by file, a last-use stamp per
    file with a `BTreeMap` to find the oldest, and a count of every reader that exists.
    `CRUST_TEX_MAX_OPEN_FILES` (default 256, `0` = the old unbounded pools) caps the
    *idle* readers. A miss pops this file's idle reader; failing that, at the cap it
    closes the least recently returned idle reader of another file, then opens. If
    nothing is idle the open happens anyway, and a reader returned while over the cap
    is closed rather than pooled — so **the peak is the cap plus the thread count**,
    and no render thread waits, which is the clock sweep's rule too. A condvar would
    give a strict bound and break that rule.
    - *Why not keep per-file pools and add a global count.* The count is easy; choosing
      what to close is not. At the cap the cache must find an idle reader on a *cold*
      file, which needs one order over every file. Without it the cheap policy (over
      the cap, don't pool the returning reader) lets dead files keep their descriptors
      forever while every live file pays a reopen per miss.
    - The pool's mutex is taken twice per **miss** — never per hit or texel — and held
      only for map operations: never across an open, a read or a close; readers leaving
      the pool are moved out and dropped after the lock is released.
    - An open failing with `EMFILE`/`ENFILE` (`cfg(unix)`) closes every idle reader and
      retries once. Other errors are not retried: a missing file would otherwise empty
      the pool on each of its misses.
    - A failed open or read is still a fallback colour, never a panic, but no longer
      silent: the file is named once at WARN (`FileSlot::warned`) and `main.rs` logs
      one WARN with the total at the end of the render.
    - `FileAssets::release_texture_files` closes every idle reader once the render is
      done, before any output is written, at any cap including `0`.
    - Scheduling only: readers are interchangeable cursors, so the cap cannot change a
      tile (pinned by `the_cap_does_not_change_a_single_byte`, cap 1 against 0).
      `--stats` prints the peak open count against the cap, and the reopens that the
      cap cost: opens on a file that had a reader closed (a TIFF reopen re-parses the
      header). A second reader opened while a file's first is in use is not one. The
      peak is counted from successful opens and actual closes, so an open that fails
      never raises it. `--stats` gives no advice on the cap: many reopens only say the
      scene touches more files than it, and the ALab numbers below show that costing
      nothing.
    - *Measured on ALab frame 1004* (2026-10-07, 72 threads, `ulimit -n 1024`, 5 722
      streamed files, 55 325 misses). Default cap: peak 259 descriptors in
      `/proc/<pid>/fd` (256 readers + stdio), 0 tile read errors, EXR
      written. `CRUST_TEX_MAX_OPEN_FILES=0`: peak 1 015 descriptors. The `EMFILE` retry
      drained the pool repeatedly and lost no tile (0 errors), and the
      image is bit-identical to the default cap's. At 32 spp under `--profile`,
      TextureLoad is 1.4% of thread time at 160.7 µs a miss at the default cap, against
      1.5% and 163.8 µs at `0`. `bench_ab.sh` (3 interleaved reps) puts the default cap
      at +1.4% min / +0.8% mean against `0`, below the noise floor. ALab's `.tx` are
      EXR, and an EXR reopen is one `File::open`; a TIFF reopen also re-parses IFD0, so
      a TIFF-heavy scene is where to re-measure before lowering the default. With
      reopens at no measurable cost, 256 stays. (These runs predate the current
      reopen and peak counting: they reported 31 657 and 26 339 "reopens", which
      included concurrent extra readers, and a cap-0 peak of 1 027 that counted
      failing opens. The descriptor peaks above come from `/proc`, not `--stats`.)

## Ptex

- **Ptex** (`texture.rs`, plus the decoder in `crust-assets/src/ptex_texture.rs`) — per-face colour textures via
  the pure-Rust [`ptex-rs`](https://github.com/doubleailes/ptex-rs) reader, driving
  `OpenPBR::base_color`. A material's `inputs:surfaceMap` asset is the hook (both of the
  island's Ptex shader paths — `PxrPtexture.filename` and `HwPtexTexture_1.file` —
  `.connect` to it, so no network walk is needed). Asset paths come from openusd's
  `resolved_path()`, which anchors against the *authoring* layer — essential here, since a
  production stage's `../../../textures/foo.ptx` is authored several directories below the
  root layer.
  - **crust-core still decodes nothing**, but the `AssetLoader` seam *inverts* for Ptex:
    an environment map crosses it as a decoded pixel buffer, whereas a `.ptx` — a per-face
    mip pyramid that can reach gigabytes — crosses it as a **sampler**
    (`load_ptex → Arc<dyn PtexTexture>`) the host owns. Defaulted to `None`, so existing
    hosts are unaffected.
  - **Face ids are mesh face indices**, so `triangulate` records per emitted triangle its
    source face plus which slice of that face's fan it is (`FanSlice`), and `World`
    resolves a hit's barycentrics into `(face_id, u, v)` — Ptex parameterises a quad
    `v0=(0,0) v1=(1,0) v2=(1,1) v3=(0,1)`, so the lower fan half gives `(u+v, v)` and the
    upper `(u, u+v)`. The table lives on `World` keyed by `geom_id`, **not** on `MeshGeom`,
    which is dropped the moment a mesh is baked or committed. A skipped face must still
    consume its face id or everything after it shades from the wrong texel
    (`face_table_tests`). `bake_indices`' mirror swap exchanges `u` and `v`, so the table
    carries that flag per placement. Built only when the material reports a
    `face_texture()`, so an untextured stage allocates nothing.
  - The table is carried on **both** geometry paths. A direct mesh gets it in
    `flush_meshes`; a prototype carries it on its `ProtoPart` and
    `attach_proto_parts` records it against the instance's `geom_id`. Wiring only
    the direct path is not enough and is not obviously broken either: the island's
    geometry is almost entirely prototype-based, so Ptex silently applied to none
    of it and every textured surface fell back to its constant `baseColor` — which
    for a `PtexBaseMaterial` is an unused placeholder, so the gardenias rendered
    flat red. A table belongs to a *slot*, which is always exactly one leaf geometry
    (the walk splits per bound mesh, and a group only concatenates its members'
    slots), so one table serves every placement and the `prim_id` a hit reports
    indexes it unambiguously however many instance levels it passed through. A
    grouped placement sets each slot's table on its own `geom_id`.
  - The host **preloads every face** into one immutable buffer by default (the streaming
    alternative is the next bullet): `PtexReader` reads from
    disk on each call (`&mut self`, pixel data uncached), and a path tracer asks from every
    thread in an unpredictable order. Faces load **mip-reduced**, capped at 32×32 by
    default (`CRUST_PTEX_MAX_LOG2` overrides as a log2 edge length) — full resolution is
    authored for close-ups, so `isLavaRocks`' 631 MB / 11 384-face colour file costs
    130 MiB instead of several GB, at a resolution far past what a 595×520 framing
    resolves. **Beneath that cap each face carries a full mip pyramid** down to 1×1,
    selected per hit by the ray cone's footprint (see "Texture filtering" below). The two
    answer different questions and it is worth keeping them apart: the cap is the
    *ceiling* on detail, the pyramid is what makes minification below it correct. The cap
    used to double as an accidental anti-aliaser, and now that it does not have to, it can
    come **down**: the island is recorded below at 1.84 GiB with a 16×16 base against 4.58
    GiB flat at 32×32, so 16×16 plus a pyramid is around 2.45 GiB — derived from that
    figure rather than re-measured — for under half the memory and better filtering at
    distance. `examples/tex_probe`'s budget table counts the pyramid, so that comparison
    can be made against a real asset directly. Levels are reduced **in memory from the
    decoded linear base**, not by asking the reader for each resolution: every extra read
    takes `&mut self` through the serial load loop (another seek and inflate) and comes
    back display-encoded, needing the `powf(2.2)` again — and averaging in that encoding
    is not averaging light, which is the whole reason the pyramid is built here.
    **Which texels get averaged is the file's business, though, not ours**: a
    `meshtype = triangle` Ptex packs *two* triangles into each square of texels, the
    upright one and its mirror across the anti-diagonal, so Ptex reduces three texels of
    the upright 2x2 with the one mirrored texel that completes it
    (`w-1-2u`, `w-1-2v` — note the index swap) rather than with the neighbour
    below-right. `PtexColor` reads `mesh_type()` once at open and picks `reduce_triangle`
    or `reduce_quad` accordingly; a 2x2 box over a triangle face mixes texels from both
    triangles and is wrong at every level above 0 by up to ~65% while still looking like
    plausible texture, which is why `triangle_levels_match_ptex_rs_reduction` compares
    against `ptex::utils::reduce_tri` rather than against an expectation written by
    hand, and why a second test pins that the two reductions really do disagree.
    Triangle faces also clamp both axes together, since the format defines only
    symmetric reductions for them. A level's offset is walked rather
    than stored — `Face` gains one `u8` in its existing padding, which over 2.5 M faces is
    the difference between free and a per-face offset array. The `+1/3` figure holds for
    square faces only: once a non-square face's short axis pins at one texel the chain
    halves rather than quarters, so 64×16 lands at 1.335×. Texels are decoded to linear once at load (the island's graph gammas raw
    Ptex, and `HwPtexTexture_1` declares `sourceColorSpace = "sRGB"`; treating the data as
    already linear overshoots albedo ~4×, which `examples/tex_probe` exists to settle).
    `docs/color_management.md` is the per-input inventory of which colour space every
    input is assumed to be in and what curve is applied — including the two gaps that
    are still open (`UsdPreviewSurface` colours are read undecoded, and nothing enforces
    that a new colour input states its space at all).
  - **Streaming** (`texture.rs`'s seam again, host side in `crust-assets/src/ptex_stream.rs`)
    — the residency alternative to that preload, opt-in via `CRUST_PTEX_STREAM=1` under
    a `CRUST_PTEX_CACHE_MB` byte budget (default 1024). `PtexStream` pages one tile of one
    level of one face through `ptex::SharedReader`, so memory scales with the cache
    instead of with the asset and **the resolution cap stops being load-bearing**: an
    unset `CRUST_PTEX_MAX_LOG2` means uncapped. Unlike the UV path there is no conversion
    step, because a `.ptx` is *already* a tiled per-face mip pyramid — the missing piece
    was a cache, and it is the reader's, not crust's. Preloading remains the default, the
    oracle, and the fallback for a file that will not open. Four things worth keeping:
    the base level is **bit-identical** to the preloaded one (0 of 57 600 pixels differ on
    `samples/ptex_quads.usda` at 16 spp with both capped alike, and texel-for-texel across
    four fixtures and every cap in `tests/ptex_stream.rs`); the **coarser levels are not**,
    since a streamed level is reduced on disk in the file's encoding while a preloaded one
    is reduced in linear light, which convexity makes the streamed chain the darker of by
    up to 0.147 — **so a texture that could read one is declined by default and preloaded**
    (`MipSpace`, `CRUST_PTEX_STREAM_MIPSPACE`, below); the microcache keeps **four** slots
    rather than the `.tx` cache's two,
    because a `.ptx` grids per *face* so a four-tile-corner tap is routine and two slots
    measured 0.000 hit rate there against 0.998 with four; and the budget moves residency
    only — a 4 MiB render is bit-identical to a 1 GiB one. `docs/ptex_streaming.md` has
    the measurements and the reasoning.
    **The mip chain is refused, not documented, and that is the project's own standard
    rather than a new rule.** `crust:mipspace` refuses a `.tx` whose levels were reduced
    in the wrong colour space, because the failure is invisible — level 0 is perfectly
    correct and only minification is wrong, which by eye is a filtering bug and nothing
    else. A `.ptx` has no marker and needs none: crust decodes colour Ptex by 2.2 and the
    file reduced before that, so the mismatch is unconditional for colour. A `Raw`
    (displacement) request has no curve, so its stored chain is already in the right
    space and is admitted. `MipSpace::Linear` (the
    default) therefore declines such a texture at admission and preloads it, reported as
    its own `backend` reason. The gate asks about the *texture*
    (`PtexStream::chain_is_exact`), not the switch, so the two configurations with no
    chain to get wrong still stream: `CRUST_PTEX_MIP=0` (exact *and* uncapped — the base
    level is the bit-identical one — at the cost of anti-aliasing), and a texture whose
    every face is one texel under the cap. `CRUST_PTEX_STREAM_MIPSPACE=file` is the
    opt-in that takes the file's chain instead; it is what the C++ `PtexCache` does and
    what **every measurement in `docs/ptex_streaming.md` was taken with**, the island's
    included — so with a pyramid on, `CRUST_PTEX_STREAM=1` by itself now streams nothing.
    That is deliberate, and the lever is one variable. The fix that would retire both
    gates is a reader that reduces in a declared working space: building the linear chain
    here would need a second pyramid cache (the design "Known gaps: texture residency" below
    rules out)
    *and* a level-0 read to answer a coarse lookup, which defeats streaming exactly where
    the island uses it.
    **`CRUST_PTEX_CACHE_MB` is the render's budget, not a file's**, and that takes work
    here: `ptex::SharedReader` owns its cache (right for a library, wrong for a scene),
    so N textures opened at the full budget would hold N times it — on a stage binding
    Ptex per element, like the island, the default 1 GiB would become tens of GiB and
    the feature would be unbounded in the texture count. `FileAssets::rebudget_ptex`
    divides one budget over the streamed textures as they arrive, **exactly** — with no
    floor under the share, because a floor is what breaks the bound: `max(budget / n,
    1 MiB)` hands out 39 MiB against a budget of 8, and overshoots *more* the smaller
    the budget gets. `MIN_PTEX_SHARE` (1 MiB) is read as a capacity instead — at most
    `budget / MIN_PTEX_SHARE` readers may stream and anything past that preloads
    (`budget_full`, which `--stats` names with a "raise CRUST_PTEX_CACHE_MB" hint) — so
    every admitted reader gets a usable share *and* `n * (budget / n) <= budget` holds
    by construction. It costs the default path nothing (1 GiB seats 1 024 readers, the
    island wants 39) and engages only when the budget is genuinely small, which is when
    honouring it matters. The `.tx` path gets all of this for free — every streaming
    texture there shares one `TileCache`.
    **The microcache is inside that budget too.** Its slots hold decoded `PixelData` the
    reader cannot count, so a slot has a 256 KiB ceiling (`MICRO_SLOT_MAX`, above any
    real tile and below the whole-face reads upstream refuses as `oversized`), the
    allowance `threads * MICRO_SLOTS * MICRO_SLOT_MAX` — capped at half the budget — is
    subtracted before the readers divide the rest, and `--stats` prints
    `thread tiles / reserve`. Without the ceiling a refused tile was retained anyway,
    four slots deep on every worker thread and invisible to both the budget and the
    report. At a 1 MiB budget a slot is 32 KiB against a 512 KiB face read, so retention
    is zero and every tap goes to the reader — slower, still correct.
  - **`--stats` prints a `Ptex` block**, and unlike the `Texture Cache` one it reports
    for *both* backends, because the first question it has to answer is which ran:
    `backend`, textures and faces, preloaded resident bytes, and for a streamed run the
    live resident/budget total, the three fetch tiers and evictions. Without it an
    island run's peak RSS is uninterpretable — the figure is dominated by geometry and
    the SBVH build transient, with Ptex residency buried inside it.
  - `CRUST_PTEX=0` declines every texture so the same scene renders on its constant
    `baseColor` — the A/B switch that separates a wrong Ptex lookup from a wrong material
    or wrong lighting. It applies to both backends.
  - **A `crust:openpbr` material cannot bind Ptex**, and nothing warns. `inputs:surfaceMap`
    is consulted only for `UsdPreviewSurface` and `PxrDisneyBsdf` — the two the island
    authors — because `decode_crust_openpbr` reads its shader's inputs 1:1 and never looks
    at the material's interface. A Ptex material authored the native way therefore renders
    on its constant `baseColor`, which is indistinguishable from `CRUST_PTEX=0`.
    `samples/ptex_quads.usda` uses `UsdPreviewSurface` for that reason.
  - **Verified numerically, not by eye** — a wrong face id or a transposed `(u,v)` still
    renders as plausible rock, so appearance proves nothing and the reference image
    (different camera, lighting, displacement and subdivision) proves less. Two
    render-free checks, both passing on the island:
    `ptex_verify` exploits the fact that each island `.ptx` embeds the base cage it was
    baked against (`PtexFaceVertCounts` / `PtexFaceVertIndices` / `PtexVertPositions`), so
    the texture can be asked which vertices *its* face N has and the answer compared with
    the mesh's face N. All 10 isLavaRocks meshes and isMountainA pass with face-vertex
    index sequences equal **in order** (45 536 and 134 012 indices respectively) — which
    pins the face correspondence *and* the corner ordering that fixes the UV orientation.
    `ptex_seams` then tests the quad convention itself against the file's `adjface` /
    `adjedge` data: mean texel difference across shared edges is 1.5–16x lower under
    `v0=(0,0)` than transposed, and 1.9–97x lower than between unrelated faces.
    Alongside those, 10 textures / 28 816 faces against 57 632 triangles (exactly 2 per
    quad). The importer warns when a texture's `numFaces` disagrees with its mesh. Not reproduced:
    displacement beyond the cage (`inputs:displacementMap` is now read raw through
    `PxrDisplace`, but at the default level 0 it moves cage vertices only); the island's authored
    `catmullClark` cages were measured *unrefined* (the default level 0 still leaves
    them unrefined, only shaded with smooth cage normals; Ptex is
    indifferent either way, since face ids index cage faces and subdivided face tables
    map back to them); and the reference's `islandsunEnv.tex` environment is a
    RenderMan-only format.

## Texture filtering

- **Texture filtering** (`ray.rs`'s `RayCone`, `camera.rs`, `rt_world.rs`, the two
  decoders) — how a texture lookup learns how much texture a pixel covers, which is
  what a mip level is chosen from. Both texture paths sampled a single resolution
  before this; minification was suppressed only by accident, because the memory caps
  threw away the high frequencies first.
  - **The footprint is a ray cone**, not ray differentials: a path tracer spawns one
    ray at a time, so the four extra rays differentials want have nowhere to come
    from, while two floats ride along free. `RayCone { width, spread }` is the
    footprint's **diameter perpendicular to the ray** and its growth per world unit.
  - **Primary rays** get `spread = Camera::pixel_span / |direction()|`. `get_ray`'s
    direction lands on the focus plane at ray parameter 1, so one pixel of `s` moves
    that point by `horizontal / res_w` — and dividing by the direction's length makes
    `focus_dist` cancel, leaving `2·tan(vfov/2)/res_h` down the frame's axis. Pinned
    by a test, because a wrong derivation here still looks plausible. The per-pixel
    `1/|direction()|` is kept rather than simplified away: at the frame edge the
    direction is longer and that pixel really does subtend less.
  - **Two invariants that are easy to break.** The grazing `1/|cos θ|` stretch is
    applied on the way *out* to a texture width and discarded — folded back into the
    cone it compounds at every bounce (five grazing hits is 3125×) and every deep
    texture reads its 1×1 level. And a bounce's lobe width comes from
    `ScatterSample::spread`, **not** from `pdf`: by the time the tracer sees a sample
    the pdf may have been replaced by the guide/BSDF mixture, so a near-mirror under a
    trained guiding field would report a broad density and blur its own reflection.
    A cosine lobe's pdf also goes to zero at grazing, which says nothing about how
    wide the lobe is.
  - **Cone → texture space** goes through a per-triangle **density**,
    `sqrt(parametric_area / world_area)`, on `UvMap` (chart units) and `FaceMap`
    (face units). Always built in the mesh's **local** frame with the placement's
    `cbrt(|det|)` recorded per `geom_id`: at `flush_meshes` a baked placement shares
    a local-space `FaceMap` while cloning its `UvMap`, and one scale cannot serve one
    table in world space and the other in local. `MeshArena::intern` is the single
    funnel every shared mesh passes through, so there are only two build sites (the
    other is the non-invertible bake, whose vertices are already world-space and
    which therefore takes scale 1.0). Being a *ratio of areas* a density needs no
    mirror-swap correction, unlike every other lookup in `rt_world.rs` — do not add
    a `swapped` arm. `FaceMap`'s parametric area is the constant 0.5 for every mapped
    fan slice (both quad arms are unit-determinant shears, `Triangle` is the
    identity), **except** on a subdivided mesh, whose triangles carry explicit
    sub-face UVs: the constant would over-estimate by 4^L, 64× at level 3, and send
    every Ptex lookup on the mesh to its coarsest level.
  - **`SideTables::Default` is hand-written for one field**: `scale` must be 1.0, not
    0.0. `attach_masked` pushes one per geometry, so a derived default divides every
    footprint by zero and hands each texture an infinite width — which renders as a
    perfectly plausible coarse mip.
  - **`0.0` means point-sample** throughout: both `eval` methods take a width and
    both read zero as "finest level", so a host that tracks no footprint gets the
    historical behaviour rather than a wrong one. `Op::Texture` scales the width by
    `uvtiling` alongside the coordinates — a texture tiled 10× is minified 10×.
  - **Magnification short-circuits before the `log2`.** It is the common case, its
    answer is level 0 regardless, and taking it through the clamp instead measured
    ~9% of render on a scene whose output does not change at all.
  - **Verified numerically** (`scripts/gen_texture_alias_scene.py --measure`), because
    a wrong mip level is a plausible blur. Aliasing does not converge, so each
    configuration is compared against a high-spp reference *of itself* — comparing
    filtered against unfiltered would measure the bias between two different correct
    answers. On the generated checkerboard plane, 16 spp against 1024 spp: RMSE
    0.01024 filtered against 0.04211 point-sampled, a **4.11× reduction**. Cost is
    ~1.7% of render on a magnified textured sample and ~19% on that plane, which is
    the honest worst case (one textured plane, depth 2, so nearly every shading call
    is a trilinear fetch).

## Known gaps: texture residency

- **Texture residency caveats.** Ptex streams now (`CRUST_PTEX_STREAM=1`, above and in
  `docs/ptex_streaming.md`), so what is left is the shape of it rather than its absence.
  The cache is the reader's, which is what this section used to ask for and is still the
  right place for it — do not grow a second one here, and do not bolt Ptex onto the `.tx`
  tile cache. What remains: the **mip chain cannot be built in linear light from streamed
  tiles**, because a streamed level comes off disk reduced in the file's own encoding
  while a preloaded pyramid is reduced in linear light — convexity makes the streamed
  chain the darker, measured at up to 0.147 on the tiled fixture, and the base level is
  bit-identical. That is now *refused* rather than merely recorded
  (`CRUST_PTEX_STREAM_MIPSPACE`, above), which makes the gap a live restriction rather
  than a wrong render: with a pyramid on, streaming is off unless the operator opts into
  the file's chain. Retiring both gates needs the reduction to happen in the reader,
  against a declared working space — not a second pyramid cache here, which would also
  have to read level 0 to answer a coarse lookup. `PtexColor` remains the default and
  the oracle. Both backends decode `uint8`, `uint16`, `half` and `float` Ptex
  samples at full range (no clip above 1.0). Every Ptex request carries a
  `ColorSpace` (`AssetLoader::load_ptex(path, space)`), applied per request by
  both backends through the same conversion (`ptex_space(space)`; the streamed
  backend resolves it once per file, not per texel): a displacement
  map asks for `RAW` and is read with no curve and no clamp, while every **colour**
  `.ptx` still asks for `g22_rec709` (converted into the working space) — there is
  no authored colour space for colour Ptex, so a linear HDR
  colour `.ptx` is decoded as though it were display-encoded. The import cache is
  keyed on `(resolved path, space)`, so a file read both ways is opened twice. The
  streamed `u8` LUT fast path covers 8-bit files in either space. Filtering across
  face boundaries is still not attempted (see the filtering caveats), and the streaming
  path does not change that. Cost on the worst case (`samples/ptex_quads.usda`, two
  textured planes filling frame): ~2.8x the preloaded render, against ~2x for the UV
  path's.
  On the UV side: automatic conversion needs `--auto-tx` and writes beside the asset
  (a read-only library preloads, with a warning per failed tile), 16-bit integer sources are still narrowed to 8 on page-in (deliberately —
  the renderer decodes `u8` through a 256-entry table, and two more bits of an LDR
  texture is not worth halving what the byte budget holds), there is no
  single-flight on a miss so two workers can decode the same tile at once (counted as
  "concurrent double fills", bounded by the thread count), and the cache is per-process
  rather than shared between renders. The EXR backing reads RGB (or a replicated single
  channel) and ignores alpha, refuses ripmaps, multi-layer and deep files, and requires
  square tiles; the TIFF writer still emits 8-bit RGB only, so an HDR conversion is an
  EXR conversion.

- **Streamed Ptex holds one file descriptor per `.ptx` for the whole render.**
  `ptex::SharedReader` keeps its `File` open, so the count scales with the streamed files
  (not files x threads, unlike the `.tx` pools before `CRUST_TEX_MAX_OPEN_FILES`), and
  that cap does **not** cover it. Today it is bounded only by accident: admission lets
  at most `budget / MIN_PTEX_SHARE` readers stream, so on the Moana island
  (`CRUST_PTEX_STREAM=1 CRUST_PTEX_STREAM_MIPSPACE=file CRUST_PTEX_STREAM_MIN_MB=0`,
  `ulimit -n 1024`, 2026-10-07) 952 of 3 618 files streamed and the process peaked at
  956 descriptors, 68 under the limit. With every file admitted
  (`CRUST_PTEX_CACHE_MB=8192`) it reached 1 024 during import: about 1 284 `.ptx` failed
  with `Too many open files` (WARN, constant base colour), then the USD stage itself
  failed to open and the render aborted. The fix belongs upstream in `ptex-rs`, beside
  its cache (close idle files, reopen on a miss), not in a second cache here.

## Known gaps: HDR texture range

- **An HDR texture's range now has a consumer, and the remaining limits are
  elsewhere.** The input that uses the range is **emission**, and MaterialX's `edf`
  is read: `uniform_edf` flattens to an `Emission` term beside the BSDF lobes, and
  `reduce()` pools those onto `emission_color`/`emission_luminance` with nothing
  clamping either. The path is unbroken — file → `.tx` → `Texture2D::eval` →
  `Op::Texture` → `Emission.color` → `emitted_at` → `next_emit` → film — so the claim
  is a sample scene now (`samples/materialx_emissive.usda`) rather than only a seam
  test. Measured on it at 32 spp: the same frame preloaded and streamed differs on
  72.6% of pixels with a **max absolute difference of exactly 15.0**, which is the
  bright cell's authored 16.0 against the 8-bit path's clamp at 1.0; `mtlx_shade`
  prints the two side by side as `(1.000 1.000 1.000)` and `(16.000 9.000 3.000)`.
  What is still true: `base_color` above 1 is **still** clamped by `eon_diffuse`, and
  correctly — an albedo above 1 creates energy where a radiance above 1 is just a
  bright light. The preloaded `UvTexture` still narrows a non-EXR source to 8 bits at
  `to_rgb8()`, so an HDR `.hdr` emission texture needs `CRUST_TEX_STREAM=1` and a
  converted `.tx`; without them it renders, clipped, rather than failing. An `.exr`
  preloads at `f32` and keeps its range. And an **emissive MaterialX surface
  is not a light-list entry** — see the MaterialX caveats below.
  The **dome-light / HDRI path was never affected by any of this**, which is worth
  stating so the next reader does not go looking for a clamp that is not there:
  `load_exr_environment` and `load_image_environment` decode to `Vec<Vec3A>` through
  `to_rgb32f` (never `to_rgb8`), `EnvironmentMap` stores `f32`, and
  `exr_environment_round_trips` pins that a value above 20 survives. The workspace's
  one `to_rgb8()` is on the preloaded UV-texture path, which an environment map
  never takes. So the Moana island's domes carry whatever range their files hold;
  that `islandsunVIS.png` is an 8-bit PNG is a property of the asset, not of crust.

## Known gaps: texture filtering

- **Texture filtering caveats.** Minification is filtered now (ray cones plus
  trilinear mip pyramids, above), so what remains is the shape of that filter
  rather than its absence. It is **isotropic**: a chart stretched in one axis is
  filtered by the geometric mean of the two, so grazing minification over-blurs
  where an EWA or ripmap filter would not — the `1/|cos θ|` stretch widens the
  footprint without giving it a direction. Cone spread ignores **surface
  curvature**, so a reflection in a curved mirror filters as though the mirror
  were flat, and ignores the **lens aperture** (a non-negative cone cannot
  express a footprint converging to the focus plane — harmless, since defocus is
  resolved by sampling) and the **IOR change across a refraction**. The
  **base-resolution cap remains**: the pyramid retires aliasing, not the ceiling,
  so a close-up still cannot resolve past 32×32 (Ptex) or 1024 (UV). **Nested
  instances** filter against the outer placement's scale only — the inner
  placements live inside a committed kernel scene and the kernel does not surface
  the instance chain — and a **non-uniform** placement collapses to `cbrt(|det|)`,
  so a `(1, 1, 10)` scale is off by up to ~4.6× on the stretched axis. Both
  degrade to a slightly wrong level, never to a wrong lookup. Ptex still does not
  filter across **face boundaries**, and on a **triangle** Ptex it does not filter
  across the packed anti-diagonal either: the mip chain is Ptex's own triangular
  reduction now, but `sample_level` is still a plain bilinear tap on the square, so a
  tap within half a texel of the diagonal picks up the mirrored triangle where
  `PtexTriangleFilter` would not. The face mapping is right either way — a
  three-vertex face resolves through `FanSlice::Triangle`, whose barycentrics *are*
  Ptex's parametric coordinates. The guide branch of `sample_bounce_direction`
  reports the widest possible lobe spread rather than the material's own, since it
  never picked a lobe; that costs sharpness only on guided secondary bounces,
  where the cone is near-saturated anyway.

## Known gaps: UV textures

- **UV texture caveats.** Normal maps need a tangent, which prototype parts
  placed through an instancer's group and motion-blurred instances still lack
  (above). On the
  `UsdPreviewSurface` side: texture **alpha** is not carried (both samplers return
  opaque RGB, so `outputs:a` reads 1.0 before `scale`/`bias`), `UsdTransform2d` is
  not evaluated, `occlusion`/`specularColor` are not read (`displacement` is, at import —
  see the `usd-scene-import` design record § Displacement), and a
  texture's `fallback` default when unauthored is the surface input's constant, then
  its schema default, rather than the spec's opaque black (deliberately — see above). A **subdivided** mesh
  shades through its *refined* chart (`faceVarying` under the mesh's
  `faceVaryingLinearInterpolation`, `vertex` like the points; see the subdivision
  section of `openspec/specs/usd-scene-import/design.md`), never the cage's UVs on
  refined triangles, which would stretch every texture across the patch it came from. Only
  `primvars:st` (and `uv`/`st0`/`UVMap` as fallbacks) is read; there is no
  general primvar plumbing and no second UV set. `texcoord`'s `index` input is
  ignored for the same reason. And `decode_tile`'s `CRUST_TEX_MAX` resize has a
  cousin of the odd-level defect the mip chain was just fixed for: it takes
  `floor(sw / factor)` destination texels and **drops the remainder columns**
  rather than covering them, so a 2050-wide source under a 1024 cap loses one
  column of 2050 and the tile's domain slips by that much. Under a destination
  texel, against the full mis-weighted one the mip chain had — and unlike that
  one it is a *resize* averaged in the file's own encoding, so fixing it would
  move every render of a texture above the cap. Worth doing, not urgent.
