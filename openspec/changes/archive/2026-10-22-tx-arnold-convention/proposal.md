# Proposal

## Why

A `.tx` that crust writes and a `.tx` that Arnold writes for the same texture never
meet. Each renderer looks for a different file name. They also disagree on what the
stored values mean. Crust stores sRGB-encoded bytes and records the source space in
`crust:mipspace`. Arnold stores values already converted to its rendering space, and
records that only in the file name. A studio that renders the same USD assets with both
renderers converts every texture twice. When one renderer is pointed at the other's
file directly, the colours come out wrong: sRGB data converted twice reads about four
times too dark (measured on Arnold 7.5, see `design.md`). The goal is that either
renderer can use the other's `.tx` files and get the same look within a small
tolerance. Crust gets there by adopting Arnold's convention outright and dropping its
own.

## What Changes

- **BREAKING** Automatic `.tx` lookup beside a source follows Arnold's naming:
  `<stem>_<canonical source space>_<target space>.<ext>.tx`, or `<stem>_raw.<ext>.tx`
  when no conversion applies. Crust no longer looks for, or writes, `<stem>.tx`.
  Existing crust `.tx` files are not found and get converted again.
- **BREAKING** `--auto-tx` and `examples/maketx` produce what Arnold 7.5's auto-tx
  produces for the same binding:
  - The same file type and bit depth: a half EXR with DWAA compression for a source
    space that has a transfer curve, the source's own depth in a zip TIFF for linear or
    data spaces, and a half EXR with zip compression for a float source.
  - The same conversion to the target space.
  - The same mip chain: OIIO's box filter, with level sizes rounded down.
  - The same flags: opaque, constant-colour and monochrome detection, NaN fixing and
    unpremultiply.
  - The same metadata: `oiio:ColorSpace`, `textureformat`, `wrapmodes`, `oiio:SHA-1`,
    `oiio:AverageColor`, `autotx`, and a `Software` string holding the `maketx` command
    line Arnold would run.
  - The same modification time, set equal to the source's.
- New `--tx-color-space <name>` (default `ACEScg`): the target space that file names
  and conversions use, independent of the render's working space. It is accepted by
  every subcommand that takes `--auto-tx`.
- **BREAKING** A `.tx` bound directly in USD is read in the binding's colour space,
  transfer curve included, whatever the file holds, as Arnold reads it. A `.tx` found
  by the automatic lookup is read in the space its name says it was converted to.
- **BREAKING** Removed: the `crust:mipspace` marker and the refusal of a `.tx` whose
  recorded space differs from the binding; `crust:txversion` and the staleness rule
  for a `.tx` written before alpha was carried; `--format` and crust's by-range choice
  of backing; the linear-light re-encoded TIFF mip chain. Staleness becomes "the
  `.tx` modification time differs from its source's", as Arnold's `maketx -u` decides
  it.
- **BREAKING** `auto` and an unset MaterialX `colorspace` resolve as Arnold resolves
  them (measured in `design.md`, decision 8):
  - `auto`: any 8- or 16-bit image is sRGB, and a half or float image is used as
    stored. Previously only 8-bit RGB / RGBA was sRGB.
  - MaterialX with no `colorspace`: a colour image is `auto`. Previously it was taken
    as already in the working space. A non-colour image is still never converted.
- **BREAKING** UV texture lookups return colour premultiplied by alpha, as Arnold's
  textures do. A straight-alpha source is premultiplied in linear light when it is
  loaded (`design.md`, decision 9). Partly transparent texels read darker than before.
- UsdUVTexture's `sRGB` token keeps meaning sRGB, under the canonical name. Arnold
  writes that binding unconverted under `_sRGB_<T>` with crust's OCIO config, so the
  two renderers do not share it (decision 1).
- **BREAKING** The guarantee that streamed and preloaded renders of 8-bit textures are
  bit-identical is withdrawn. Two tolerance checks replace it: streamed against
  preloaded, and crust against a `.tx` written by Arnold's `maketx`.
- Performance: a streamed sRGB colour texture is now cached as `half` (6 bytes a
  texel, not 3), so the same `CRUST_TEX_CACHE_MB` holds half as many colour texels, and
  tiles are decoded from DWAA instead of Deflate. Both costs are measured and recorded
  before the change lands. The sRGB curve and the change of primaries are no longer
  applied per lookup for these files.

## Capabilities

### New Capabilities

None. Everything here is texture residency and interpretation, which `textures`
already owns.

### Modified Capabilities

- `textures`:
  - Streaming is still an A/B of preloading, but under Arnold's file convention and
    within a tolerance instead of bit for bit.
  - New requirements cover the `.tx` file name, the conversion `--auto-tx` performs,
    and how a directly bound `.tx` is read.
  - The colour-space and minification requirements drop their `.tx` bit-identity and
    linear-light clauses.
- `cli`: the flags shared between `render`, `diagnostic`, `check` and `light-paths`
  gain `--tx-color-space`.

## Impact

- `crates/crust-assets/src/tiled/`: `make.rs` (conversion, naming, staleness),
  `write.rs` and `exr_write.rs` (Arnold-shaped files: DWAA, round-down levels, the
  OIIO metadata), `read.rs` and `exr_read.rs` (the `crust:mipspace` and
  `crust:txversion` reading goes away), `mod.rs`, and `cache.rs` and `stream.rs` (a
  half tile in a curved space is decoded at lookup). `crust-assets/src/lib.rs`
  (`prepare_tx`, the lookup).
- `crates/crust-render`: `main.rs` (the `--tx-color-space` flag), and
  `examples/maketx.rs` rewritten around the same conversion.
- `crates/crust-core`: `check.rs` and `diagnostic/checks.rs` (`textures_without_tx`
  counts the new file names), and the colour API (the config's canonical name for a
  space).
- Tests: the bit-identity tests for streamed `.tx` files
  (`a_streamed_tx_matches_the_preloaded_texture_in_acescg`, the alpha pair, and the
  staleness tests) are replaced. New tolerance tests run against Arnold's
  `maketx.exe` and `kick.exe` and are skipped where Arnold is not installed.
- Documentation:
  - `openspec/specs/textures/design.md`: Streaming textures, `.tx`, the invariant.
  - `docs/color_management.md`.
  - `docs/architecture.md`: the streamed ↔ preloaded bit-identity pair in Invariants.
  - `CLAUDE.md`: the pairs list.
  - Site pages: `reference/command-line.md` (`--auto-tx`, `--tx-color-space`), the
    texture and `maketx` pages, and `usd/materials.md`.
- Dependencies: none new. `exr` 1.74 already reads and writes DWAA. SHA-1 for
  `oiio:SHA-1` needs either a small in-crate implementation or a dependency audited
  with `cargo deny`; `design.md` decides which.
