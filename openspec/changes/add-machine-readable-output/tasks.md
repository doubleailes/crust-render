# Tasks

## 1. Report conventions (design D1–D3)

- [ ] 1.1 Add `serde` (derive) and `serde_json` as direct dependencies of
      crust-core, and `serde_json` to crust-render. Verify that
      `cargo deny --locked check` and `cargo build --workspace` pass, and that
      `Cargo.lock` gains no new package.
- [ ] 1.2 Add the `Report<T>` envelope (`format`, `crust_version`, flattened
      body) and the `finite_or_null` and `Duration` → `*_s` helpers in
      crust-core. Verify with unit tests:
      - `format` is the first key and `crust_version` the second;
      - `f64::INFINITY` and NaN serialize as `null`;
      - `Duration::from_millis(1500)` serializes as `1.5`.

## 2. `render --stats-json` (design D1, D4; `cli` spec)

- [ ] 2.1 Derive `Serialize` for `RenderStats` and its children (`Phase`,
      `SceneCounters`, `PrimitiveCounts`, `RayStats`, `ImageCounters`,
      `TextureCacheStats`, `PtexCacheStats`, `SubdivisionCounters`,
      `DisplacementCounters`, `RenderProfile`). Give unit suffixes to fields
      whose names lack them, and serialize the derived figures the text report
      prints. Add `peak_memory_bytes: Option<u64>` to `RenderStats`, taken
      once by the host before reporting, and make `Display` print that field
      instead of calling `peak_memory_bytes()`. Verify with:
      - a fixture `RenderStats` test that pins the full key set of
        `crust-stats/1` and fails when a field is added without a reviewed
        name;
      - a test that the text report's `peak memory (RSS)` line and the JSON's
        `peak_memory_bytes` come from the same snapshot, and that `None`
        serializes as `null` and prints no line.
- [ ] 2.2 Add `--stats-json PATH|-` to `render`. With `-`, choose
      `Terminal::Stderr` in `main`, so stdout carries only the JSON; with a
      path, write the file after the images. Print the text table only with
      `--stats`/`--profile`. Verify with CLI tests on
      `samples/cornellbox.usda`:
      - `--stats-json -`: stdout parses as one object, and stderr has the
        INFO lines;
      - `--stats --stats-json f.json`: the text report is logged and the file
        parses;
      - `--profile` adds a `profile` key, which is absent without it.
- [ ] 2.3 Document `--stats-json` in `site/content/docs/reference/command-line.md`
      (with the `-` rule) and in `openspec/specs/cli/design.md` (§ `--stats` /
      `--profile`, and § Logging for the stream switch). Verify with
      `zola build` in `site/`.

## 3. EXR sampling stamp (design D7; `image-output` spec)

- [ ] 3.1 Record what the import rendered from: `Scene` gains `camera_path`
      (set by `resolve_camera` after any fallback, `None` for the procedural
      camera) and `time` (the time code as given, not the floored seed).
      Verify with import tests:
      - `RenderSettings.camera` naming a missing camera gives the first
        camera's path;
      - the procedural fallback gives `None`;
      - `-f 10.5` and `-f 10.25` give times `10.5` and `10.25`, with the same
        sampler seed `10`.
- [ ] 3.2 Add `SamplingStamp` to crust-core, built from `RenderSettings`,
      `RayStats`, the camera path and the time, with the typed values of
      design D7: every attribute in the `image-output` spec, filter and radius
      included. Verify with unit tests:
      - a non-adaptive 16 spp setting gives `sppTaken = (16, 16)`;
      - clamp `None` gives `0`;
      - `PixelFilter::Gaussian { radius: 1.5 }` gives `gaussian` and `1.5`;
      - no time and no camera path omit `crust:frame` and `crust:camera`.
- [ ] 3.3 Add one `stamp(&mut LayerAttributes, &SamplingStamp)` in crust-render
      and call it from both `write_beauty` and `products::write_product`.
      Verify with a test per writer that renders the Cornell box at `-s 16
      --indirect-clamp 0` and reads back `crust:spp = 16`,
      `crust:minSpp = 32`, `crust:sppTaken = (16, 16)` and
      `crust:indirectClamp = 0`, with matching typed attributes.
- [ ] 3.4 Reserve the `crust:` prefix in `write_product`: authored values are
      not copied, and a warning is logged, as for `colorInteropID`. Verify
      with a test where a product authors `driver:parameters:crust:spp`.
- [ ] 3.5 Rewrite tests that pin whole EXR headers to compare with `crust:*`
      attributes removed, and keep pixel and window comparisons bitwise.
      Verify that `cargo test --workspace` passes.
- [ ] 3.6 Document the stamp in `site/content/docs/usd/aovs.md` (or the
      image-output page it links) and in `openspec/specs/image-output/design.md`,
      listing each attribute, its EXR type and the flag/USD attribute it
      mirrors. Verify with `zola build`.

## 4. `crust diff` (design D5, D8, D9; `image-comparison` spec)

- [ ] 4.1 Move the example's `load` into crust-assets as
      `read_exr_planes(path) -> Result<Planes, Error>`, which also returns the
      header's `crust:*` stamp. Verify with unit tests: a written-then-read
      multi-layer EXR round-trips its channels and stamp, and a missing or
      truncated file is an `Err`, not a panic.
- [ ] 4.2 Move the comparison into crust-core as `compare(&Planes, &Planes)
      -> DiffReport` (`DiffReport: Serialize`, `crust-diff/1`), with no I/O.
      Verify with unit tests on in-memory planes:
      - identical files;
      - one moved NaN;
      - equal infinities;
      - a channel only in `a`;
      - a resolution mismatch;
      - no beauty (no metrics);
      - an infinite `max_rel` → `null`.
- [ ] 4.3 Render the text report from `DiffReport`, keeping the example's line
      formats, with the first line `WxH  differing pixels: N/T (P%)`. Verify
      that, on a pair of fixture EXRs, the old example's output matches the
      new text output line for line, by running both before deleting the
      example.
- [ ] 4.4 Add the `diff` subcommand to `main.rs` only (no new crust-render
      source file): `crust diff <a> <b> [--json PATH|-]`, reading through
      crust-assets and comparing through crust-core, exiting 0 identical,
      1 differs, 2 error, with the log on stderr. Verify with CLI tests for
      each exit status, and that `--json -` stdout parses.
- [ ] 4.5 Add comparability (D8) to `compare`: from both stamps, compute `unknown`, `ok` or
      `warn` with notes, put them in the JSON, and print the notes to stderr in
      text mode. Verify with tests:
      - unstamped → `unknown`;
      - two `-s 16` renders → `ok`;
      - `-s 64` (above `minSpp`) → `warn` with the adaptive note;
      - clamp 10 vs 0 → `warn` with the clamp note;
      - `--filter gaussian` vs `--filter box`, and the same filter at two
        radii → `warn` naming the field;
      - stamps differing only in `crust:version` → `ok`;
      - in every case the exit status depends only on the pixels.
- [ ] 4.6 Document `crust diff` in `site/content/docs/reference/command-line.md`
      (exit statuses, JSON, comparability, and that `ok` is not proof of noise)
      and in `openspec/specs/cli/design.md`'s command cookbook. Verify with
      `zola build`.

## 5. Retire `exr_diff` (design D9)

- [ ] 5.1 Switch `scripts/check_images.sh` to `crust diff` and its exit status
      (drop the `cargo build --example` line and the `sed` parse). Verify
      that `check_images.sh record`, then `check` on an unchanged tree, prints
      "All scenes bit-identical." and exits 0, and that it exits 1 after
      `-s 15` goldens are swapped in.
- [ ] 5.2 Switch `scripts/gen_texture_alias_scene.py`'s `diff()` to
      `crust diff --json -` without `check=True`: accept exit 0 and 1, read
      `beauty.rmse` and `beauty.mean_abs` from the JSON for both, and raise on
      2. Verify by running its `diff()` on two renders at different spp (exit
      1, metrics returned) and on a missing file (raises).
- [ ] 5.3 Delete `crates/crust-render/examples/exr_diff.rs`. Replace every
      reference with `crust diff`:
      - `CLAUDE.md`;
      - `scripts/bench_scenes.sh`;
      - `docs/*.md`;
      - `openspec/specs/*`;
      - `samples/*.usda` comments;
      - the active changes `add-diagnostic-command`,
        `neural-visibility-light-selection`, `render-gaussian-splats` and
        `retire-openusd-workarounds`.

      Verify that `rg exr_diff --glob '!openspec/changes/archive/**'` finds
      nothing.

## 6. `ls` metadata and time code (design D6; `cli` spec)

- [ ] 6.1 Turn `list_prims` into `list_records(path, kind, frame)` returning
      per-kind records, and keep `Scene::list_usd` returning paths from them.
      Verify that the existing `ls` tests pass unchanged, and that text output
      is byte-identical for `cornellbox.usda` and a streamed-chunk sample.
- [ ] 6.2 Factor the import's camera choice (`wanted_camera` precedence:
      host path, first product's camera, `RenderSettings.camera`; then
      `resolve_camera`'s first-camera fallback) into one function the import
      and the listing both call. Verify that the existing camera-selection
      import tests pass unchanged.
- [ ] 6.3 Camera records through `CameraFrame::read` and `build_camera`'s
      `fStop` / `focusDistance` reads (made `pub(super)` if private):
      `focal_length_mm`, `aperture_mm`, `f_stop`, `focus_distance`,
      `is_render_camera`, `hidden`. Verify with tests:
      - an authored and an unauthored focal length, the unauthored one equal
        to the render's fallback;
      - `is_render_camera` for a product camera over `RenderSettings.camera`,
        for a missing settings camera (the fallback is marked), and for a
        stage without cameras (none marked).
- [ ] 6.4 Light records through the `lux.rs` readers: `type`, `intensity`,
      `exposure`, `color`, `normalize`. Verify with a test on
      `samples/*` lights, and with a stage whose intensity is time-sampled,
      read with and without `-f`.
- [ ] 6.5 Material records: `surface` (authored surface shader id, or
      `null`) and `bound`, true when `resolve_bound` resolves at least one
      geometry prim the walk visits to the material, computed only with
      `--json`. Verify with a test stage holding:
      - a material bound directly;
      - one bound only through an ancestor;
      - one bound through a collection;
      - one targeted by a binding that a `strongerThanDescendants` ancestor
        binding overrides (`bound = false`);
      - one bound only for a purpose other than the render's (`bound =
        false`, unless the full-purpose fallback reaches it);
      - one bound by nothing.
- [ ] 6.6 Add `--json PATH|-` (`crust-ls/1`) and `-f/--frame` to `ls`, parsing
      `-f` like `render -f`. Verify with CLI tests (`ls camera --json -`
      parses, records are in text order) and a key-set test per kind. Measure
      `ls material --json` on ALab against text `ls`, and record the ratio in
      the cli design record.
- [ ] 6.7 Document `ls --json` and `ls -f` in
      `site/content/docs/reference/command-line.md` and
      `openspec/specs/cli/design.md`. Verify with `zola build`.

## 7. Integration

- [ ] 7.1 Run the CI set locally:
      - `cargo fmt --all -- --check`;
      - `cargo clippy --workspace --all-targets -- -D warnings`;
      - `cargo test --workspace --no-fail-fast`;
      - `cargo deny --locked check`;
      - the pinned nightly clippy.
- [ ] 7.2 Confirm there is no render-path cost: callgrind on
      `samples/cornellbox.usda -s 2` with `RAYON_NUM_THREADS=1`, parent vs
      this change, must agree within noise (the stamp and JSON run after
      tracing).
- [ ] 7.3 Agent loop smoke test: render the Cornell box twice at `-s 16
      --indirect-clamp 0` with `--stats-json`, then run `crust diff --json -`
      on the pair. Verify exit 0 and `comparability.status = "ok"`; then
      re-render one side at `-s 64` and verify `warn`.

## Workflow follow-up

- Update `add-diagnostic-command`: its task 2.1 (serde) is done by this
  change, its report should use the `Report<T>` envelope, and its `cli`
  "Subcommands" delta should list `diff`.
- Archive this change after review, and sync `cli`, `image-output` and the
  new `image-comparison` main specs.
