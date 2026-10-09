# Tasks

## 1. Baseline

- [ ] 1.1 On `main`, build release and run `scripts/check_images.sh record <dir>`. Save a callgrind instruction count for `samples/cornellbox.usda -s 2` (`RAYON_NUM_THREADS=1`). Verify the golden directory and the callgrind total exist for the later comparisons.

## 2. Staged first sweep (D1)

- [ ] 2.1 In `render_pass`, replace the single first sweep with the stage list `[1, 2, 4, …, sweep_to]`, one `par_iter_mut` per stage. Call `finish_round` only after the last stage. Leave training passes (a training `GuidingContext`) unstaged. Add an internal `PassConfig::staged` (default true) for tests. Verify `cargo test -p crust-core` passes.
- [ ] 2.2 Count progress as units × stages + rounds and update any test that pins the total. Verify the existing progress-order test (one report at a time, +1 each) passes under tiles and scanlines.
- [ ] 2.3 Add a bitwise test in `tracer/tests.rs`: staged vs. unstaged × tiles vs. scanlines, on an adaptive render, a non-adaptive one, one with AOVs (film compared plane by plane), and a guided one. Verify it passes.
- [ ] 2.4 Run `scripts/check_images.sh check <dir>` against the 1.1 goldens and verify there are zero differences. Run callgrind as in 1.1 and verify the instruction count is within noise. Run `scripts/bench_ab.sh` against the `main` binary on cornellbox and one texture-heavy sample, and record min and mean in the `rendering` design record (§ Adaptive sampling, a "staged first sweep" paragraph).

## 3. Render control, snapshots, cancellation (D2–D5)

- [ ] 3.1 Add `RenderControl` and `RenderOutcome` in `tracer/` and re-export them from `crust_core`. Doc comments state the contract (per render; cancel is sticky; snapshot is `None` before the first publish; generation is monotonic). Add a row to the Seams table in `docs/architecture.md`. Verify `cargo doc -p crust-core` builds without warnings.
- [ ] 3.2 Make `PixelState::estimate` return zero at `taken == 0`, and have `AovFilm::store` leave clear values for such pixels. Add a unit test for a zero-sample pixel (no NaN). Verify the 2.3 test and the goldens are still identical.
- [ ] 3.3 Add the new `Renderer` entry point taking `&RenderControl` (tiled, progress, optional AOV request → buffer, film, `RayStats`, outcome). Route the existing `render*` methods through it with no control. Verify the existing tracer tests pass unchanged.
- [ ] 3.4 Publish per unit after each stage and round into the control's display buffer, and bump the generation. Add tests: the snapshot after an unguided completed render equals the returned image bitwise, and generations strictly increase across reads taken from a second thread. Verify both pass.
- [ ] 3.5 Check the cancel flag before each pixel advance, and stop scheduling stages and rounds once it is set. Do not walk progress to the total on cancel. Add tests:
  - cancelled before the call → `Cancelled`, zero rays, an all-zero image;
  - cancelled from a second thread mid-render → returns promptly, no NaN, `RayStats` equals the sum the units traced.

  Verify they pass.
- [ ] 3.6 Verify the zero-control path is unchanged by re-running 2.4's callgrind and `check_images.sh check`.

## 4. Guided renders (D6)

- [ ] 4.1 Thread the control through `render_guided`: every pass publishes, and on cancel scheduling stops. Blend the completed passes, plus the interrupted one when every pixel has `taken ≥ 2`, or return the interrupted pass alone when none completed. Do the same for the AOV blend. Add tests for cancelling during the first training pass and during the final pass after its 4 spp stage. Verify both pass with no NaN.
- [ ] 4.2 In `openspec/specs/rendering/design.md`, add a "Progressive output and cancellation" section (D1–D6 in brief) and Known gaps entries: progressive AOVs; the guided preview getting noisier at the final pass; import, `Renderer::new` and the `learned` pre-pass cannot be cancelled. Verify the section links resolve.

## 5. CLI (D7, D8)

- [ ] 5.1 Add `ctrlc` to `crust-render`. Run `cargo deny --locked check` and verify it passes.
- [ ] 5.2 Install the SIGINT state machine (loading → exit 130; rendering → cancel; writing → exit 130). Render through the control. On `Cancelled`, write all outputs, log `WARN` with the min and max samples reached, and return exit code 130. Add an integration test in `crust-render/tests/` that sends SIGINT to a long render of a sample scene. Verify the outputs exist, have no NaN, and the status is 130.
- [ ] 5.3 Add `--checkpoint <SECONDS>` (positive; 0 or negative is a usage error). Use a scoped thread that rewrites the final PNG path from the snapshot when the generation moved, and warns once when the first product has no beauty. Add tests: `--checkpoint 0` is refused; a run with the flag ends with byte-identical EXR and PNG to a run without it.
- [ ] 5.4 Write `crust:renderStatus = "interrupted"` in the single-beauty EXR writer and `products::write_product` only on cancellation. Extend the 5.2 test to read the attribute back. Verify `check_images.sh check` stays identical for completed renders.
- [ ] 5.5 Document the CLI changes:
  - `site/content/docs/reference/command-line.md`: `--checkpoint`, Ctrl-C behaviour, exit 130.
  - `site/content/docs/usd/aovs.md` and `site/content/docs/reference/command-line.md`, where the EXR headers are described: the `crust:renderStatus` attribute.
  - `openspec/specs/cli/design.md`: logging and the command cookbook entry.
  - `openspec/specs/aovs/design.md`: the non-goal becomes "progressive AOV snapshots and display drivers".

  Run `zola build` in `site/` (Zola 0.21) and verify it succeeds with no broken links.

## 6. Integration

- [ ] 6.1 Run the full CI set locally and verify each passes:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --no-fail-fast`
  - `cargo deny --locked check`
  - the pinned-nightly clippy and test commands from CLAUDE.md
- [ ] 6.2 Run `crust render -i samples/cornellbox.usda -s 4096 --checkpoint 2`. Verify by hand that the PNG refreshes and Ctrl-C leaves a usable, flagged EXR. Note the result in the PR description.

## Workflow follow-up

- Sync the delta specs and archive the change once merged.
- A follow-up change for the diagnostic's time budget, which can use `RenderControl`.
- A follow-up change for Hydra groundwork: restart on camera change, double-buffered display, the C ABI crate.
