## Context

How Ptex works today (`crates/crust-assets/src/ptex_texture.rs`, `ptex_stream.rs`,
`docs/ptex_streaming.md`):

- **The preload, `PtexColor`.** It opens a `PtexReader` and, per face, fetches
  `get_data_at_res(min(authored, cap))`, with a cap of `CRUST_PTEX_MAX_LOG2`, default
  5, i.e. 32×32. It decodes to linear `f32` RGB (gamma 2.2), then builds the mip chain
  *below* that base in linear light with `reduce_half` / `reduce_half_linear` (shared
  `axis_taps`). For a face authored above the cap, the base texels are therefore the
  **file's** reduced level, not a linear reduction of level 0.
- **The stream, `PtexStream`.** A sampler over `ptex::SharedReader`, an LRU of decoded
  blocks under a byte budget split across streamed files. It reads one tile of one
  level of one face at a time, with a per-thread four-slot microcache.
  - At a resolution both backends hold, it is bit-identical to `PtexColor`; that is
    the invariant everything rests on.
  - Unset, its cap means *uncapped*.
  - Its chain is gated by `CRUST_PTEX_STREAM_MIPSPACE`: `linear` (default) refuses any
    mipmapped file, and `file` accepts the file's whole chain.
- **Admission.** Files that would preload in less than 8 MiB preload anyway. Streaming
  is off unless `CRUST_PTEX_STREAM=1`.
- **The rule this design must respect** (textures design record, "Known gaps: texture
  residency"): no second pyramid cache in crust, and no bolting Ptex onto the `.tx`
  cache. The reduction belongs in the reader, beside its tile cache.
- `ptex-rs` is the doubleailes fork, pinned by `rev`, so an upstream API change is in
  reach.

## Goals / Non-Goals

**Goals:**
- Streaming on by default, with a chain that is bit-identical to the preload at every
  resolution the preload holds.
- Derived levels that live in, and are bounded by, the reader's existing budget.
- A derived level never reads a level finer than the cap. Its cost is bounded by the
  cap-level face (at most 32×32 by default), never by level 0.

**Non-Goals:**
- Making the levels above the cap linear-exact. They are the file's, as in every
  production Ptex renderer, and they are still closer to level 0 than the preload's cap
  level.
- Changing the preload, or the default cap of 5.
- A per-texture Ptex colour space, or cross-face filtering.

## Decisions

### 1. The chain is defined per face, relative to the preload's base

For a face with authored resolution `A` and cap `C`, let `B = min(A, C)` per axis. That
is exactly the base `PtexColor` would hold.

| streamed resolution | source |
|---|---|
| finer than `B` (between `A` and `B`) | the file's stored level at that resolution, read as tiles exactly as `file` does today |
| exactly `B` | the file's level at `B`, which are the same bytes `PtexColor` fetches |
| coarser than `B` | derived: level `k+1` is the reduction of level `k`, starting from `B` decoded to linear, by the function `PtexColor` uses |

Level *selection* stays footprint → resolution, as both backends already do. Where a
footprint is no finer than one `B` texel, both backends select the same resolutions
and read identical values, which makes them bit-identical. Where it is finer, the
streamed backend reads a finer stored level, or blends one with `B`, and the preload
clamps to `B`.

- **Alternative: derive the whole chain from level 0 in linear light**, the "pure"
  answer. Rejected. It reads the full-resolution face to answer a coarse lookup, which
  is the case that dominates a large distant set, and so defeats streaming where it
  matters. The island's authored total is 494 GiB.
- **Alternative: flip the default to `file`.** Rejected. It changes coarse-level
  values against the preload, darker by up to 0.147, which is the filtering-looking
  bug the refusal exists for.

### 2. Derived levels are reader-cached blocks, produced by a host reducer (upstream)

`ptex-rs` adds, behind its `cache` feature:

```text
trait DerivedLevels: Send + Sync {
    /// Produce derived level `k` of `faceid`, given its parent (level `k-1`, or the
    /// base block when `k` is the first derived level).
    fn derive(&self, faceid: usize, k: u8, parent: &DerivedBlock) -> DerivedBlock;
    fn base_res(&self, faceid: usize) -> Res;   // B for this face
}
SharedReader::with_derived(Arc<dyn DerivedLevels>)
SharedReader::get_derived(faceid, k) -> Result<DerivedBlock>
```

- **Storage.** A `DerivedBlock` is host-typed bytes (crust stores linear `f32` RGB) in
  the same LRU and byte budget as decoded tiles, keyed by `(faceid, k)`.
- **The `base_res` level.** The reader produces it from `get_data_at_res(B)` through
  the host's decode, so the decode stays crust's 256-entry LUT and the `u8` path's
  bit-identity is unchanged.
- **Eviction.** Derived blocks are evicted like any other block. Re-deriving is
  deterministic, so eviction cannot change a value.
- **`cache_stats`** gains `derived_blocks`, `derived_bytes` and `derives`.

The reducer is a closure over crust's existing `reduce_half_linear`, which is the
point: the preloaded and streamed chains run *one* function, the way
`reduce_half` ↔ `reduce_half_linear` already share `axis_taps`.

- **Alternative: a derived-level cache in crust-assets.** Rejected by the design record
  (a second cache, outside the budget, invisible to `--stats`).
- **Alternative: an offline conversion writing linear-reduced levels into a sidecar
  `.ptx`, `maketx`-style.** Rejected for now. The `.ptx` encoding quantises the result,
  so it cannot be bit-identical to the `f32` preload. It would also give up the "no
  conversion step" property `docs/ptex_streaming.md` is built on.

### 3. Defaults and switches

| switch | new default | values |
|---|---|---|
| `CRUST_PTEX_STREAM` | on | `0` preloads everything, the A/B "off" side and exactly today's default |
| `CRUST_PTEX_STREAM_MIPSPACE` | `capped` | `capped`, `linear` (today's refusal), `file` (today's opt-in) |

Other rules:
- `chain_is_exact()` is true under `capped` for every file.
- Admission (`CRUST_PTEX_STREAM_MIN_MB` = 8, the budget split, `MIN_PTEX_SHARE`) is
  unchanged. It is what keeps small scenes on the preload, and therefore keeps every
  checked-in sample's image.
- `CRUST_PTEX_MAX_LOG2` keeps meaning "the preload's cap". Under `capped` it is also
  the streamed chain's `B`.
- An explicitly set cap still caps the streamed finer levels, as it does today. That is
  how the two backends are compared at a resolution both hold.

### 4. Reporting

- The Ptex `backend` line gains `streamed (capped chain)`.
- The `streamed resident / budget` line includes derived bytes, with a sub-line
  `derived levels  N blocks, X MiB, Y derives`.
- The admission reasons table in `docs/ptex_streaming.md` drops "preloaded for a linear
  mip chain" from the default case.

## Risks / Trade-offs

- **Images change on scenes with large Ptex files.** Close-ups resolve detail above the
  32×32 cap that the preload discarded.
  → This is the intended quality gain. The spec pins that nothing changes where the
  preload holds the texels. The island before/after is recorded with relMSE and a
  crop, and `CRUST_PTEX_STREAM=0` reproduces the old image.
- **Inconsistent detail between files**: a 7 MiB file preloads capped, and a 9 MiB
  file streams uncapped.
  → Documented. The same split exists today under `=file`. A follow-up can stream
  above the cap for preloaded files too, or lower the threshold, once measured.
- **The first coarse touch of a face pays a decode and a reduction** of a block of at
  most 32×32 per face.
  → Cached, so it is paid once per face per budget lifetime. Measured with
  `bench_ab.sh` against `CRUST_PTEX_STREAM=0` on the island and on
  `samples/ptex_quads.usda` with `CRUST_PTEX_STREAM_MIN_MB=0`, the worst case.
- **The upstream API is a new surface** in `ptex-rs`.
  → Landed and tested in the fork first, behind the `cache` feature, then re-pinned by
  `rev`. crust adopts it in the same change.
- **A derived block evicted mid-render** is recomputed bit-identically. A budget too
  small to hold one derived block degrades to re-deriving per lookup: slow, but
  correct. The `evictions` line shows it, as it does for tiles.

## Migration Plan

1. Land `DerivedLevels` in `ptex-rs` with its own tests (derive determinism, budget
   accounting, eviction), tag it, and re-pin `rev`.
2. Add `capped` in crust-assets with its bit-identity tests while `file` / `linear` are
   unchanged, then flip the two defaults in `config.rs`.
3. Rollback: `CRUST_PTEX_STREAM=0` at run time, or revert.
