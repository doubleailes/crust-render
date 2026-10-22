# Tasks

## 1. Characterise Arnold 7.5 as checked-in reference data

- [x] 1.1 Add `scripts/arnold_tx_oracle.py`, which runs Arnold's `kick`, `maketx` and
  `oiiotool` (from `ARNOLD_BIN`, default `C:\Program Files\Arnold-7.5.0.0-windows\bin`)
  over small sources with **distinct** names, one per `color_space` row in design.md,
  including `raw` and `Raw` as separate files. It records for each the `.tx` name, file
  type, sample type, compression, rounding mode, channels, `Software` string and
  modification time into `crates/crust-assets/tests/data/arnold_tx/cases.json`, plus
  the `.tx` files themselves (kept small: 37x23 and 64x64). Verify: the script runs
  twice with identical JSON, and the `raw` row is settled in design.md Open Questions.
- [x] 1.2 Extend the script with a USD fixture (`UsdUVTexture` `sourceColorSpace`
  sRGB / raw / auto / unset; MaterialX `colorspace` set / unset) rendered through `kick`,
  recording the `.tx` names Arnold's USD procedural produces. Verify: `cases.json` has a
  `usd` section, and any name that differs from design decision 1 is written into
  design.md before group 2 starts.
- [x] 1.3 Measure what `maketx -u` compares in `Software`. The script writes a `.tx` with
  the source's modification time and a `Software` of `crust-render : maketx <Arnold's
  args>`, then runs `maketx -u` and records whether it was rebuilt. Verify: design.md
  decision 4 states which prefix is used, citing the result.
- [x] 1.4 Record the direct-binding table (the `flat` renders in design.md Context) in
  `cases.json` and check that `exr` 1.74 decodes Arnold's DWAA fixture tile by tile in a
  `crust-assets` test. Verify: `cargo test -p crust-assets arnold_dwaa_tiles_decode`
  passes.

## 2. Naming and the target space

- [x] 2.1 Add to the colour module the OCIO config's canonical name for a resolved space
  (next to `Space::label`), keeping the authored string available for the `raw` keyword.
  Verify: a unit test maps `srgb_tx`, `sRGB Encoded Rec.709 (sRGB)` and `auto` on 8-bit
  RGB to `sRGB Encoded Rec.709 (sRGB)`, and keeps `raw` and `Raw` distinct.
- [x] 2.2 Replace `tx_sibling` with the Arnold name (design decision 1): `_raw` or
  `_<C>_<T>`, `.<ext>.tx`, with UDIM tokens kept in the stem. Verify: a test checks every
  name in `cases.json` (both the `image` and the `usd` sections), byte for byte.
- [x] 2.3 Add `--tx-color-space` (default `ACEScg`) to the shared scene flags of `render`,
  `diagnostic`, `check` and `light-paths`, validated against the OCIO config at parse
  time and passed to `FileAssets`. Verify: `crust-render/tests` cover the two new `cli`
  scenarios, and `cargo run --release -- render --help` lists the flag.
- [x] 2.4 Make `check` / `diagnostic`'s `textures_without_tx` count the new names.
  Verify: `cargo test -p crust-core --test diagnostic` and `cargo test -p crust-render
  --test check` pass, with fixtures using Arnold names.
- [x] 2.5 Document `--tx-color-space` in `site/content/docs/reference/command-line.md` and
  in `openspec/specs/cli/design.md`. Verify: `zola build` (0.21) in `site/` succeeds.
- [x] 2.6 Resolve `auto` and an unset MaterialX `colorspace` as Arnold does (design
  decision 8). `auto` gives sRGB for any 8- or 16-bit image. An unset `colorspace` on a
  MaterialX colour image is `auto`, and a non-colour image stays unconverted. Update
  `docs/color_management.md`, the site's `usd/materials.md` and the tests and goldens
  this moves. Verify: tests cover the four "Colour space travels with the texture"
  scenarios, `cargo test --workspace` passes, and each golden that changed is named
  with its cause in the textures design record.

## 3. Reading: the effective space

- [x] 3.1 Remove `crust:mipspace`, `crust:txversion`, `mip_space_matches` and the
  `PredatesAlpha` staleness from the readers and `prepare_tx`. Verify: `cargo build` and
  `cargo clippy --workspace --all-targets -- -D warnings` are clean, with no reference
  left (`grep -rn "crust:mipspace\|txversion" crates` is empty).
- [x] 3.2 Make the effective space E (design decision 2) part of opening a streamed
  texture: T for a `_<C>_<T>` file, the binding's space for `_raw` and for a directly
  bound `.tx`. The u8 table is built for E; a half tile in a curved E is decoded at
  page-in; E → working stays per lookup. Verify: tests cover the three `textures`
  scenarios "An Arnold .tx of an sRGB albedo in a Rec.709 render", "An already-converted
  .tx bound as sRGB" and "… bound with no colour space", each against the values in
  `cases.json`.
- [x] 3.3 Staleness: a `.tx` is stale when it is missing or its modification time differs
  from its source's in whole seconds. Keep the existing complete-UDIM-set rule. Verify:
  tests cover "A source edited after conversion" (used with a warning) and "A .tx
  Arnold wrote" (not rewritten under `--auto-tx`), and a partly converted set still
  preloads.
- [x] 3.4 Make every UV texture lookup return premultiplied colour (design decision 9).
  A straight-alpha source is premultiplied once at preload, in linear light, before the
  pyramid is built; a `.tx` declaring unassociated alpha is premultiplied at page-in;
  associated files are read as stored. Update the "Texture alpha" tests and
  `docs/color_management.md`'s alpha section. Verify: tests cover the three "Texture
  alpha" scenarios on both backends, and a texture without alpha renders bit-identical
  to before.

## 4. Writing: what Arnold's auto-tx writes

- [x] 4.1 Implement the pre-resize stages in OIIO's order: `fixnan box3`, then opaque,
  constant-colour and monochrome detection, then unpremultiply. Verify: unit tests on
  synthetic images (a NaN texel, an opaque alpha, a constant image, a grey image)
  compare against the corresponding Arnold fixtures from 1.1.
- [x] 4.2 Implement the C → T conversion in `f32` with OCIO, re-premultiply, and
  quantise to the output type with OIIO's rounding. Verify: level 0 of crust's output
  and Arnold's `maketx` output for each fixture agree within design decision 7's
  crust-vs-Arnold bound.
- [x] 4.3 Implement OIIO's box-filter mip reduction at round-down sizes, built level from
  level. Verify: on the odd-sized fixture (37x23) every level's size equals Arnold's and
  its texels agree within the decision 7 bound.
- [x] 4.4 Write the files: a half EXR with DWAA or zip, or a TIFF at the source depth with
  zip, as the file-type table says; 64x64 tiles; `roundingmode` 0. Write the metadata
  `oiio:ColorSpace`, `textureformat`, `wrapmodes`, `oiio:AverageColor`, `oiio:SHA-1`
  (in-crate SHA-1, tested against FIPS 180-4 vectors), `autotx`, and `Software` /
  `Exif:ImageHistory` from one function holding the 7.5 flag table, then set the
  modification time to the source's. Verify: `oiiotool --info -v` on crust's file shows
  the same keys and values as Arnold's for every fixture (apart from SHA-1 and
  DateTime), and `maketx -u` reports "no update required".
- [x] 4.5 Route `make_tx` / `make_tx_atomic` (and so `--auto-tx`) through 4.1–4.4, and
  remove `TxFormat` and the `--format` heuristics. Rewrite `examples/maketx.rs` as
  `maketx <image> [colorspace] [--tx-color-space=<T>]`, printing the Arnold name it
  wrote. Verify: the example converts `samples/` textures, and the "An 8-bit sRGB
  source", "A linear or data source" and "A greyscale source" scenarios pass as tests.
- [x] 4.6 Have Arnold render crust's files. The oracle script renders a fixture with
  Arnold's own `.tx`, then with crust's `.tx` swapped in under the same name, and
  compares the two with `crust diff --json`. Verify: relMSE is within decision 7, and
  Arnold logs no regeneration for crust's files.

## 5. Tolerance replaces bit identity

- [x] 5.1 Replace the streamed ↔ preloaded bit-identity tests
  (`a_streamed_tx_matches_the_preloaded_texture_in_acescg`,
  `streamed_alpha_agrees_bit_for_bit_with_preloaded`,
  `a_cutout_with_opaque_tiles_agrees_bit_for_bit_with_preloaded` and the TIFF/EXR
  round-trips that assume crust's layout) with tolerance tests at decision 7's level-0
  bounds, alpha included. Verify: `cargo test -p crust-assets` passes.
- [x] 5.2 Add an image-level check: `samples/materialx_basic` and one UDIM fixture at
  `-s 16 --indirect-clamp 0`, rendered with `CRUST_TEX_STREAM=0` and streamed after
  `--auto-tx`, compared with `crust diff --json`. Verify: relMSE ≤ 1e-3, and the
  measured values are written into the textures design record.
- [ ] 5.3 Run the Arnold-backed tests from groups 1 and 4 on a machine with Arnold, and
  confirm they are skipped cleanly without it (unset `ARNOLD_BIN`, rename the default
  path). Verify: `cargo test --workspace --no-fail-fast` passes in both setups.

## 6. Performance

- [x] 6.1 Measure the cost of half colour tiles and DWAA decode: run `scripts/bench_ab.sh`
  with the binary before this change against after, on a textured scene with
  `--auto-tx` conversions present on both sides (each binary's own convention), and read
  the peak cache bytes and tile page-in time from `--stats` / `--profile`. Verify: the
  min / mean results and peak bytes are recorded in the textures design record,
  together with the effective capacity change of `CRUST_TEX_CACHE_MB` for colour
  textures.

## 7. Documentation of the new convention

- [x] 7.1 Rewrite the textures design record's "Texture residency switches" and
  "Streaming textures" sections. The Arnold convention, the measured tables and the
  effective-space rule replace `crust:mipspace`, `crust:txversion` and the bit-identity
  invariant. Add the 8-bit requantisation banding and SHA-1 reproduction to Known gaps.
  Verify: no section still describes `<stem>.tx` or `crust:mipspace` as current
  (`grep -n "crust:mipspace\|txversion" openspec/specs/textures/design.md` only finds
  history).
- [x] 7.2 Update `docs/color_management.md` (the `.tx` rows of the decode tables and the
  mip-space section), `docs/architecture.md` (remove "streamed ↔ preloaded `u8`
  textures" from Invariants) and the pairs list in `CLAUDE.md`. Verify: the
  bit-identity pair appears in none of the three.
- [x] 7.3 Update the site: `reference/command-line.md` (`--auto-tx` behaviour),
  `usd/materials.md` and the `.tx` / maketx pages (naming, the direct-binding rule, the
  breaking change and how to migrate), and `architecture/` pages that describe the old
  convention. Verify: `zola build` (0.21) in `site/` succeeds with no broken links.

## 8. Integration

- [ ] 8.1 Run the CI set locally: `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast` and `cargo deny
  --locked check`. Verify: all four pass.
- [x] 8.2 End to end on the OpenPBR playground (`D:\samples\OpenPBRShaderPlayground-1.1`):
  crust renders with Arnold's existing `.tx` set, then Arnold renders after crust
  `--auto-tx` converted a fresh copy. Each is compared to the other's own-file render
  with `crust diff`. Verify: both relMSEs are within decision 7, and the result is
  recorded in the textures design record.
- [x] 8.3 Validate the change: `openspec validate tx-arnold-convention --strict`. Verify:
  it reports the change as valid.

## Workflow follow-up

- Archive the change once the PR is reviewed and merged (`/opsx:archive`).
