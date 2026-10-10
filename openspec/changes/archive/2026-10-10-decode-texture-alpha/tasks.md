## 1. Decode (crust-assets)

- [x] 1.1 `image_file::decode_with_alpha`: report an authored alpha, and not the
      `ExtraSamples = 0` sample the TIFF workaround declares alpha. Verify:
      `only_an_authored_alpha_is_reported`.
- [x] 1.2 `ALPHA_U8` / `ALPHA_STEPS` (`raw`'s tables), `drop_opaque_alpha`,
      `decode_rgb_of_rgba` (design D1, D2). Verify: `the_alpha_tables_are_raws`.
- [x] 1.3 Preload: `decode_tile` / `decode_exr_tile` (EXR `A` through
      `try_read_exr_texels`) keep a cutting alpha per tile; `reduce_half` /
      `reduce_half_linear` over RGBA; `UvTexture` dispatches `(storage, alpha)`
      (D3). Verify: the `uv_texture` alpha tests, and the RGB ones unchanged.
- [x] 1.4 Stream: `TileData::alpha`, `Tile::rgba_u8` / `rgba_half`, the TIFF
      reader's `alpha_sample` / `to_texels`, the EXR reader's `resolve_alpha`,
      `StreamingTexture` dispatches `(linear, alpha)`. Verify:
      `an_rgba_tx_round_trips_with_its_alpha`,
      `alpha_normalisation_puts_the_alpha_after_the_colour`.
- [x] 1.5 Write: `write_tx_rgba` (`ExtraSamples = 2`), `write_tx_exr_rgba`,
      `make_tx` keeps the alpha and reports it (`MadeTx::alpha`, the `maketx`
      example). Verify: `streamed_alpha_agrees_bit_for_bit_with_preloaded`,
      `an_exr_backing_streams_the_alpha_too`, `a_udim_set_streams_alpha_per_chart`.

## 2. Import (crust-core) and reports

- [x] 2.1 Remove `preview.texture_alpha` (`preview.rs`, `warnings.rs`, the
      warnings reference) and the `PreviewSurface` doc's alpha gap.
- [x] 2.2 `crust-check/1` → `crust-check/2` (D6): `check.rs`, the CLI and MCP
      docs and tests, `CLAUDE.md`, the `cli` design record.
- [x] 2.3 The issue's reproduction end to end through `FileAssets`, preloaded and
      `--auto-tx` streamed, and an RGB file reading opaque
      (`crust-assets/tests/texture_alpha.rs`). Verify: it fails on the base
      commit (opacity 1.0 on the transparent half) and passes here.

## 3. Measure and document

- [x] 3.1 Every sample bit-identical (`scripts/check_images.sh` recorded with the
      base binary, checked with this one).
- [x] 3.2 Instruction counts of an RGB-textured render, preloaded and streamed,
      against the base binary (callgrind, one thread).
- [x] 3.3 The `textures`, `materials` and `cli` design records,
      `docs/color_management.md`, and the user documentation (`usd/materials.md`,
      `reference/warnings.md`, `reference/command-line.md`,
      `help/claude-desktop.md`). Verify: `zola build` in `site/`.
- [x] 3.4 The CI set: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`.

## 4. Review follow-ups (Qodo)

- [x] 4.1 A tile whose alpha is opaque throughout is cached as RGB (D7).
      Verify: `a_tile_whose_alpha_is_opaque_is_cached_as_rgb`,
      `a_cutout_with_opaque_tiles_agrees_bit_for_bit_with_preloaded`.
- [x] 4.2 `crust:txversion=2` on every crust `.tx`; a versionless crust `.tx`
      beside a source declaring alpha is stale (D8). Verify:
      `a_tx_written_before_alpha_is_stale_for_a_source_that_has_one`,
      `a_tx_from_before_alpha_is_refused_or_reconverted`.
- [x] 4.3 Measurements labelled as instruction counts.
