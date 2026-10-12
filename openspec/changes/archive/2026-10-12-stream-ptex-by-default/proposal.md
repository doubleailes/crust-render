## Why

On the Moana island, preloading Ptex costs 6.02 GiB, which is 5.4 GiB more than streaming
(measured 2026-10-01: peak RSS 43.1 → 37.3 GiB). But streaming is opt-in twice over.
`CRUST_PTEX_STREAM=1` alone streams nothing, because every mipmapped `.ptx` is refused
under the default `CRUST_PTEX_STREAM_MIPSPACE=linear`. The only way in is `=file`, which
accepts a mip chain reduced in the file's encoding, up to 0.147 darker under
minification.

The refusal protects an oracle the island does not actually reach. The preloaded backend
fetches each face at the 32×32 cap with `get_data_at_res`. On a face authored larger
than that, those texels are the file's own reduced level, encoded-space reduction
included. Only the levels *coarser* than the cap are rebuilt in linear light. So the
exact part of the preloaded chain is "the file at the cap, linear below it", and a
streamed texture can reproduce exactly that.

## What Changes

- **A third Ptex mip-chain policy, `capped`, becomes the default**
  (`CRUST_PTEX_STREAM_MIPSPACE=capped`). A streamed texture reads three regions
  differently:

  | region | source |
  |---|---|
  | **finer than the cap** | the file's stored levels: detail the preload discards, at no worse a reduction than the preload's own cap level |
  | **at the cap** | the file's level at the cap resolution: the same bytes the preload fetches |
  | **coarser than the cap** | derived in linear light from the cap level, with the same reduction the preloaded pyramid uses |

  At every resolution the preloaded backend holds, the streamed texels are
  **bit-identical** to it. The only difference is extra resolution above the cap.
  `linear` (refuse) and `file` (the file's whole chain) stay available for A/B.
- **`ptex-rs` caches derived levels.** `SharedReader` gains a host-supplied derived
  block: produced once per face and level by a reducer crust passes in, and held in the
  same LRU under the same byte budget as decoded blocks. There is still no second
  cache in crust: the design record's rule holds, and the reduction lives "beside the
  tile cache" upstream. A derived level reads only the cap-level face, never level 0.
- **`CRUST_PTEX_STREAM` defaults to on.**
  - The existing admission rule decides what actually streams: files under
    `CRUST_PTEX_STREAM_MIN_MB` (8 MiB) still preload, which is 3,579 of the island's
    3,618 textures.
  - So small scenes, and every checked-in Ptex sample, keep preloading and render
    exactly as before.
  - `CRUST_PTEX_STREAM=0` restores today's behaviour, and is the "off" side of the
    A/B.
- **Image change on scenes with large Ptex files**: where a face covers more than one
  cap texel per footprint, streamed textures now resolve their authored detail instead
  of the 32×32 cap. Elsewhere the image is bit-identical to the preload. This is an
  intended quality gain, not drift.
- **`--stats`** names the new policy on the Ptex `backend` line and reports derived-level
  residency (count and bytes) inside the streamed budget.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `textures`: the "Ptex" requirement changes. Large `.ptx` files stream by default
  under the `capped` chain, bit-identical to the preload at every resolution it holds.
  A mipmapped file no longer needs `=file` to stream. `linear` keeps the refusal as an
  explicit choice.

## Impact

- **`ptex-rs` (doubleailes fork):** a derived-block API on `SharedReader` (a host
  reducer, a cache key per face, level and policy, and accounting in `cache_stats`),
  released and re-pinned by `rev` in the workspace `Cargo.toml`.
- **`crates/crust-assets`:**
  - `ptex_stream.rs`: the `capped` level routing, the reducer adapter over
    `decode_face` and `reduce_level` (shared with `PtexColor`), and a `chain_is_exact` that is
    true under `capped`;
  - `lib.rs`: admission and default;
  - the `--stats` Ptex block.
- **`crates/crust-core/src/config.rs`:**
  - `ptex_stream` defaults to `true`;
  - `PtexMipSpace` gains `Capped`, which becomes the default;
  - `docs/architecture.md` § Environment switches is updated.
- **Bit-identity pair** (new; CLAUDE.md and `docs/architecture.md` § Invariants):
  streamed `capped` ↔ preloaded, at and below the cap, for every sample type. This is
  pinned by extending `crates/crust-assets/tests/ptex_stream.rs` to mipmapped fixtures
  with faces above the cap.
- **Render speed:** the island measured +1.2% Render with `=file` streaming. The
  derived levels add one reduction per face and level touched below the cap, cached.
  This change measures it with `bench_ab.sh` against `CRUST_PTEX_STREAM=0`.
- **Out of scope:**
  - a per-texture colour space for Ptex (still gamma 2.2 for all);
  - filtering across faces;
  - raising the preload cap.
