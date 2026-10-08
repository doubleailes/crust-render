## Why

Making a crust render faster or cleaner today is manual expert work. The
workflow is:

1. Render with `--stats` / `--profile`.
2. Guess which setting matters.
3. A/B it with the tooling CLAUDE.md describes (`bench_ab.sh`, `-s 16`,
   `--indirect-clamp 0`, `exr_diff`).
4. Avoid the traps that tooling exists for: sequential timings that lie by
   15%, a biased clamp that "removes noise", and adaptive sampling coupling
   time to variance.

The goal is for an LLM agent to run this loop:

```
diagnose → change a setting → diagnose again → stop when converged
```

The agent needs one command that does the measuring correctly and reports
evidence, not guesses, in a form it can parse, compare across runs, and act
on.

Most of the parts exist:

- `render_guided` already computes efficiency `E = 1/(cost · MRSE)` and
  drops guiding when `ΔEff < 1`;
- `RayStats`, the profile and the texture caches already explain where time
  goes.

What is missing:

- a driver that applies this test to every unbiased setting on
  representative crops, within a time budget;
- a report built for a machine reader.

## What Changes

- **`crust diagnostic -i <scene> [--budget 120s] [--json PATH] [--baseline PREV.json]`**
  plus the render's scene-shaping flags: `--camera`, `-f`, `--region`, and
  the setting flags (`--strategy`, `--light-selection`,
  `--light-samples[-indirect]`, `--indirect-clamp`, `--filter`,
  `--subdiv-*`, `--auto-tx`). The agent can therefore diagnose the state its
  last change produced without editing USD.
- **Diagnose only**: it writes no image and does not modify the stage. Its
  only outputs are:
  - the Markdown report on stdout;
  - the JSON report at `--json` (default `crust-diagnostic.json`);
  - the log on stderr.
- **One import.** Every probe and trial reuses the imported scene. A new
  `Renderer::reconfigure(settings)` rebuilds only the state that depends on
  the settings: light selection, including the `learned` pre-pass, and
  guiding.
- **Phases, in priority order, until the budget is spent:**
  - **P0 static checks** from the import and the inventory, e.g. UV textures
    without a `.tx`, emissive materials that are not light-list entries,
    light count versus selection mode, subdivision memory.
  - **P1 baseline**: the full frame at a low fixed spp, with
    `--indirect-clamp 0` and adaptive sampling off. It produces:
    - a relative-variance map;
    - a per-tile time map;
    - per-light-path variances, through `add-lpe-variance`;
    - variances per light group;
    - ray, profile and cache statistics;
    - the energy the authored clamp *would* remove, measured without
      applying it.
  - **Crops**: up to three, chosen from P1 (highest relative variance,
    highest time, the median tile; overlaps merged), sized so every worker
    thread has tiles to render. With `--region`, that region is the only
    crop. Rendered through `add-render-region`.
  - **Tier 1, unbiased swaps**, ranked by `ΔEff = E_trial / E_baseline` per
    crop and overall:
    - MIS strategy;
    - light selection;
    - light samples at the camera and indirect vertices;
    - guiding on/off;
    - a final combined trial of the conclusive winners.

    The noise breakdown orders the trials: for example, guiding comes first
    when indirect paths dominate.
  - **Tier 2, sample budget** (unbiased, noise against time): the spp
    needed to reach a target MRSE, projected to the full frame, and what
    adaptive sampling at the authored threshold saves.
  - **Tier 3, picture-changing settings**: measured, never ranked, reported
    last:
    - the clamp's energy removed and pixels affected;
    - the share of paths cut by `max_depth`, and the time and energy
      difference of a lower depth;
    - subdivision cost.
- **Measurement discipline built in:**
  - baseline and trial are interleaved, R repeats each;
  - per-pair ΔEff with a spread;
  - a verdict of `better` / `worse` / `inconclusive`, where a ratio within
    ±5% across all pairs is never called a gain;
  - MRSE is measured against a shared reference: the inverse-variance blend
    of every unbiased image of that crop.
- **Report for a machine reader:**
  - versioned `format: crust-diagnostic/1`;
  - stable key order, and units in key names;
  - the effective settings the diagnosis ran with;
  - every suggestion given as both its CLI flag and its `crust:*` USD
    attribute (or `action: none` when crust has no setting for it — never
    an invented one);
  - a `converged` verdict;
  - a `not_tried` list with reasons;
  - a ready-to-run `crust render` command.
- **`--baseline PREV.json`**: a deltas section against a previous run
  (time, MRSE, settings changed, findings resolved or new), refused as "not
  comparable" when scene, frame, camera or resolution differ.
- **Exit status:**
  - `0`: tier 1 completed;
  - `3`: the budget ran out before tier 1 completed (the report is still
    written);
  - `1`: error;
  - `2`: usage error.

## Capabilities

### New Capabilities

- `diagnostics`: the `crust diagnostic` command. This covers its phases,
  measurement rules, crop selection, report format, baseline comparison and
  exit status.

### Modified Capabilities

- `cli`: a third subcommand, and the render flags `diagnostic` shares.

## Impact

- **Depends on:** `add-render-region` (crop trials) and `add-lpe-variance`
  (noise breakdown). Both land first.
- `crust-core`:
  - a new `diagnostic` module: phases, scheduler, crop picker, report types
    and Markdown rendering;
  - `Renderer::reconfigure`;
  - a per-tile timer in the tile loop, off unless requested;
  - a clamp-measurement counter in the integrator: off unless requested,
    with the "off" side the code path that runs today;
  - `serde` / `serde_json`: already in `Cargo.lock`, which `cargo deny`
    checks; they become direct dependencies.
- `crust-render`: the `Diagnostic` subcommand. It parses arguments, calls
  the engine, writes stdout and the JSON, and maps the exit status. Render
  flags shared with `render` move into a flattened `SceneArgs`.
- Performance: none for `render`. The new counters and timers are off
  outside `diagnostic`, and the zero-AOV instruction count is pinned.
- Docs:
  - `site/content/docs/reference/command-line.md` (the subcommand and its
    flags);
  - a new user page on reading the report and using it in an agent loop;
  - the `cli` design record's cookbook;
  - a new `diagnostics` design record.
- Future work, not in this change: per-object noise attribution once
  `add-identity-aovs-openexrid` provides object IDs.
