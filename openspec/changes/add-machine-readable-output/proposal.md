# Proposal

## Why

Agents and scripts already drive `crust` through the shell, but they have nothing
to read reliably:

- `--stats` is a human-formatted table, logged on stdout among the INFO lines.
- `crust ls` prints paths only.
- The "did it change?" check (`exr_diff`) is a cargo example. It exits 0 whether
  or not the images differ, so `check_images.sh` and `gen_texture_alias_scene.py`
  scrape its text with `sed` and regexes.
- Nothing in an EXR says how it was sampled. So nothing can warn that a
  comparison falls into the traps CLAUDE.md documents: adaptive sampling above
  `min_spp` cascading one-ulp differences, or a biased `--indirect-clamp` on one
  side only.

This change gives each of these a stable, versioned JSON form and an exit status
that answers the question. It is also the groundwork `add-diagnostic-command`
needs: serde in crust-core, the format conventions, and a comparability rule.
That change is kept separate.

## What Changes

- **`crust diff <a.exr> <b.exr> [--json PATH|-]`**, a new subcommand.
  - It replaces the `exr_diff` example and keeps its measurement: bitwise
    identity over every channel of every layer, plus the beauty's max abs/rel,
    mean abs, RMSE, relMSE and trimmed relMSE against `a`.
  - Exit status: `0` identical, `1` differs, `2` error. An unreadable file
    becomes an error message instead of a panic.
  - It reads both EXRs' sampling stamps and reports comparability (`ok`,
    `warn` or `unknown`) with notes. Comparability never changes the exit
    status.
- **BREAKING (dev tooling):** `crates/crust-render/examples/exr_diff.rs` is
  deleted. Every reference is moved to `crust diff`: `CLAUDE.md`, scripts,
  `docs/`, specs and design records, active changes, sample comments.
  `check_images.sh` reads the exit status instead of parsing text.
- **`crust render --stats-json PATH|-`** writes the `--stats` report as JSON
  (`crust-stats/1`).
  - It implies collecting the statistics, not printing the table. `--stats`
    can be given as well, to get both.
  - With `-`, the JSON goes to stdout and the render's log moves to stderr.
- **`crust ls` gains metadata and a time code.**
  - `--json PATH|-` (`crust-ls/1`) gives per-prim records:
    - cameras: focal length, aperture, clipping, projection, whether this is
      the render camera, hidden;
    - lights: type, intensity, exposure, color, normalize;
    - materials: the authored surface shader, and whether any prim binds it.
  - `-f/--frame` evaluates those values at a time code.
  - The text output stays one path per line, unchanged.
- **Every EXR `crust render` writes is stamped** with how it was sampled:
  - `crust:spp`, `crust:minSpp`, `crust:sppTaken` (min/max over the pixels);
  - `crust:indirectClamp`, `crust:samplingStrategy`, `crust:lightSelection`;
  - `crust:frame`, `crust:camera`, `crust:version`.

  Both writers stamp: the single beauty EXR and each RenderProduct. A product's
  authored attribute with a `crust:` name is overridden, with a warning.
- All three JSON formats share one envelope: `format`, `crust_version`, then
  the payload. Keys are snake_case with units in the name. Non-finite floats
  are `null`.

## Capabilities

### New Capabilities

- `image-comparison`: `crust diff`. It defines what "identical" means, the error
  metrics, the exit status, the `crust-diff/1` report, and the comparability
  verdict read from EXR sampling stamps.

### Modified Capabilities

- `cli`:
  - the subcommand list gains `diff`;
  - `ls` gains `--json`, `-f/--frame` and per-kind metadata;
  - `render` gains `--stats-json`;
  - a shared rule for every machine-readable report: envelope, naming,
    non-finite values, and which stream the log goes to.
- `image-output`: every EXR records how it was sampled (`crust:*` header
  attributes), and crust's own keys win over authored product attributes.

## Impact

- **crust-core**:
  - `serde` and `serde_json` become direct dependencies (both are already in
    `Cargo.lock`);
  - `RenderStats` and its children gain `Serialize`;
  - the listing returns records instead of strings, read through the import's
    own camera, UsdLux and material readers and evaluated at an optional time
    code;
  - a `SamplingStamp` value built from `RenderSettings` plus `RayStats`.
- **crust-render**:
  - a new `diff` subcommand, with EXR reading through the existing `exr`
    dependency;
  - `--stats-json`, and `ls --json` / `-f`;
  - one stamp function shared by `write_beauty` and `products::write_product`;
  - the log's stream chosen per invocation.
- **Removed**: the `exr_diff` example.
- **Scripts**: `check_images.sh`, `gen_texture_alias_scene.py`,
  `bench_scenes.sh` (a comment).
- **Docs**:
  - `CLAUDE.md`;
  - `docs/` (5 files);
  - `openspec/specs/*/design.md` and `materials/spec.md`, especially the `cli`
    command cookbook;
  - `site/content/docs/reference/command-line.md` and the image-output / AOV
    user pages.
- **Active changes** whose tasks name `exr_diff`:
  `add-diagnostic-command`, `neural-visibility-light-selection`,
  `render-gaussian-splats`, `retire-openusd-workarounds`.
  `add-diagnostic-command` also modifies the `cli` "Subcommands" requirement;
  whichever change lands second merges both subcommand lists.
- **Performance**: none on the render path. The stamp and serialization run once
  per render, after tracing.
- **Output**: EXR pixels are unchanged. EXR headers gain attributes, so tests
  that pin "header byte-identical to before" must be updated to allow the stamp.
