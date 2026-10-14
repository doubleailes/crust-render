# Proposal

## Why

USD payloads and purposes exist so an artist can see a stage, and decide, without
paying for its heaviest parts. Crust ignores both. Every import loads every payload and
draws only `default` and `render` purpose, so on ALab (frame 1004) the first pixel waits
for a ~1:34 import, 80% of the frame, and in a `crust mcp` session every edit that
re-imports pays that again. ALab publishes what a decision needs outside its payloads:
`GEO_PROXY` scopes (`purpose = "proxy"`) and `extentsHint`. The UsdRender settings that
choose purposes, `includedPurposes` and `materialBindingPurposes`, are a known gap.

A DCC handles this with two independent controls on a live stage. A purpose switch is a
display filter over what is composed. Loading a payload composes only that subtree. This
change brings that model to crust in its simplest form: the view is an *input to the
import*. The CLI takes it as flags, and the session holds it as state and re-imports
through the same flags. A session image is then reproducible with one `crust render`
command line.

## What Changes

- **The import honours `includedPurposes` and `materialBindingPurposes`** on the
  `RenderSettings` prim, with the UsdRender fallbacks (`["default", "render"]` and
  `["full", ""]`). A stage that authors neither renders exactly as today. `default`
  purpose is always drawn.
- **The import takes a payload load set**: every payload (the default, as today) or
  none, then per-prim load and unload rules, with USD load-rule semantics and no
  dependence on order. The render camera is always composed: the payloads on its
  ancestry load with it.
- **A proxy stands in only for an unloaded payload.** When `render` and `proxy` are both
  drawn, a proxy is skipped unless its nearest payload-carrying ancestor is unloaded. A
  loaded asset then shows its render geometry and an unloaded one its proxy, with no
  duplicates. Strict USD would draw both. This crust rule applies only to that mix:
  neither purpose alone, nor the stage defaults, are affected.
- **New shared scene flags** on `render`, `check`, `diagnostic` and `light-paths`, each
  overriding the stage the way `--camera` overrides `RenderSettings.camera`:
  - `--purposes <list>`: the purposes drawn besides `default` (usdrecord's
    `--purposes`).
  - `--binding-purposes <list>`: the material binding purposes tried, in order; `all`
    names the all-purpose binding.
  - `--unloaded`: start from no payload loaded (usdview's `--unloaded`).
  - `--load <prim>` and `--unload <prim>`, both repeatable: load or unload the payloads
    at and below a prim. Together they can express any load set a session reaches.
- **The MCP session holds a view**: its load set, purposes and binding purposes. The view
  is session state, never an opinion in the override layer, so `render_final` and the
  layer stay the final render's.
  - **BREAKING (session clients):** `open_session` opens in the **decision view**:
    payloads unloaded, purposes `render` + `proxy` (so a payload loaded later shows its
    render geometry), binding purposes `preview` then all-purpose. An optional `view`
    argument opens in the `final` view (today's behaviour) instead. On a stage without
    payloads, proxies or `preview` bindings, the two views give the same image.
  - New tools: `load(prims)` and `unload(prims)` change the load set, and
    `set_view(...)` sets the purposes and binding purposes or names a preset
    (`decision` or `final`). Each re-imports the saved layer under the new view. None
    writes the layer, and `undo` does not undo them.
  - Every result that reports on the scene (`open_session`, the view tools, `render`,
    `snapshot`, `check`) carries the view and the `crust render` flags that reproduce it.
  - **Renders are reproducible** with those flags: a session render is bit-identical to
    `crust render <output> <view flags>` with the same region and samples.
  - An edit to `includedPurposes` or `materialBindingPurposes` re-imports instead of
    being applied in place, since it moves geometry.
- **Out of scope, recorded as the follow-up:** applying a load or a purpose change in
  place, DCC-style. That means walking only the resynced subtree, as openusd's
  `StageSink::load_rules_changed` reports it, then rebuilding the top level. It breaks
  bit-identity with a fresh import, so it gets its own change, A/B'd against this one.
  Also out of scope: `model:drawMode` stand-ins (`bounds`, `cards`) for unloaded models,
  and making `crust ls` follow the view.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `usd-scene-import`: purposes and material binding purposes come from
  `RenderSettings` (or a host override) instead of being fixed. The import takes a
  payload load set and always composes the render camera.
- `cli`: the shared scene flags gain `--purposes`, `--binding-purposes`, `--unloaded`
  and `--load`.
- `mcp-session`: the session holds a view and opens in the decision view. It gains the
  `load`, `unload` and `set_view` tools, and its results report the view.
  Reproducibility is stated against `crust render` with the view's flags. Three of the
  modified requirements are also modified by in-flight changes (`live-stage-scene-deltas`,
  `live-session-source-reload`, `add-light-path-inspector`). This delta is written over
  their text, so it must be archived after them.

## Impact

- `crust-core`:
  - `scene/usd_import/mod.rs`: `prune_reason` takes the included purposes, and all three
    walks (traversal, placement count, prototype walk) pass the same set. The chunk and
    single-stage opens apply the load set.
  - `materials.rs`: `RENDER_BINDING_PURPOSE` becomes the ordered binding purposes.
  - `settings.rs`: reads the two token arrays.
  - `scene.rs` (`UsdImportOptions`) and `live.rs` (`StageFacts`, so an in-place
    settings update refuses a purpose change).
- `crust-render`:
  - `main.rs`: the four flags go in `SceneArgs`.
  - `mcp/`: `session.rs` and `scene.rs` (`check_args` takes the view's flags), `mod.rs`
    (the three tools and the result fields), and `delta.rs`.
- **Tests**: no checked-in sample has a payload, a proxy or a `preview` binding. A
  fixture with all three is new.
- **Docs**:
  - `site/`: `reference/command-line.md`, `usd/geometry.md` (the purpose row),
    `usd/render-settings.md`, `usd/materials.md`, `help/claude-desktop.md`, and
    `architecture/limitations.md` / `design-choices.md`.
  - Design records: `usd-scene-import` and `mcp-session`, and the `aovs` known gap they
    retire.
  - `docs/architecture.md`.
- **Performance**: the default import is unchanged: every payload loaded, render purpose,
  `full` bindings. Pruning compares a token against a small set instead of two
  constants. Its instruction count is checked with callgrind on `cornellbox.usda`.
