# Proposal

## Why

Crust's warnings are its most useful feedback about a scene. Each one means something
authored was refused, approximated or skipped. Today they exist only as prose in the
log. About 140 `warn!` sites, most of them in the USD import, print free-form lines
that no JSON report carries. So an agent or a pipeline asking "why does this render
look wrong?" has to scrape log text. The warn-once sites also name only the first
offending prim ("Mesh at … (and possibly others)"), so neither a person nor a tool
can find the rest. The CLI already speaks versioned JSON (`crust-stats/1`,
`crust-ls/1`, `crust-diff/1`, `crust-diagnostic/1`); warnings are the gap.

## What Changes

- **A stable warning vocabulary.** Every warning raised while a stage is imported
  gets a code (`<domain>.<cause>`, e.g. `mesh.displaced_at_cage`,
  `material.fallback_default`, `light.degenerate_shape`). Each code also gets one
  fixed **kind**: `refused` (an invalid authored value was replaced by a
  fallback), `approximated` (a valid value crust renders differently from what was
  authored) or `skipped` (something authored produces nothing). There is one code
  per *cause*, not per call site, so the vocabulary is expected to hold about 50–70
  codes. Codes are public and documented. Adding a code is compatible; renaming or
  removing one is a breaking change to every report that carries it.
- **Every occurrence is counted.** The import collects one record per code. A record
  holds the code, its kind, how many times it fired, the first prims it fired on
  (up to a cap) and the first occurrence's message. The warn-once sites still log
  only once, but the record counts every occurrence. "(and possibly others)" becomes
  a number and a list.
- **Log lines carry their code.** A warning in the log reads
  `WARN [mesh.displaced_at_cage] Mesh at /geo/rock …`. The rest of the text is
  unchanged, so the documentation can be searched by code.
- **The engine returns warnings with the scene.** A loaded `Scene` carries the
  warning records its import raised, just as it carries its import stats. The
  engine still writes nothing; hosts (the CLI, later Hydra) decide what to do with
  them.
- **New subcommand `crust check`.** It imports a stage exactly as a render would,
  takes the same scene-shaping flags as `diagnostic`, renders nothing and reports:
  - the warnings;
  - the camera, resolution and products a render would use;
  - the scene counts.

  The output is text on stdout by default. `--json PATH|-` writes `crust-check/1`.
  `--deny <kind>[,<kind>…]` (or `--deny all`) makes the run exit 3 when any warning
  of a denied kind was raised. That gives an agent or a CI job a stop condition for
  "make this stage warning-free". The command exits 0 otherwise, 1 on an error and
  2 on a usage error.

Out of scope, and planned as follow-ups:

- warnings raised during the render itself (they come from worker threads, and
  there are only a handful);
- warnings raised while reading the environment configuration;
- warnings in the `crust-stats/1` / render summary and in the diagnostic report;
- a pixel probe.

## Capabilities

### New Capabilities

- `scene-warnings`: covers the warning vocabulary (codes, kinds, compatibility
  rules), which warnings are collected and how (one record per code, every
  occurrence counted, capped prim list, deterministic order), the `[code]` prefix in
  log lines, and the record's JSON shape.

### Modified Capabilities

- `cli`: the `Subcommands` requirement gains `check`. New requirements cover what
  `crust check` reports, its text and `crust-check/1` JSON outputs, `--deny`, and
  its exit codes.

## Impact

- **crust-core:**
  - a new warnings module: the code enum with each code's kind, documentation
    string and log policy, plus a scoped collector entered by `load_scene` (the same
    pattern as `EvalTimeScope`);
  - every import-time `warn!` in `scene/usd_import/`, `scene.rs` and `color.rs` moves
    to the collecting macro;
  - the warn-once booleans in `mesh.rs` and elsewhere are replaced by a per-code
    log-once policy;
  - `Scene` gains its warning records, and `lib.rs` re-exports the types.
- **crust-assets:** loaders called during the import record through the same macro
  (UDIM tile skips, stale `.tx` tiles, failed conversions). Loader failures the core
  already reports at the call site keep that single core-side code.
- **crust-render:** the `check` subcommand and its text and JSON writers.
- **Docs:**
  - a new `site/content/docs/reference/warnings.md` listing every code, its kind
    and what to do about it;
  - `reference/command-line.md` for `check`;
  - `openspec/specs/cli/design.md` § Logging and § Machine-readable reports;
  - a `docs/architecture.md` invariant: a new warning is a new code.
- **Performance:** the warnings are import-only and bounded (one record per code,
  capped prim list), so render throughput is unaffected. No new dependencies.
