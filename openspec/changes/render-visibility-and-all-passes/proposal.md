# Proposal

## Why

A shot's compositing passes still take one hand-written stage each, even though
`--pass` reads `UsdRenderPass` (issue #340). A pass cannot turn objects or
lights off: `renderVisibility` is not read, so a "background plate alone,
lights off" pass still needs its own stage. Each pass also re-imports the whole
shot. And nothing compares the passes before a render, so a setting the shot's
base settings leak into every pass (depth of field), or a product that silently
writes to the default path, shows up only in the images (issue #348).

## What Changes

- **`collection:renderVisibility` is read.** Geometry outside the active pass's
  `renderVisibility` is invisible to every ray category, as `crust:rayMask = 0`
  makes it. It stays in the scene's acceleration structure, so a pass changes
  only what each ray sees. A light outside it is off: it is not in the light list,
  lights nothing, and is seen by no ray. Light linking is resolved without it.
  `renderVisibility` takes precedence over `matte` and `cameraVisibility`.
  `prune` is still not read and still warns.
- **Pass-level setting overrides come from USD composition.** A pass's
  `renderSource` settings prim is read as composed. A per-pass `RenderSettings`
  that `inherits` (or `references`, or `specializes`) the shot's base settings
  and overrides `disableDepthOfField` is the supported way to keep a camera
  setting out of a pass. The user documentation gives the recipe. This adds no
  new attribute.
- **`crust render --all-passes`** renders every `RenderPass` on the stage (the
  list `crust ls pass` prints) in namespace order, in one process. Passes whose
  settings agree on everything the import bakes in share one import: geometry,
  materials, textures and the acceleration structure are built once. Each pass
  gets its own holdout flags, ray masks, light list, light selection and
  settings. Every pass's images SHALL be bit-identical to that pass rendered alone
  with `--pass`. Passes whose import-time settings differ get one import per
  group. `--all-passes` cannot be combined with `--pass` or `-o`.
- **Output collisions stop the render.** Before any pixel is rendered,
  `--all-passes` refuses a stage where two passes would write the same file.
  This is the case where a missing product override sends two passes to the
  default path.
- **`crust check --all-passes`** describes every pass: its settings prim, its
  holdout, camera-hidden and render-hidden counts, its products and files, and
  each effective setting that differs from the stage's base settings. That makes
  a leaked depth of field visible before rendering. It reports output collisions
  as a finding. The `crust-check/2` JSON gains a `passes` key, `null` without
  `--all-passes`; the key is additive, so the version does not change.
- **`crust check --pass`** also reports the render-hidden count.
- **User documentation**: the render-passes page loses the
  `renderVisibility` and "one render renders one pass" known gaps. It gains
  `renderVisibility`, the settings-override recipe, `--all-passes`, and
  `--all-passes` on `check`. The command-line reference gains `--all-passes`.
  The warnings reference covers the narrowed `render_pass.collection_not_read`,
  and the `site/` architecture page covers the shared import.

No breaking change. A stage rendered without `--pass` or `--all-passes` renders
bit-identically. A pass that authors no `renderVisibility` restriction renders
bit-identically to today. A pass whose `renderVisibility` restricts something
used to render that geometry, with a warning; it now hides it.

**Prerequisite:** `render-pass-matte-and-camera-visibility` (#340) is archived
first, so its `render-passes` capability and its `scene-check` and `cli`
requirements are in `openspec/specs/`. The deltas below modify those versions.

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `render-passes`: `renderVisibility` is read (geometry and lights); the
  composed `renderSource` carries pass-level overrides; `--all-passes` renders
  every pass, bit-identical to each pass alone; the not-read requirement and the
  known gaps shrink to `prune` and volumes.
- `cli`: `render` takes `--all-passes`, refused with `--pass` or `-o`, and
  refused when two passes would write one file.
- `scene-check`: `--all-passes` describes every pass with its differences from
  the base settings; output collisions are a finding; the JSON gains `passes`,
  and the single-pass description gains the render-hidden count.

## Impact

- **crust-core**:
  - `scene/usd_import/passes.rs`: `ActivePass` judges a set of passes in the one
    traversal, writing a per-pass membership table instead of writing straight
    into `WorldBuilder`. `renderVisibility` is judged beside `matte` and
    `cameraVisibility`. `settings.rs` keeps resolving each pass's
    `renderSource`.
  - `rt_world.rs`: apply a pass's table (holdouts, masks) to a built `World`.
  - The light list and light links: resolved per pass from the lights the pass
    keeps.
  - The import options gain the set of passes; `check.rs` gains the per-pass
    description.
- **crust-rt**: rewrite the masks in triangle and instance records of a built
  scene in place (the BVH's topology and bounds are unchanged).
- **crust-render**: `--all-passes` on `render` and `check`, the per-pass render
  loop, the collision check, and the `passes` report.
- **Performance**: a render without `--all-passes` gains no per-ray work. The
  goldens and the zero-AOV instruction count must not move. With `--all-passes`,
  N passes sharing a group cost one import plus N renders, instead of N imports
  plus N renders.
- **Dependencies**: none new.
