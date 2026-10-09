# Proposal

## Why

Today the only way to learn what crust will make of a stage is to render it: minutes
to hours, and then you read the log. An agent or a pipeline needs a cheap pre-flight
step before committing to that. It should answer these questions in seconds, as
data:

- What would the render use?
- What did crust refuse, approximate or skip?
- Which settings are worth changing?

Each answer already exists inside crust. The answers to the first question are on
`Scene`. The second becomes structured with `structured-warnings`. The third is the
diagnostic's static findings, most of which need no render. But only `render` and
`diagnostic` (which costs a time budget) expose them.

## What Changes

- **New subcommand `crust check -i scene.usda`.** It imports the stage exactly as
  `crust render` with the same flags would. It renders nothing, and writes no image
  or file beside the stage, except the `.tx` files `--auto-tx` creates, as a render
  would. It takes the scene-shaping flags `diagnostic` takes, with the same names,
  values and defaults. `-i` is required.
- **What it reports:**
  - **the render it describes:** stage, frame, camera, resolution, region, and every
    product with the file it would write and its AOV channels;
  - **effective settings:** every setting the render would run with, each with the
    `crust render` flag and the `crust:*` attribute that change it, in the
    diagnostic's existing `effective_settings` shape;
  - **the import:** its phases' time and peak memory, and the scene counts, with the
    keys `crust-stats/1` already uses;
  - **findings:** the diagnostic's static findings that need no render (textures
    without `.tx`, many lights picked uniformly, a visualisation strategy left on,
    …), with the same ids, kinds, evidence and actions as `crust diagnostic`;
  - **warnings:** the import's warning records from `structured-warnings`.
- **Outputs.** By default, a text report goes to stdout and the log to stderr.
  `--json PATH|-` writes `crust-check/1`, following the shared report shape. With
  `-`, the JSON goes to stdout instead of the text.
- **A stop condition.** `--deny <kind>[,<kind>…]` takes warning kinds (`refused`,
  `approximated`, `skipped`) or `all`. When a warning of a denied kind was raised,
  the reports are still written, they name the denied codes, and the process exits
  3. Otherwise exit codes are 0 for success, 1 for an error (stage not openable,
  report not writable) and 2 for a usage error. This follows `diagnostic`'s
  convention that 3 means "report written, condition not met".

Out of scope, and planned as follow-ups:

- denying on findings;
- checking several stages or frames in one run;
- a faster import that skips BVH builds when only checking (the check costs the
  render's import time);
- render-time warnings.

## Capabilities

### New Capabilities

- `scene-check`: covers `crust check`: what it imports and guarantees not to do,
  what its report holds (the render described, effective settings, import costs,
  findings, warnings), its text and `crust-check/1` outputs, `--deny`, and its exit
  status.

### Modified Capabilities

- `cli`: the `Subcommands` requirement gains `check`, which shares the scene-shaping
  flags with `diagnostic` and refuses render-only flags.

## Impact

- **Depends on `structured-warnings`:** the `Warning` records on `Scene`, the
  vocabulary and its reference page. This change lands after it.
- **crust-core:**
  - `diagnostic::checks::Facts` and `checks::run` become callable with import-only
    facts (the baseline fields are already `Option`; the import-only subset needs a
    constructor);
  - the diagnostic's `effective()` settings rows and `SceneInfo` are shared rather
    than duplicated;
  - a `crust-check/1` report type joins the other reports (shared opening and null
    handling in `report.rs`).

  Diagnostic output does not change.
- **crust-render:** the `check` subcommand, which reuses `load_scene`, plus its text
  writer.
- **Docs:**
  - `site/content/docs/reference/command-line.md` (`check`);
  - a short "check before you render" section in
    `site/content/docs/help/diagnosing-a-render.md`;
  - `openspec/specs/cli/design.md` (cookbook entry; § Machine-readable reports);
  - the command block in CLAUDE.md.
- **Performance:** no effect on `render`. A check costs one import plus the cheap
  checks.
