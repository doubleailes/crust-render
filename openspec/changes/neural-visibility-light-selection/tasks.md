# Tasks

## 1. `crust-nn`: the tiny-network kit (design D1, D3, D5)

- [ ] 1.1 Scaffold `crates/crust-nn` and add it to the workspace:
  - `#![forbid(unsafe_code)]`, no dependencies, edition 2024.
  - Verify with `cargo build -p crust-nn`, and with `cargo deny --locked check` passing unchanged.
- [ ] 1.2 Implement `Dense` (forward and backward), ReLU, sigmoid and `bce_with_logits`:
  - plain `f32`, fixed loop order, no `mul_add`;
  - verify that unit tests match hand-computed values for a 2→2 layer.
- [ ] 1.3 Implement `HashGrid` (L levels, F features, T entries, base resolution, growth; trilinear forward, backward that scatters into the table):
  - Verify with unit tests: interpolation is exact at grid corners, and continuous across a cell face.
- [ ] 1.4 Implement `Network` (encoding → dense stack → logits) and `grad_check` (central finite differences):
  - Verify a test asserting relative gradient error < 1e-3 on random inputs, for every parameter group.
- [ ] 1.5 Implement `Adam`, with per-group learning rates:
  - Verify that a test fits a 1D step function (a "wall") to BCE < 0.05 in a fixed number of steps.
- [ ] 1.6 Make initialisation take its random numbers from a caller-supplied `FnMut() -> f32`, so crust-core can feed it `openqmc`:
  - Verify that a test shows the same draws give bitwise-identical networks.
- [ ] 1.7 Add deterministic batched gradients (fixed chunks, summed in chunk order):
  - Verify that a test gives bitwise-equal gradients at 1 and at 8 rayon threads.
  - Run `scripts/test_simd_matrix.sh -p crust-nn` and confirm it passes (extend the script to accept the crate if needed).
- [ ] 1.8 Document the eighth crate:
  - its row in `docs/architecture.md`'s crate table;
  - CLAUDE.md § Workspace ("Eight crates", plus its `forbid(unsafe_code)` line);
  - `site/content/docs/architecture/overview.md`.
  - Verify with `zola build` in `site/` succeeding.

## 2. Labels and training in crust-core (design D2, D4, D5)

- [ ] 2.1 Split the receiver walk out of `light_cache::train`, so it reports per receiver per light both the contribution estimate and the "delivered light" fraction:
  - Verify that `cargo test -p crust-core --test learned_selection` passes.
  - Verify that `scripts/check_images.sh check` against goldens recorded before the split shows 0 differing pixels under `--light-selection learned`.
- [ ] 2.2 Add `crates/crust-core/src/light_visibility.rs`. It holds:
  - the bounds (reuse `learned`'s quantile bounds);
  - the network built per D3;
  - training per D5, under a new `K_NVC` openqmc key;
  - the `p ↦ (pmf, cdf)` function per D2, sharing `light_cache::DEFENSIVE`;
  - a `DEBUG` line with receivers, lights, epochs, final loss and time.
  - Verify that a unit test on a synthetic two-room scene (one light per room, wall between) predicts V̂ < 0.1 for the far room's light and > 0.9 for the near one's.
- [ ] 2.3 Pin training determinism:
  - Verify that a test trains twice, at 1 and at the default rayon thread count, and asserts the weights are bitwise equal.
- [ ] 2.4 Pin the D2 formula's edge cases:
  - Verify unit tests asserting every emitting light gets ≥ `D / n_live`;
  - that pmfs sum to 1 within 1e-6;
  - that `cdf` brackets `pmf`, as `light_cache`'s `each_pmf_is_exactly_its_cdf_interval` does;
  - and that the power table is returned when all V̂ are 0.

## 3. Go/no-go gate on inference cost (design D6 prerequisite, D9)

- [ ] 3.1 Assert that NEE's sampling position and the bounce side's `prev.pos` are the same `Vec3A` bits, at every NEE site (surface and volume / phase), behind `debug_assert!`:
  - Verify with `cargo test --workspace` (debug) passing over every sample.
  - If any site differs, fix it so both sides store one value, and confirm `power` / `learned` stay bit-identical with `check_images.sh`.
- [ ] 3.2 Add `crates/crust-render/examples/nvc_bench.rs`. It trains on a scene and times one `(pmf, cdf)` evaluation, one shadow ray, and one `learned` lookup over the same receivers, printing min-of-N wall time:
  - Verify it runs on `samples/usdlux.usda` and on ALab.
- [ ] 3.3 Count the same three costs with callgrind, using the CLAUDE.md recipe, on `usdlux` and ALab.
  - Record wall time and instructions in a new `docs/light_sampling.md` § "Neural visibility cache".
  - Write the go/no-go decision there per D9.
  - If no-go: shrink D3 once and re-measure. If it is still no-go, stop here, keep the recorded result, and update proposal.md and design.md to say so.
- [ ] 3.4 Add the `nvc_bench` recipe to the command cookbook in `openspec/specs/cli/design.md`:
  - Verify that the documented command runs as written.

## 4. The `neural` selection mode (spec: `lighting`; design D2, D6, D7)

- [ ] 4.1 Add `LightSelection::Neural` and its `named!` row, with a one-line description:
  - Verify that `names.rs`'s round-trip test passes, and that `crust render --help` lists `neural`.
- [ ] 4.2 Route `pick_index_at` / `pmf_at` / `find_index_by_geom_at` / `infinite_at` through the network when it is installed, with the D6 single-entry thread-local memo:
  - Verify a unit test that NEE's `pmf` for a light equals, bitwise, what `find_index_by_geom_at` and `infinite_at` return at the same `p`, with the memo both hit and cold.
- [ ] 4.3 Train in `Renderer::new` beside `learned`, installing over a power table, with the fallbacks from the spec:
  - < 2 lights or no receivers → silent;
  - > 64 lights → one `WARN`.
  - Verify the spec scenarios "Too many lights" and "One light" as integration tests (bit-identical to `power`; `WARN` count).
- [ ] 4.4 Accept `crust:lightSelection = "neural"` in `usd_import/settings.rs`, and update its comment:
  - Verify with a test that loads a `.usda` authoring it and asserts the selection.
- [ ] 4.5 Pin the spec's determinism scenarios:
  - Verify an integration test rendering a sample under `neural` tiled and scanline, and at 1 and N threads, asserting bit-identity (extend `tests/learned_selection.rs`, or add `tests/neural_selection.rs`).
- [ ] 4.6 Pin "Existing modes are unchanged":
  - Verify `scripts/check_images.sh check` against goldens recorded on the base commit shows 0 differing pixels under `power`, `uniform` and `learned`.
- [ ] 4.7 Update the `lighting` design record (`openspec/specs/lighting/design.md`, the light-selection section) with `neural`'s load-bearing details: D2, D6, D7.
- [ ] 4.8 Update user docs:
  - `site/content/docs/reference/command-line.md` (`--light-selection neural`);
  - `site/content/docs/usd/render-settings.md` (`crust:lightSelection`);
  - `site/content/docs/help/faq.md`, where it lists the selection modes.
  - Verify with `zola build`.

## 5. Unbiasedness and MIS checks (spec: `lighting`)

- [ ] 5.1 "Converges to the power reference": render `usdlux` under `neural` at 16, 64 and 256 spp with `--indirect-clamp 0` against a 1024 spp `power` reference, using `crust diff`:
  - Verify relMSE falls as roughly 1/N (record the three values in `docs/light_sampling.md`).
- [ ] 5.2 "Emission is not double-counted": render `veach_mis` at 1024 spp under `neural` and `power`, `--indirect-clamp 0`:
  - Verify the image means agree within the noise (report both, and the per-seed spread over `-f 1..4`).

## 6. The equal-time verdict (design Goals, D9)

- [ ] 6.1 relMSE at 16 spp (full and trimmed, `--indirect-clamp 0`, against 1024 spp `power` references) for ALab direct lighting, ALab full, `usdlux` (4 seeds), `veach_mis` (4), `domelight` (4) and `materialx_basic`, under `power` / `learned` / `neural`:
  - Verify that the table is in `docs/light_sampling.md`, in the same shape as §3.12's.
- [ ] 6.2 `scripts/bench_ab.sh` `learned` against `neural` on ALab and `usdlux` at 128 spp; report min and mean:
  - Verify that the equal-time gains are computed and recorded beside 6.1, with the verdict: keep opt-in, propose D2's contribution variant, or retire.
- [ ] 6.3 Measure whether `D` can fall below 0.3 under `neural` (0.2, 0.1) on `domelight` and ALab:
  - Verify that the result and the chosen value are recorded beside the `DEFENSIVE` constant's comment.

## 7. Integration checks

- [ ] 7.1 Run the four CI jobs locally and confirm each is clean:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --no-fail-fast`
  - `cargo deny --locked check`
- [ ] 7.2 Run the pinned nightly leg (`cargo +nightly-2026-09-26 clippy --workspace --all-targets -- -D warnings` and its tests) and confirm it is clean.

## Workflow follow-up

- Sync the `lighting` delta into `openspec/specs/lighting/spec.md`, and archive the change, once the verdict in 6.2 is recorded and reviewed.
