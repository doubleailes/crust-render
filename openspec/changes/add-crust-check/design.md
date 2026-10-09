# Design

## Context

See proposal.md (Why). The pieces `crust check` assembles already exist; only one of
them is new:

- **The render's import.** `crust-render/src/main.rs` `load_scene(&SceneArgs, …)` is
  the function `render` and `diagnostic` both call, and `SceneArgs` is the flag set
  they share.
- **The render described.** `Scene` carries `camera_path`, `settings` (resolution,
  region) and `aovs`. `crust-render/src/products.rs` has `product_channels`
  (channel names per var) and `refuse_shared_paths` (drops a product whose file
  another product already claims), which `render` applies before writing.
- **Effective settings.** `diagnostic/mod.rs` `effective(&RenderSettings, &Options)`
  builds `Vec<Setting>` (name, value, flag, usd_attribute). `report.rs` defines
  `SceneInfo` (path, frame, camera, resolution, region).
- **Findings.** `diagnostic/checks.rs` maps `Facts` → `Vec<Finding>`. Every fact
  only the baseline knows is an `Option`, and the checks that need one return early
  on `None` (`f.indirect_dominant?`). Today's checks that are decidable after the
  import alone: `textures_without_tx`, `many_lights_uniform` and
  `visualization_strategy`, plus `peak_memory` when it is fed the import's peak.
- **Import costs.** `scene.stats` holds the import phases (time, RSS/peak) and the
  `SceneCounters` that `crust-stats/1` serialises.
- **Warnings.** `Scene::warnings`, which `structured-warnings` adds (a dependency).

## Goals / Non-Goals

**Goals:**

- "As a render would" holds by construction: the same `load_scene`, the same flag
  struct, the same product resolution.
- No new vocabulary: every section reuses the shape and key names of a report that
  already exists (`crust-diagnostic/1`, `crust-stats/1`, the warning record).
- `crust diagnostic` output stays byte-for-byte identical.

**Non-Goals:**

- A faster, check-only import (skipping BVH builds or texture decodes). It is
  tempting, but then "as a render would" stops being true by construction. It is a
  follow-up with its own measurement.
- Denying on findings, multi-stage or multi-frame checks.
- New static checks. Any check added to `CHECKS` later appears in `check`
  automatically if it is decidable from the import.

## Decisions

### D1. `check` reuses `load_scene` and `SceneArgs` unchanged

`Command::Check(Box<CheckArgs>)` flattens `SceneArgs` exactly as `DiagnosticArgs`
does, and adds `--json` and `--deny`. clap enforces "`-i` is required" through a
`CheckArgs` validation, not by changing `SceneArgs` (its `input` stays optional for
`render`). The scene is loaded by the same `load_scene` call, with the same
`FileAssets` built from `--auto-tx`.

*Alternative:* a crust-core `check(path, options)` entry point. Rejected because
`load_scene` and `FileAssets` live in the CLI. Duplicating them in the engine would
reintroduce exactly the drift D1 avoids.

### D2. Products go through the render's own resolution

`check` applies `refuse_shared_paths` to `scene.aovs` and lists each surviving
product with its resolved file path and `product_channels`. With no products, it
reports the default beauty path `render` would write. That default is factored into
one function both commands call, instead of being repeated in `check`.

### D3. Share the diagnostic's pieces; don't copy them

- `effective()` currently takes the diagnostic's `Options`. Split it into a part
  that depends on `RenderSettings` plus the shared scene flags, which `check` calls,
  and the few diagnostic-only rows, which stay in `diagnostic`. `Setting` and
  `SceneInfo` move to the shared `report.rs` scope (or are re-exported), with
  unchanged serde names.
- Add `Facts::from_import(&Scene, auto_tx, import_peak)`, which fills `lights`,
  `light_selection`, `auto_tx`, `textures_without_tx`, `guiding`, `strategy`,
  `peak_mem_bytes` (the import's peak) and `machine_mem_bytes`, and leaves every
  baseline field `None`. The diagnostic keeps building its full `Facts` as today. A
  test asserts that, for the same scene, the import-only findings are a subset of
  the diagnostic's pre-baseline findings, with equal ids, kinds, evidence and
  actions.
- A diagnostic snapshot test (the existing ones in `diagnostic/snapshots`) pins
  that the diagnostic's report does not change.

### D4. The `crust-check/1` report lives in crust-core, and its writers in crust-render

The `CheckReport` struct sits beside the other reports and serialises through
`report.rs` (`format`, `crust_version`, `Num` for nullable numbers). Its keys are
in the order the spec fixes. `import` is the same phase objects `crust-stats/1`
writes, and `counts` is the same `scene` object. `crust-render` builds the report
and writes either text or JSON. The JSON is written through the existing
`write_json` and `is_stdout` helpers, so `--json -` moves the log to stderr as for
`ls`, `diff` and `--stats-json`.

The text writer is a small function in `main.rs`, as `ls`'s text output is today.
If it grows past about 100 lines it moves to `crust-render/src/check.rs`.

### D5. `--deny` is a typed list, evaluated after the report is built

`--deny` parses through a clap `ValueEnum` list (`refused`, `approximated`,
`skipped`, `all`). An unknown kind is therefore a usage error (exit 2), for free.
`denied` holds the codes of the records whose kind is denied, in record order. The
reports are written first, and the exit status is decided after. A failed write is
exit 1 even when warnings were denied: the error takes precedence, because without
a report there is nothing to act on.

### D6. Exit status 3, matching `diagnostic`

3 already means "the report was written, but the condition you asked about was not
met" for `diagnostic` (budget exhausted). Reusing it keeps one convention across
subcommands, documented in `reference/command-line.md`.

## Risks / Trade-offs

- [Products dropped by `refuse_shared_paths` warn from the CLI, outside the import's
  warning scope, so they never become records] → the product is absent from
  `products` and the WARN line is on stderr. This is noted as a known gap in the
  `scene-check` design record. Coding it is a follow-up once CLI-side warnings have
  a scope.
- [A check costs a full import: minutes on Moana or ALab] → still far cheaper than
  a render. The import phases in the report make the cost visible. The faster
  import is a measured follow-up (Non-Goals).
- [Finding summaries were written for the diagnostic and mention trials
  (`visualization_strategy`: "every trial is measured against it")] → reword the
  summaries that mention trials so they read correctly in both reports. That changes
  diagnostic text but no ids, evidence or actions, so the snapshot update is
  reviewed as text-only.
- [`effective()` refactor changes diagnostic output by accident] → covered by the
  snapshot test in D3.
- [`structured-warnings` slips] → `check` can land without warnings only by
  dropping a section and a flag, which changes the spec. Instead the dependency is
  explicit, and this change starts after `structured-warnings` task group 1 lands.
  Group 1 provides `Scene::warnings`, even before the sites are migrated.
