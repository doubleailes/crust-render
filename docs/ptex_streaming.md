# Streaming Ptex

How `CRUST_PTEX_STREAM=1` works, what it is bit-identical to, where it is
deliberately not, and what it costs. Written the way `docs/color_management.md`
is — as the record you consult before blaming the streaming path for something,
rather than as a feature announcement.

## Why, and why it took a dependency bump

Preloading makes memory scale with the scene's total Ptex footprint. On the
Moana island that is 2 576 238 faces: **4.58 GiB** at the default 32×32 cap,
1.84 GiB at 16×16, and **494 GiB** at the authored resolution. So
`CRUST_PTEX_MAX_LOG2` was never a tuning knob — it is the thing that makes the
island loadable at all, at the price of discarding authored detail permanently
and of still being unable to help a scene that binds more than fits.

The UV path answered this a while ago with `.tx` tile streaming. Ptex could
not, and "Known incomplete work" was specific about why and about where the
fix belonged:

> That is a limitation of the *reader*, not of crust: a `.ptx` is already a
> per-face mip pyramid and `ptex-rs` already addresses it randomly through
> `get_data_at_res`, so the fix is a `PtexCache` equivalent in `ptex-rs` —
> exactly as the C++ Ptex library ships one — rather than a second cache in
> `crust-assets`. Do not bolt Ptex onto the `.tx` tile cache; the two formats
> want different keys and the per-face pyramid is already on disk.

Upstream now ships exactly that. `ptex::SharedReader` is a `&self` reader over
an LRU of decoded blocks under a byte budget, and `tile_layout` / `tile_info` /
`get_tile` make one tile of one level of one face addressable without
materialising the face. `crates/crust-assets/src/ptex_stream.rs` is therefore a
*sampler over* that reader and not a cache: level selection, tile addressing,
the colour decode, and a per-thread microcache.

**There is no conversion step**, and that is the substantive difference from
the `.tx` path. `maketx` exists because a `.png` is not tiled or mip-mapped; a
`.ptx` already is both. The missing piece was never a format.

## What it changes for the caps

| | preloaded (default) | streamed |
| --- | --- | --- |
| `CRUST_PTEX_MAX_LOG2` unset | 32×32 per face | **uncapped** |
| `CRUST_PTEX_MAX_LOG2` set | that ceiling | that ceiling |
| memory scales with | the scene's Ptex | `CRUST_PTEX_CACHE_MB` |
| mip chain | reduced in linear light, in memory | the file's, off disk |

An unset cap meaning "uncapped" is the point of the feature. An explicitly set
one still applies, because that is what makes the two backends comparable at a
resolution both hold — which is how every equality below is measured.

## The invariant

**At a resolution both backends hold, streaming changes where the texels live
and nothing else.**

End to end, `samples/ptex_quads.usda` at 16 spp with `CRUST_PTEX_MAX_LOG2=5` on
both sides:

```
320x180  differing pixels: 0/57600 (0.0000%)
max abs diff: 0e0   rmse: 0e0
```

Texel for texel, `crates/crust-assets/tests/ptex_stream.rs` compares
`PtexTexture::eval` bit-for-bit across four fixtures (`u8` 1- and 4-channel,
`uint16` triangle, `float32`), every face, ~1 400 sample points per face, and
caps from 0 up to the authored resolution. The `u8` decode table is pinned
against the scalar decode it memoises, which is what lets that be an equality
rather than a tolerance.

The capped-reduction arm is tested separately (`a_capped_reduction_agrees_too`)
because it is the one where `is_tiled` is false and a "tile" is the whole face
— a bug there would hide behind the tiled fixture passing.

**The budget moves residency, not the image.** A 4 MiB-budget render of the
sample scene is bit-identical to a 1 GiB one.

## Where the two legitimately differ

Above the base level, and in one direction.

A preloaded pyramid is reduced **in memory from the decoded linear base**,
because averaging display-encoded texels is not averaging light — that is the
whole reason `PtexColor` builds its own chain instead of asking the reader for
each resolution. A streamed level cannot be: it is on disk, reduced by the
writer (or recomputed by the reader) in the file's own encoding, and decoded to
linear only once it arrives.

`x^2.2` is convex, so the mean of the decoded texels is never below the decode
of their mean: **the streamed chain is the darker one.** Measured on
`quad_tiled.ptx` across six footprint widths and the full sample grid:

```
mip-chain divergence: streamed darker by up to 0.1474, brighter by at most 0.0
```

This is the same defect `crust:mipspace` guards against for `.tx`. It is
accepted here rather than fixed because the fix — reducing in linear light from
streamed base tiles — means a second pyramid cache of crust's own, which is
precisely the design ruled out above. It is also what every production Ptex
cache does.

In a render it shows up only under minification. On the sample scene uncapped,
8 898 of 57 600 pixels differ from the uncapped preload (rmse 0.023), all of
them in the minified tiled panel.

## Four microcache slots, not two

`tiled::cache`'s per-thread microcache keeps two entries and measured 98.6% of
8.7 M lookups never reaching a lock. The Ptex one keeps **four**, and the
difference is measured rather than inherited.

A `.tx` is one tile grid over the whole texture, so a bilinear tap straddling a
seam is rare. A `.ptx` grids **per face**, and the faces large enough to be
tiled are exactly the ones a streamed render spends its time in — so seams are
routine, and a lookup on a *four-tile corner*, where the u and the v tap both
straddle, needs four distinct tiles for its four taps. With two slots each tap
evicts one the same lookup is about to ask for:

| | 2 slots | 4 slots |
| --- | --- | --- |
| tap inside a tile | 0.999 | 0.999 |
| tap on a four-tile corner | **0.000** | 0.998 |

A total thrash, for one extra `Option` pair per thread and a linear scan that
finds its hit at index 0 either way. Both halves are asserted in
`the_microcache_absorbs_most_taps`, so shrinking the slot count back fails the
test rather than quietly costing a render its cache.

## The budget is the render's, not a file's

`ptex::SharedReader` owns its cache. That is the right shape for a *library* —
a `.ptx` is a self-contained pyramid and a reader should not need to know about
its siblings — but it means N textures opened at `CRUST_PTEX_CACHE_MB` each
hold **N times** it. The `.tx` path never had this problem: every streaming
texture there shares one `TileCache`, so the total is the budget by
construction.

It is not a rounding error on a production stage. The Moana island binds Ptex
per element, so the default 1 GiB would have become tens of GiB — and the
feature whose entire purpose is to bound residency would have been unbounded in
the texture count.

`FileAssets::rebudget_ptex` divides one budget over the streamed textures as
they arrive, re-dividing on each open (the count is only final when the import
is, and a texture has to be usable the moment it is opened). An even split
rather than the demand-driven pool OIIO would use — that would need a second
cache here, which is the design ruled out above — but the property that matters
holds: **the total is what was asked for.** `MIN_PTEX_SHARE` (1 MiB) floors each
share, because a zero budget in `ptex::CacheOptions` disables caching outright
and overshooting the total beats silently turning the cache off — but it is a
backstop, not the policy; see the next section for what actually bounds the
reader count.

Pinned by `one_budget_is_shared_across_textures_not_repeated_per_file`, which
first asserts the naive total *does* multiply — so the sharing is testing
something.

## Not every texture is worth a cache slot

The island is what forced this, and the numbers are its own. It binds **3 618**
`.ptx` totalling **5.98 GiB** preloaded, and they are Pareto-distributed:

| | textures | share of bytes |
| --- | --- | --- |
| top 2 (`trunk0001`, twice) | 0.06% | 42% |
| top 25 | 0.7% | 88% |
| >= 1 MiB | 167 (4.6%) | 97% |
| < 1 MiB | 3 451 (95%) | 3% |

The median texture is under a kilobyte. An even split over all 3 618 gives the
four textures holding *half the bytes* a 0.3 MiB cache each — smaller than one
of their faces, so every read comes back `oversized` and nothing caches at all
— while 3 451 sub-kilobyte files each hold a slot they can never fill.
Flooring the share instead (this module's first answer) multiplies out:
3 618 x 4 MiB is **14.1 GiB**, worse than the 5.98 GiB preload it replaces.

So admission is per texture, and priced against the alternative:
**a texture smaller than the cache slot it would occupy should just be
preloaded.** `PtexStream::preload_bytes` answers what preloading would cost
from the parsed header alone — `face_infos()` carries every face's resolution
with no pixel I/O — so the test is exact and free. Below
`DEFAULT_STREAM_MIN_MB` (8 MiB, `CRUST_PTEX_STREAM_MIN_MB`), preload.

| threshold | streamed | share each | preloaded | total | vs preload |
| --- | --- | --- | --- | --- | --- |
| 0 (admit all) | 3 618 | 0.3 MiB | 0 | 1.0 GiB | nothing caches |
| 1 MiB | 167 | 6.1 MiB | 0.18 GiB | 1.18 GiB | 5.1x |
| **8 MiB** | **39** | **26 MiB** | **0.54 GiB** | **1.54 GiB** | **3.9x** |
| 64 MiB | 14 | 73 MiB | 1.15 GiB | 2.15 GiB | 2.8x |

8 MiB is the balance: a lower threshold wins on paper but starves each reader
below a working set, and the headline total is worthless if nothing caches.
`CRUST_PTEX_STREAM_MIN_MB=0` admits everything, which is what the sample
scene's A/B needs — its fixtures are kilobytes, so by default they preload and
the comparison would measure nothing. `--stats` says `backend` either way.

Pinned by `the_island_distribution_stays_bounded_under_admission`, whose
bucket table is derived from the 3 618 real rows rather than invented.

## Reading it in `--stats`

`--stats` prints a `Ptex` block, and unlike the `Texture Cache` block it reports
for **both** backends, because the first question it has to answer is which one
ran:

```
Ptex
  backend                      streamed
  textures                     2 (6 faces)
  streamed resident / budget   11.98 KiB / 8.00 MiB over 2 textures
  texel fetches                5 612 932
    thread microcache hits     5 612 515 (100.0%)
    reader cache hits          366
    reads from disk            8
  evictions                    0
```

against a preloaded run's:

```
Ptex
  backend                      preloaded
  textures                     2 (6 faces)
  preloaded resident           38.06 KiB
```

`backend` can also read `N streamed, M preloaded (fell back)`, which is not a
bug — streaming falls back per file — but is exactly when you want to be told.
`evictions` running with the misses is the line that says the budget is under
the working set, the one thing raising it fixes.

Without this block an island run's peak RSS cannot be interpreted: the number
is dominated by geometry and the SBVH build transient, so a streamed run and a
preloaded one differ by the Ptex residency buried inside a much larger figure.

## Measured on the Moana island

Everything above is fixtures and a synthetic scene. This is the asset the
feature was built for: `island.usda` at 640x360, 8 spp, `CRUST_PTEX_STREAM=1`
with a 2 GiB budget, against the same build preloading.

| | preloaded | streamed | |
| --- | --- | --- | --- |
| Ptex resident | **5.98 GiB** | **0.61 GiB** | **9.8x less** |
| textures | 3 618 preloaded | 39 streamed + 3 579 preloaded | |
| faces | 2 564 203 | 949 554 streamed, 1 614 649 preloaded | |
| Ptex decode | 84.8 s | 13.8 s + 0.2 s to open | **-71 s** |
| `Load assets` | 01:40.7 | **27.3 s** | |
| `Traverse prims` RSS | 47.34 GiB | **41.48 GiB** | **-5.86 GiB** |
| peak RSS | 51.28 GiB | **47.08 GiB** | **-4.20 GiB** |
| `Render` | 13:57.0 | 14:06.8 | **+1.2%** |
| total | 20:07.4 | 18:58.7 | |

Three things worth taking from that.

**The render cost is +1.2%, not the 2.8x the sample scene shows.** That scene
is two textured planes filling frame at depth 3, built to be the worst case;
the island is traversal-bound, so the fetch cost disappears into it. Both
numbers are honest and the gap between them is the point — the sample scene
bounds the cost, the island shows what it is in practice.

**Streaming also made the render *start* faster.** Preloading 5.98 GiB means
decoding 2.5 M faces, 84.8 s of it; streaming opens 39 headers in 0.2 s and
decodes only the 0.54 GiB it declines to stream. `Load assets` fell from
01:40.7 to 27.3 s, which is most of why the whole run finished 69 s sooner
despite the render being slightly slower.

**Peak RSS fell less than residency did (-4.20 against -5.37 GiB), and that is
expected.** Peak lands at `Commit acceleration structure`, the SBVH build
transient, not at texture load — so the last word on the island's memory is
still the build, and Ptex residency is only what it builds on top of. The
figure to watch for this feature is the `Traverse prims` RSS, which fell by
the full amount.

### The cache barely filled, and that is the mechanism working

`streamed resident / budget` read **3.28 MiB / 2.00 GiB**, with **0 evictions**
and 18 026 disk reads against 4 496 112 texel fetches (90.2% absorbed by the
thread microcache).

Three megabytes, from textures that preload to gigabytes. The reason is the
ray cone: at 640x360 a tree trunk covers a handful of pixels, so the footprint
asks for a *coarse* level of each face, and a coarse level is a few texels.
Streaming reads only the resolution the framing resolves. **Preloading
structurally cannot do that** — it decodes every face at the cap whether or not
the camera ever sees it, which is what the 5.98 GiB is.

So the budget was ~600x oversized for this framing, and that costs nothing: it
is a ceiling, not an allocation. Do not "optimise" it downward on the strength
of this number — a closer camera or a 4K frame walks up the same faces at
finer levels and will use it. The number that says the budget is too small is
`evictions` running with the misses, and here it is zero.

## Cost

`samples/ptex_quads.usda` is built to be the honest worst case: two textured
planes filling the frame at depth 3, so nearly every shading call is a texture
fetch and there is no geometry for the kernel to spend time in. Interleaved
min-of-7 (sequential comparisons on a loaded machine lie — see "Measuring a
change"), uncapped preload against a 4 MiB streamed budget:

| | render | peak RSS |
| --- | --- | --- |
| preload, uncapped | 0.123 s | 18.36 MiB |
| stream, 4 MiB budget | 0.342 s | 10.27 MiB |

~2.8×, against the UV path's ~2× on its own worst case. Both are worst cases
and neither is what a real frame looks like; what the table is for is the
shape — residency bounded by a number you choose, paid for in fetch cost.

## Testing it yourself

```bash
# The equality.
CRUST_PTEX_MAX_LOG2=5 cargo run --release -- -i samples/ptex_quads.usda -o a.exr
CRUST_PTEX_MAX_LOG2=5 CRUST_PTEX_STREAM=1 \
    cargo run --release -- -i samples/ptex_quads.usda -o b.exr
cargo run --release -p crust-render --example exr_diff -- a.exr b.exr

# The point: uncapped, under a budget, with the counters.
CRUST_PTEX_STREAM=1 CRUST_PTEX_CACHE_MB=64 \
    cargo run --release -- -i samples/ptex_quads.usda --stats -o c.exr

# The unit invariants.
cargo test -p crust-assets --test ptex_stream -- --nocapture
```

`samples/ptex_quads.usda` is the repository's first scene to bind a `.ptx`, so
`scripts/check_images.sh` covers Ptex now by its `samples/*.usda` glob. Its
textures are under `samples/textures/` — four files from the reference C++ Ptex
writer, see `ptex_fixtures.md` there — kept in one place so the unit invariant
and the rendered one cannot be checked against different bytes.

## Known gaps

- **Not the default**, and should not become one until it has been run against
  a real asset. Everything above is measured on fixtures and a synthetic scene;
  the island is the test that matters and needs the DPEL download.
- **The mip chain divergence** above. Bounded and one-directional, but real.
- **Ptex is 8-bit through both backends** (`PtexColor` and `PtexStream` both
  decode to `f32` but the island's files are `u8`), so an HDR `.ptx` gains
  nothing here — the same gap the `.tx` EXR backing closed for UV textures.
- **No single-flight on a miss**, inherited from the reader: two workers can
  decode the same tile at once. Bounded by the thread count.
- **No cross-face filtering**, unchanged from preloading — see the filtering
  caveats in CLAUDE.md. Streaming neither helps nor hurts it.
- **A `crust:openpbr` material still cannot bind Ptex at all**, streamed or
  not: `inputs:surfaceMap` is consulted only for `UsdPreviewSurface` and
  `PxrDisneyBsdf`. Unrelated to residency, found while writing the sample
  scene, and worth fixing separately.
