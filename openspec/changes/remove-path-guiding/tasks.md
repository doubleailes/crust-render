## 1. Pin the unguided render

- [ ] 1.1 Build the release binary and record goldens of the unguided samples at 16 spp
      with `--indirect-clamp 0` into a directory outside the repo
      (`scripts/check_images.sh record <dir>`). Done when `check <dir>` passes on the
      untouched tree; every later group re-runs it.
- [ ] 1.2 Add a test that renders `samples/cornellbox.usda` at 16 spp with `crust:pathGuiding`
      authored `false` and without it, and asserts equal bits. Done when it passes now and,
      after group 2, still passes with the attribute turned into a warning.

## 2. Settings and warning

- [ ] 2.1 Add `SettingsPathGuidingRemoved` (`settings.path_guiding_removed`, `Refused`,
      `Once`) to `warnings.rs`, with its row in `site/content/docs/reference/warnings.md`.
      Done when the codes-in-step test passes.
- [ ] 2.2 In `usd_import/settings.rs`, stop reading `crust:pathGuiding`,
      `crust:guidingTrainIterations` and `crust:guidingProb`; raise the warning once when
      any is authored, whatever the value, naming the attributes found. Done when tests
      cover the three scenarios of the `usd-scene-import` delta (`true`; only
      `guidingProb`; none authored) and the 1.2 test passes.
- [ ] 2.3 Drop the guiding fields and accessors from `RenderSettings` and the
      `with_guiding` builder, and guiding from the settings log line and `crust check`'s
      effective settings. Done when `cargo build --workspace` passes with callers fixed
      and `crust check -i samples/cornellbox.usda --json -` has no guiding setting.
- [ ] 2.3a Remove the guiding paragraphs from `site/content/docs/usd/render-settings.md`,
      `usd/overview.md`, `getting-started/quick-start.md`, `architecture/limitations.md`,
      `architecture/design-choices.md` and `help/faq.md`, and the guiding settings from
      `README.md`. Done when `zola build` in `site/` (Zola 0.21) passes and
      `grep -ri "pathGuiding" site README.md` finds only the warnings reference.

## 3. Diagnostic

- [ ] 3.1 Remove the `guiding` factor, `set_guiding`, the `guiding_without_indirect`
      finding, `schedule::guiding_training_spp`, the training-cost branch in the budget
      admission, and `guiding` from the noise-row ordering (`noise.rs`) and
      `Facts` (`checks.rs`). Done when the diagnostic's unit and integration tests pass
      with their guiding cases deleted or rewritten, and the Markdown snapshot
      (`fixture.md`) is regenerated with no guiding row.
- [ ] 3.2 Make `--baseline` skip a trial whose factor is unknown. Done when a test reads
      the committed `before_hardening.json` (which names `crust:pathGuiding`) as a
      baseline and gets deltas for every other trial, with no error.
- [ ] 3.3 Update `site/content/docs/help/diagnosing-a-render.md` and the guiding mentions
      of `claude-desktop.md`. Done when `zola build` passes and neither page says
      "guiding".

## 4. The multi-pass render

- [ ] 4.1 Delete `render_guided`, `GUIDING_PASS_SEED_STEP`, the efficiency estimate, and
      `Renderer::render`'s branch to it; `render` calls the one pass. Done when
      `cargo test -p crust-core` passes and `check_images.sh check` is green.
- [ ] 4.2 Delete `blend_passes`, `blend_weights`, `blend_luminance`, `AovFilm::blend` and
      their tests, after confirming by `grep` that nothing else calls them. Done when the
      build passes and the grep is empty.
- [ ] 4.3 Remove what only a second pass needed: the interrupted-pass rule and the
      "pass without two samples" warning, `max_samples_reached` when its only reader was
      the guided preview, and the "guided render" cases in `stats.rs`, `profile.rs`,
      `crust-render/src/main.rs` and `mcp/render.rs`. Done when the cancellation,
      progressive-snapshot, adaptive-sampling and MCP tests pass unchanged. If a counter
      serves adaptive rounds, keep it and remove only its guiding wording.
- [ ] 4.4 Delete the guided cases from `tests/lpe.rs`, `tests/aovs.rs`, `tests/usd_scene.rs`,
      `tests/render_smoke.rs`, `tests/diagnostic.rs`, `tracer/tests.rs` and
      `benches/integrator.rs`, and `samples/cornellbox_guided.usda`. Done when
      `cargo test --workspace` passes and `grep -rn cornellbox_guided .` is empty.

## 5. The integrator hooks

- [ ] 5.1 In `tracer/path.rs`, remove `GuidingContext`, the `guiding` field of the path
      context, the guide branch and mixture pdf in the scatter step, the `K_GUIDE` key, and
      `train` on path records and its sample emission. Done when `trace_path` takes no
      guiding argument and `check_images.sh check` is green.
- [ ] 5.2 Remove the mixture's NEE twin in `bounce_emission_weight` and
      `escaped_emission` (the `guiding_here` parameters). Done when the MIS tests in
      `tracer/tests.rs` pass and the LPE `C.*[LO]` pin to the beauty still holds bitwise.
- [ ] 5.3 Run the kernel and shading pins after the hook removal:
      `cargo test -p crust-core --test resolve --test lpe` and
      `scripts/test_simd_matrix.sh -p crust-rt`. Done when they pass.

## 6. Delete the module

- [ ] 6.1 Delete `crates/crust-core/src/guiding/`, `tests/guiding.rs`,
      `tests/guiding_field.rs`, the `mod guiding` and `pub use guiding::…` lines in
      `lib.rs`, and any now-unused `pdf.rs`, `color.rs` and `aabb.rs` helpers that were
      guiding-only (`cargo clippy` names the dead ones). Done when
      `cargo clippy --workspace --all-targets -- -D warnings` passes.
- [ ] 6.2 Check the sibling things that only look like guiding are untouched: subsurface
      Dwivedi sampling (`subsurface.rs`) and learned light selection. Done when
      `cargo test -p crust-core subsurface` and the learned-light tests pass.

## 7. Records and contributor docs

- [ ] 7.1 In `openspec/specs/rendering/design.md`, replace § "Path guiding" and
      § "Known gaps: path guiding" with a short "Removed: path guiding" section: why, the
      efficiency-gate idea (ΔEff against a shared reference image), the pass-blend bias
      suspected in #244, and the commit that held the implementation. Remove the guided
      clauses elsewhere in the file (snapshots, cancellation, crops). Done when
      `grep -n -i "guid" openspec/specs/rendering/design.md` shows only that section and
      subsurface Dwivedi.
- [ ] 7.2 Update `docs/architecture.md` (crate table, integrator row, guided-mixture pair,
      RenderControl row), `CLAUDE.md` (the guide mixture ↔ NEE pair, the guided-render
      mentions) and `docs/light_sampling.md`, `docs/shading_performance.md`. Done when
      `grep -rn -i "path guiding\|pathGuiding\|render_guided" docs CLAUDE.md` is empty.
- [ ] 7.3 Final run: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`, `cargo deny --locked check`, and
      `check_images.sh check`. Done when all five are green.

## Workflow follow-up

- Close [#244](https://github.com/doubleailes/crust-render/issues/244) as moot, linking
  this change.
- Archive the change so the deltas fold into `openspec/specs/`.
