## 1. Blended tables (`crates/crust-core/src/light_cache.rs`, `light/list.rs`)

- [ ] 1.1 Replace the per-cell lookup with a trilinear blend of the 8
      surrounding cells' tables, with the power table for cells without one
      (design D1). Verify with a unit test: along a line crossing a cell
      boundary, every light's probability changes continuously and the blend
      sums to 1.
- [ ] 1.2 Make `pick_index_at`, `pmf_at`, `find_index_by_geom_at` and
      `infinite_at` read the blend. Verify: `tests/learned_selection.rs`
      (tiled ↔ scanline bit-identity) passes, and a strategy-agreement test
      (light-only, BSDF-only and power-MIS) passes on `samples/usdlux.usda`
      with `learned`.

## 2. Visibility-aware floor

- [ ] 2.1 Record, per cell, which lights were seen in the cell and its 26
      neighbours during the pre-pass. Apply `max(0.3/n, f/n_seen)` to those
      lights (design D2). Verify with a unit test: an unseen light keeps `0.3/n`,
      and a light seen once nearby gets the floor.
- [ ] 2.2 Verify the ALab finding still holds: occluded shadow rays on ALab's
      direct-only render stay within 10% of today's `learned` (33.4%).
- [ ] 2.3 Choose `f` from {0.15, 0.3, 0.5} by full relMSE (4 seeds) on the
      OpenPBR Shader Playground, `domelight` and ALab. Record the result in
      `docs/light_sampling.md` §3.12.

## 3. The gate (design D3)

- [ ] 3.1 Measure equal-time relMSE, `learned` against `power`, full and
      trimmed, 4 seeds, `--indirect-clamp 0`, with `scripts/bench_ab.sh`
      timing, on every checked-in sample, `veach_mis`, the OpenPBR Shader
      Playground and ALab. Record the table in `docs/light_sampling.md`.
- [ ] 3.2 If every scene passes the gate, make `learned` the default in the
      tracer settings, the USD import fallback and the CLI help. Verify: a render
      with no `crust:lightSelection` uses `learned`, and `--light-selection
      power` is bit-identical to the pre-change default (`check_images.sh check`
      against a `main` recording, run with `power`).
- [ ] 3.3 If any scene fails, keep `power` as the default, ship D1 and D2, and
      list the failing scenes and their numbers in the design record.

## 4. Images and documentation

- [ ] 4.1 Re-record the goldens with the new default. Verify that each sample
      moves by noise alone: for three samples, the difference from the `power`
      golden falls as 1/√N across spp.
- [ ] 4.2 Update the `lighting` design record, `docs/light_sampling.md`,
      `site/content/docs/reference/command-line.md` and
      `site/content/docs/usd/render-settings.md` with the default and the
      migration note. Verify: `zola build` in `site/`.
- [ ] 4.3 Run the CI set: fmt, clippy `-D warnings`, `cargo test --workspace`.
