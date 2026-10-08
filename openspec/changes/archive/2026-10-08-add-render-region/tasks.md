# Tasks

## 1. The region in the settings (design D1)

- [x] 1.1 Add `PixelRect { x0, y0, x1, y1 }`, half-open:
      - methods `width`, `height`, `contains`, `index(x, y)`, and
        `clip_to(w, h) -> Option<PixelRect>`;
      - `RenderSettings::region` (default the full frame) with
        `with_region`, which clips and refuses an empty result;
      - `with_resolution` resets the region to the new frame.

      Verify with unit tests: clipping, an empty result, `index` of a region
      offset from the origin.

## 2. Import `dataWindowNDC` (design D2)

- [x] 2.1 Convert NDC to pixels by pixel centres, with NDC y bottom-up.
      Resolve product > settings > full frame. Clip overscan with a warning;
      refuse an empty window with a warning. Remove `dataWindowNDC` from
      `warn_unhonoured`.

      Verify with `.usda` unit tests for every scenario in
      `specs/usd-scene-import/spec.md`, including the exact-boundary and
      1-pixel windows.

## 3. Render the region (design D4, D5, D7)

- [x] 3.1 Make `generate_tiles` / `generate_rows` take the region:
      - tiles keep frame-aligned 16-pixel boundaries, clipped to the region;
      - rows span the region's width.

      Verify with unit tests: a full-frame region yields today's tile list
      exactly.
- [x] 3.2 Size `Buffer`, `AovFilm`, the sample-count and variance planes, and
      `PassStats::var_map` to the region, indexing through
      `PixelRect::index`. The adaptive neighbour hold skips out-of-region
      neighbours. The guided schedule runs over the region.
- [x] 3.3 Add a bitwise test: Cornell box at 16 spp, full frame vs
      `--region 37,21,101,77`, comparing beauty, an LPE AOV, depth and
      `sampleCount` per pixel. Add a tiles-vs-scanlines region test.
- [x] 3.4 Run `scripts/check_images.sh check` against goldens recorded
      before the change. Expect no change. Confirm the zero-AOV instruction
      count with callgrind on the Cornell box at `-s 2`: within noise of the
      parent commit.

      Result: all 37 sample scenes `identical` against goldens recorded with
      the parent commit (Kitchen_set is not in this checkout). Callgrind,
      cornellbox `-s 2`, one thread: 4 237 271 763 → 4 244 156 136
      instructions (+0.16%). The integrator is unchanged to the instruction
      (`advance_pixel`, `scatter_resolved`, `eval_all` equal); the difference
      is the per-pixel gather and output indexing through `PixelRect`
      (`render_pass` +2.9 M, the EXR closure +3.0 M, `write_png` +0.9 M,
      about 30 instructions a pixel), so it does not grow with spp.

## 4. Output (design D6)

- [x] 4.1 Confirm the `exr` crate's API for a data window inside a display
      window (layer position + display window), with a 10-line round-trip
      test before the writer change. If it cannot express it, stop and
      revise D6.
- [x] 4.2 Write the beauty EXR and every product with display window =
      frame and data window = region. Write the PNG at the region's size.
      Verify: an EXR round-trip test reads both windows back. A no-region
      render's EXR and PNG are byte-identical to the parent commit's.

      Result: the PNGs and every product EXR (`aovs.usda`, `aovs_lpe.usda`)
      are byte-identical to the parent's. The no-products beauty EXR has a
      byte-identical header and identical pixels, but its bytes differ:
      `write_rgb_file`'s default encoding writes compressed blocks in
      completion order, so the parent's own file differs between two runs
      (three runs, three checksums). Byte identity is not attainable there.

## 5. CLI (design D3)

- [x] 5.1 Add `--region X0,Y0,X1,Y1` with a clap value parser: four
      integers, `X1 > X0`, `Y1 > Y0`. Clip after import, with an error
      naming the resolution when the result is empty. Make `--region` take
      precedence over the authored window. Add the region and its frame
      share to `--stats`.

      Verify with CLI tests for each scenario in `specs/cli/spec.md`.

## 6. Documentation

- [x] 6.1 Update the user documentation:
      - `site/content/docs/reference/command-line.md`: a `### region`
        section under "Input and output";
      - `site/content/docs/usd/aovs.md`: `dataWindowNDC` leaves the "not
        honoured" list; describe how a window resolves;
      - the `cli` design record's flag list and cookbook (crop a frame);
      - the `rendering` design record (region bit-identity and its
        neighbour-hold and guiding exceptions).

      Verify with `zola build` in `site/` (Zola 0.21).
- [x] 6.2 Run the CI set locally: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace`.
