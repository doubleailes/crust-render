## 1. Baseline

- [x] 1.1 Build the parent commit's release binary (`bin_before`). Record
      `scripts/check_images.sh record <dir>` with it, using `--indirect-clamp 0`.
- [x] 1.2 Record the island baseline at level 0 under an RSS guard, in three
      configurations: default (preload), `CRUST_PTEX_STREAM=1
      CRUST_PTEX_STREAM_MIPSPACE=file`, and `CRUST_PTEX_STREAM=0`. Keep the EXRs, the
      `--stats` Ptex blocks and peak RSS.

## 2. Upstream: derived levels in ptex-rs

- [x] 2.1 Add the `DerivedLevels` trait, `SharedReader::with_derived` and
      `get_derived(faceid, k)`, holding derived blocks in the same LRU and byte budget,
      keyed by `(faceid, k)`.
- [x] 2.2 Produce the base derived block from `get_data_at_res(base_res)` through the
      host's decode, then each further level from its parent through the host reducer.
- [x] 2.3 Add `derived_blocks`, `derived_bytes` and `derives` to `CacheStats`.
- [x] 2.4 Write upstream tests: deterministic re-derive after eviction, budget
      accounting including derived bytes, no read finer than `base_res`, and
      concurrent `get_derived` from many threads.
- [x] 2.5 Tag the fork, and re-pin `ptex` by `rev` in the workspace `Cargo.toml`
      (keeping the `cache` feature). Pinned by `rev` while under review (a tag push
      runs the fork's crates.io release), then by version once 0.4.0 was tagged on
      the fork's main and published: `ptex-rust = "0.4.0"` from crates.io.

## 3. crust-assets: the capped chain

- [x] 3.1 Add `PtexMipSpace::Capped` and its `FromStr` / `Display`, and make
      `chain_is_exact()` true under it.
- [x] 3.2 Implement the `DerivedLevels` adapter: the decode through the existing LUT
      and `f32` paths, and the reduction through `reduce_level`, the function the
      preloaded pyramid uses.
- [x] 3.3 Route lookups in `PtexStream` by resolution: finer than `B` reads file tiles,
      `B` and coarser read derived blocks. Keep the four-slot microcache for both, and
      keep the slot ceiling and the reserve inside the budget.
- [x] 3.4 Extend `crates/crust-assets/tests/ptex_stream.rs` with mipmapped fixtures
      whose faces exceed the cap (`u8` 1 and 4 channels, `uint16`, `float32`). Assert
      bitwise equality with `PtexColor` for every face, at footprints no finer than
      one cap texel, under caps 0..=authored.
- [x] 3.5 Add a test that, at a footprint finer than one cap texel, the streamed `capped`
      texture reads a stored level finer than `B`, and equals the `file` chain there.
- [x] 3.6 Re-run the existing invariants unchanged: the budget-sum and admission tests,
      and `a_tile_larger_than_a_slot_is_never_retained`.

## 4. Defaults and reporting

- [x] 4.1 `crust-core/src/config.rs`: `ptex_stream` defaults to `true` and
      `ptex_stream_mipspace` to `Capped`. Update the doc comments, which today say
      "Off by default".
- [x] 4.2 Update `docs/architecture.md` § Environment switches: the new defaults, the
      three `MIPSPACE` values, and `CRUST_PTEX_STREAM=0` as the old behaviour.
- [x] 4.3 `--stats` Ptex block: `streamed (capped chain)` on `backend`, and the
      `derived levels` sub-line.
- [x] 4.4 Add `streamed capped ↔ preloaded at and below the cap` to the bit-identity
      pairs in CLAUDE.md and `docs/architecture.md` § Invariants.

## 5. Verification

- [x] 5.1 `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings` and
      `cargo test --workspace`.
- [x] 5.2 `scripts/check_images.sh check <dir>` with defaults against the 1.1 goldens:
      every sample bit-identical, because the sample `.ptx` files are under the
      admission threshold.
- [x] 5.3 `samples/ptex_quads.usda` with `CRUST_PTEX_STREAM_MIN_MB=0` and
      `CRUST_PTEX_MAX_LOG2=5` on both sides: `capped` streamed against preloaded,
      bit-identical.
- [x] 5.4 `bench_ab.sh`, the same binary with `CRUST_PTEX_STREAM=0` against the default,
      on the island (`-s 4`) and `ptex_quads` (`CRUST_PTEX_STREAM_MIN_MB=0`): min and
      mean Render, plus Load assets.
- [x] 5.5 Island under the RSS guard with defaults: Ptex resident, derived bytes and
      peak RSS against 1.2. Compare the image with the preload (relMSE, and where it
      differs) and with the `=file` render: coarse regions must match the preload, not
      `=file`.

## 6. Documentation

- [x] 6.1 `docs/ptex_streaming.md`: a "The capped chain" section with the argument that
      the preload's own base is the file's cap level, the derived-level design, and
      the 5.3–5.5 figures. Update "The opt-in, and what it costs to decline" and the
      `backend` reasons table.
- [x] 6.2 `openspec/specs/textures/design.md`: update "Texture residency switches" and
      replace the "mip chain cannot be built in linear light from streamed tiles" gap
      with what remains (the file levels above the cap, and the preload/stream
      threshold split).
- [x] 6.3 `docs/moana_profile.md`: the island memory figures with streaming on by
      default.
