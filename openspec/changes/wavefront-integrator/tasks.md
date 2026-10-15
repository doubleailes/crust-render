# Tasks

## 1. Phase 0: where a path's time goes

- [ ] 1.1 Run `--profile` and `--stats` on the default `scripts/bench_scenes.sh` scenes
  and on `materialx_basic`, at the bench settings. Verify: each scene has a row with
  the `Trace`, `Occlusion`, `EvalBsdfs`, `RunShader`, `Texture`, `TextureLoad` and
  `SurfaceLighting` shares.
- [ ] 1.2 Run callgrind (2 spp, `RAYON_NUM_THREADS=1`) on `cornellbox`, `materialx_basic`
  and `openpbr_showcase`, and record the inclusive instruction counts of `advance_pixel`
  and the interpreter-dispatch share (`Program::apply_op`). Verify: these are also the
  Phase 1 baselines (D3), saved beside the table.
- [ ] 1.3 Where the assets are present, add ALab frame 1004 and the Moana island to the
  table, using the recipes in `docs/alab_profile.md` and `docs/moana_profile.md`. Verify:
  the rows exist, or the record says the assets were unavailable.
- [ ] 1.4 Write "Where a path's time goes" into `openspec/specs/rendering/design.md`: the
  table, plus a ranking of the follow-up exploits (shading dispatch, texture coherence,
  ray coherence, batched inference) read from it. Verify: the section exists and cites the
  command behind every number.

## 2. Phase 1: a resumable path

- [ ] 2.1 Introduce `PathState<'w>` in `tracer/path.rs`, holding the loop's carried locals
  and per-path buffers (D2). Keep the per-vertex buffers (`thin`, `nee_split`,
  `bounce_split`) on the worker scratch. Verify: `cargo build` and `cargo clippy
  --workspace --all-targets -- -D warnings` are clean.
- [ ] 2.2 Split `trace_path` into `begin`, `step` (yields `Extend(ray)` at the extension
  ray and at the depth-exhausted query; carries on through walk exits) and `finish`
  (the backward gather and training samples). Keep all five const generics and
  `#[inline(always)]`. Verify: `cargo test --workspace` passes.
- [ ] 2.3 Rebuild `trace_path` and the `ray_color*` entry points as a one-path driver over
  `begin`/`step`/`finish`, with unchanged signatures. Verify: `scripts/check_images.sh
  check` against goldens recorded on the parent commit reports every image identical.
- [ ] 2.4 Measure Phase 1 against the 1.2 baselines. Verify: `advance_pixel` inclusive
  instructions are ≤ +0.3% on all three scenes. If not, apply D3's fallback (hot locals
  as arguments) and re-measure before proceeding.
- [ ] 2.5 Update the `rendering` design record ("Render pipeline") to describe the path
  as state + step + gather, and the doc comments on `trace_path`/`PathScratch` that
  describe the old loop. Verify: no doc comment in `tracer/` still describes
  `trace_path` as a single loop.

## 3. Phase 2: the switch

- [ ] 3.1 Add `wavefront: bool` to `crust_core::Config`, parsed once from
  `CRUST_WAVEFRONT` (`0`|`1`). A bad value warns once and uses `0`. Verify: a unit test
  in `config.rs` covers unset, `0`, `1` and `fast` (one warning, `false`).
- [ ] 3.2 Add the row to `docs/architecture.md` § Environment switches (off side: one
  path at a time) and add "batched ↔ one path at a time" to its Invariants list. Add a
  `CRUST_WAVEFRONT` section and table row to
  `site/content/docs/reference/environment-variables.md`. Verify: `zola build` in
  `site/` (Zola 0.21) passes its link and anchor checks.

## 4. Phase 2: the batched sweep

- [ ] 4.1 Add `tracer/wavefront.rs`: a batch of slots (per-slot `PathState`, extension
  ray, hit, filter weight, time, sample index), batch formation from whole pixels of
  one unit's stage in pixel order up to the cap `K` (D4), and the round loop (D5). The cap is a
  test-only override on `Instruments`, beside `unstaged`.
  Verify: unit tests in the module check batch formation for `K` = 1, 7, 1024 against
  hand-computed slot lists, including a pixel larger than `K`.
- [ ] 4.2 Implement the slot-order fold (D6): per pixel, add radiance, filter weight,
  luminance moments, AOV planes, inspection records and the measured clamp in
  increasing sample index, and push training samples in slot order. Verify: a test
  renders `cornellbox` at `K` = 7 batched and one at a time, and compares the image,
  `RayStats` and per-pixel `taken` bitwise.
- [ ] 4.3 Route `advance_pixel`'s caller through the batched sweep when
  `Config::wavefront` is set, for every const-generic instantiation, under tiles and
  scanlines. Verify: `CRUST_WAVEFRONT=1 cargo run --release -- render -i
  samples/cornellbox.usda -o /tmp/a.exr -s 16` and the same with `=0` give
  `crust diff` exit 0.
- [ ] 4.4 Add the bitwise pin test `a_batched_sweep_renders_one_path_at_a_time_bit_for_bit`
  in `tracer/tests.rs`, shaped like the staging pin: every sample scene (small
  resolution, 16 spp), batched × one-at-a-time, tiles × scanlines, staged × unstaged,
  `K` ∈ {1, 7, default}. Verify: the test passes and covers AOVs (`aovs_lpe.usda`),
  media (`fog`, `smoke`), cutouts and thin walls (`materialx_cutout`, `thin_window`),
  subsurface, hair and light linking.
- [ ] 4.5 Add the guided pin (`cornellbox_guided.usda`: every pass's training samples, the
  blend, the image), the adaptive pin (`-s 64`, non-zero threshold: equal per-pixel
  sample counts) and the inspection pin (`crust light-paths` records for one pixel equal
  under both). Verify: all three tests pass.
- [ ] 4.6 Add the cancellation test: cancel a batched render mid-sweep, and check that it
  returns, that no pixel is NaN, and that the counters cover exactly the folded
  samples. Verify: the test passes under tiles and scanlines.
- [ ] 4.7 Run `scripts/check_images.sh check` with `CRUST_WAVEFRONT=1` against goldens
  recorded without it. Verify: every image is identical. Then run
  `scripts/test_simd_matrix.sh -p crust-core`; verify it passes.

## 5. Phase 2: the gate

- [ ] 5.1 Run callgrind (2 spp, one thread) with the switch on and off on `cornellbox`,
  `materialx_basic` and `openpbr_showcase`. Verify: the instruction delta is recorded
  per scene against D7's ≤ 3%.
- [ ] 5.2 Run `scripts/bench_ab.sh` with the same binary under `CRUST_WAVEFRONT=0` and `=1`
  on the default bench scenes (plus ALab and Moana where present), for
  `K` ∈ {256, 1024, 4096}, and record peak RSS from `--stats`. Verify: min and mean
  per scene and per `K` are recorded against D7's ≤ 5%, and `K` is set to the best
  value.
- [ ] 5.3 Write "Batched sweep" into `openspec/specs/rendering/design.md`: the design
  (D1–D6), the gate's numbers, pass or fail, and "Known gaps" (only the extension ray
  is batched; no refill; no exploit yet). Update the packet-tracing gap in
  `openspec/specs/intersection-kernel/design.md` to point at it. Verify: both records
  read consistently with the measured result.

## 6. Integration

- [ ] 6.1 Run the CI set locally: `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`, `cargo deny
  --locked check`, and the pinned nightly clippy. Verify: all are clean.
- [ ] 6.2 Validate the change: `openspec validate wavefront-integrator --strict`. Verify:
  it reports no errors.

## Workflow follow-up

- Archive the change once reviewed, syncing the `rendering` and `cli` deltas.
- Open the first exploit change, chosen by Phase 0's ranking (likely
  material-sorted batched shading, or texture-tile sorting for ALab and Moana).
