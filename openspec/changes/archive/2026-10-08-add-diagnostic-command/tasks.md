# Tasks

Prerequisites: `add-render-region` and `add-lpe-variance` are merged.

## 1. Engine seams (design D2, D6, D8)

- [x] 1.1 Add `Renderer::reconfigure(&mut self, settings)` (light
      selection, including the `learned` pre-pass, and every per-settings
      field), and reimplement `Renderer::new` as construct + `reconfigure`.
      Verify with a bitwise test for each varied setting (strategy, light
      selection ×3, light samples, guiding, max_depth): `new(s)` ==
      `new(s0).reconfigure(s)` on the Cornell box at 16 spp.
- [x] 1.2 Add a per-work-unit timer, enabled by a field on the render call
      (not `Config`, not an env var), returning a per-tile seconds map.
      Verify: off by default; the callgrind zero-AOV instruction count on
      the Cornell box at `-s 2` stays within noise of the parent.
- [x] 1.3 Add the clamp-measurement counter: given `Some(limit)` while the
      clamp is off, sum per sample the luminance the clamp would remove,
      and count the pixels touched. Verify:
      - images are bitwise unchanged with the counter on;
      - the measured energy equals the luminance difference between
        clamped and unclamped renders at 16 spp, within f32 tolerance;
      - callgrind is unchanged with the counter off.
- [x] 1.4 Report setup and render time separately from a render call: the
      `learned` pre-pass and guiding training versus sampling.

## 2. Module skeleton and report types (design D1, D11)

- [x] 2.1 Create `crust-core/src/diagnostic/`:
      - `mod.rs` (`run`);
      - `report.rs` (`Report` and children, `Serialize`);
      - `markdown.rs`;
      - `checks.rs`, `crops.rs`, `schedule.rs`, `trials.rs`.

      Add `serde` and `serde_json` as direct dependencies of `crust-core`;
      `cargo deny --locked check` must pass.
- [x] 2.2 Add a test pinning the JSON key order and `format` value against
      a fixture `Report`. Add a Markdown snapshot test of the same fixture
      (verdict block first, sections in JSON order).

## 3. Baseline (design D3, D4, D7)

- [x] 3.1 Implement the calibration pass and `spp_P1`, and the probe
      settings (clamp off, adaptive off, fixed spp).
- [x] 3.2 Run P1 with the engine-built LPE value + variance request for
      the D7 partition, the per-tile timer and the clamp counter. Collect
      MRSE, per-tile relative variance, `RayStats`, profile sections and
      cache stats. Verify that two P1 runs give bitwise-identical images
      (the determinism D5 relies on); if not, revise D5 to average MRSE.
- [x] 3.3 Implement per-light labelling for the light-group breakdown when
      no `lpeTag` is authored and there are at most 8 lights. Verify the
      beauty is bitwise unchanged with labels.

- [x] 3.4 Check every D7 expression against the LPE parser (`<R[GS]>`
      included). Replace any refused form with an equivalent union, and
      test that the partition sums to the beauty on the Cornell box.
## 4. Crops (design D6)

- [x] 4.1 Implement the crop picker: crop side from the thread count,
      summed-area scoring, criteria A/B/C, background exclusion, IoU > 0.5
      replacement. Use `--region` as the sole crop. Verify with unit tests
      on synthetic variance/time maps (a hot corner, a uniform map, a map
      where A and B coincide).

## 5. Trials (design D5, D8, D10)

- [x] 5.1 Implement the scheduler:
      - cost estimate from P1;
      - tier shares 70/15/15 with roll-forward;
      - never start an overrunning trial;
      - `not_tried` with reasons.

      Verify with unit tests on a fake clock and cost model.
- [x] 5.2 Implement the tier-1 one-factor trials with interleaved repeats,
      ordered by the D7 rules table. Build the per-crop reference by
      inverse-variance blend, then compute per-pair ΔEff, per-crop
      verdicts, the geometric-mean overall and `mixed`. Verify the verdict
      logic with unit tests on synthetic pair values, including every
      scenario in the spec.
- [x] 5.3 Implement the combined trial of winners, and pick the suggestion
      between combined and best single.
- [x] 5.4 Implement tier 2: `spp_to_target` from the threshold or
      `--target-mrse`, the full-frame projection (render time only), and
      the adaptive trial per crop.
- [x] 5.5 Implement tier 3: the clamp counter results, the
      `ended_depth` share plus the half-depth crop trial, and the
      subdivision stats.

## 6. Static checks (design D9)

- [x] 6.1 Implement the first check set, each with a unit test on a small
      `.usda` (or fixture stats):
      - non-`.tx` textures;
      - cache hit rate;
      - unlit emitters;
      - many lights with uniform selection;
      - guiding without indirect-dominant rows;
      - peak memory.

      Each action must name an existing flag and attribute, or be `none`
      with a reason.

## 7. Baseline comparison and convergence (design D11, D12)

- [x] 7.1 Implement `converged`, `suggestions` (flag + attribute + expected
      ΔEff + evidence ids) and `suggested_command`.
- [x] 7.2 Implement `--baseline`: the comparability check, then deltas for
      time, MRSE, settings, findings and suggestions. Verify with tests on
      fixture reports: comparable, different camera, different format
      version.

## 8. CLI (design D13, D14)

- [x] 8.1 Move the scene-shaping render flags into a flattened
      `SceneArgs` used by `render` and `diagnostic`. Verify that the
      existing CLI tests pass unchanged and that `crust render --help`
      lists the same flags.
- [x] 8.2 Add the `Diagnostic` subcommand with `--budget`, `--json`,
      `--baseline`, `--repeats` and `--target-mrse`. Markdown goes to
      stdout and JSON to the file. Map exit status 0/3/1/2. Log one INFO
      line per phase; per-trial lines are DEBUG.
- [x] 8.3 Add end-to-end tests on `samples/cornellbox.usda` with a small
      budget:
      - stdout is Markdown only;
      - the JSON parses, with `format = crust-diagnostic/1`;
      - no image is written;
      - the stage is unchanged;
      - `-s` is refused;
      - a missing input exits 1;
      - a 1 s budget exits 3 with `not_tried` populated.

## 9. Documentation

- [x] 9.1 Update `site/content/docs/reference/command-line.md` with a
      `diagnostic` section: every flag, exit statuses, outputs.
- [x] 9.2 Add a new user page: reading the report, the verdicts and their
      thresholds, the tiers, and a worked agent loop (diagnose → apply →
      `--baseline` → converged).
- [x] 9.3 Add a new `openspec/specs/diagnostics/design.md` record from
      this design. Add a "diagnose a scene" recipe to the `cli` cookbook.
      Add the module to `docs/architecture.md`'s crate table.
- [x] 9.4 Run `zola build` in `site/` (Zola 0.21). Run the CI set locally:
      fmt, clippy `-D warnings`, `cargo test --workspace`, and
      `cargo deny --locked check`.
