# Tasks

## 1. The region in the settings (design D1)

- [ ] 1.1 Add `PixelRect { x0, y0, x1, y1 }`, half-open:
      - methods `width`, `height`, `contains`, `index(x, y)`, and
        `clip_to(w, h) -> Option<PixelRect>`;
      - `RenderSettings::region` (default the full frame) with
        `with_region`, which clips and refuses an empty result;
      - `with_resolution` resets the region to the new frame.

      Verify with unit tests: clipping, an empty result, `index` of a region
      offset from the origin.

## 2. Import `dataWindowNDC` (design D2)

- [ ] 2.1 Convert NDC to pixels by pixel centres, with NDC y bottom-up.
      Resolve product > settings > full frame. Clip overscan with a warning;
      refuse an empty window with a warning. Remove `dataWindowNDC` from
      `warn_unhonoured`.

      Verify with `.usda` unit tests for every scenario in
      `specs/usd-scene-import/spec.md`, including the exact-boundary and
      1-pixel windows.

## 3. Render the region (design D4, D5, D7)

- [ ] 3.1 Make `generate_tiles` / `generate_rows` take the region:
      - tiles keep frame-aligned 16-pixel boundaries, clipped to the region;
      - rows span the region's width.

      Verify with unit tests: a full-frame region yields today's tile list
      exactly.
- [ ] 3.2 Size `Buffer`, `AovFilm`, the sample-count and variance planes, and
      `PassStats::var_map` to the region, indexing through
      `PixelRect::index`. The adaptive neighbour hold skips out-of-region
      neighbours. The guided schedule runs over the region.
- [ ] 3.3 Add a bitwise test: Cornell box at 16 spp, full frame vs
      `--region 37,21,101,77`, comparing beauty, an LPE AOV, depth and
      `sampleCount` per pixel. Add a tiles-vs-scanlines region test.
- [ ] 3.4 Run `scripts/check_images.sh check` against goldens recorded
      before the change. Expect no change. Confirm the zero-AOV instruction
      count with callgrind on the Cornell box at `-s 2`: within noise of the
      parent commit.

## 4. Output (design D6)

- [ ] 4.1 Confirm the `exr` crate's API for a data window inside a display
      window (layer position + display window), with a 10-line round-trip
      test before the writer change. If it cannot express it, stop and
      revise D6.
- [ ] 4.2 Write the beauty EXR and every product with display window =
      frame and data window = region. Write the PNG at the region's size.
      Verify: an EXR round-trip test reads both windows back. A no-region
      render's EXR and PNG are byte-identical to the parent commit's.

## 5. CLI (design D3)

- [ ] 5.1 Add `--region X0,Y0,X1,Y1` with a clap value parser: four
      integers, `X1 > X0`, `Y1 > Y0`. Clip after import, with an error
      naming the resolution when the result is empty. Make `--region` take
      precedence over the authored window. Add the region and its frame
      share to `--stats`.

      Verify with CLI tests for each scenario in `specs/cli/spec.md`.

## 6. Documentation

- [ ] 6.1 Update the user documentation:
      - `site/content/docs/reference/command-line.md`: a `### region`
        section under "Input and output";
      - `site/content/docs/usd/aovs.md`: `dataWindowNDC` leaves the "not
        honoured" list; describe how a window resolves;
      - the `cli` design record's flag list and cookbook (crop a frame);
      - the `rendering` design record (region bit-identity and its
        neighbour-hold and guiding exceptions).

      Verify with `zola build` in `site/` (Zola 0.21).
- [ ] 6.2 Run the CI set locally: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace`.
