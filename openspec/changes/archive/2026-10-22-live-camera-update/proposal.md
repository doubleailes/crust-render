## Why

A host that moves the camera interactively needs a camera move to cost a
settings re-read, not a re-import. Hydra viewports such as usdview move it on
every mouse drag (`hydra-render-delegate`), and the MCP session's agent moves it
too. Today the session's classifier (`crust-render/src/mcp/delta.rs`,
`classify`) sends any `xformOp*` edit on the render camera to `full`. On ALab
frame 1004 that is a ~112 s re-import. Lens edits on the same camera already
apply in place, because `live`'s settings rebuild (`refresh_settings`)
re-reads the camera through the import's own `build_camera`.

The rebuild also guards adaptive tessellation correctly. It refuses whenever
adaptive subdivision or implicit shapes sized on screen would read another
camera projection (`StageFacts::adaptive`, `on_screen_shapes`). That guard
makes every camera move on such a scene a full import, because the camera the
image is seen through is also the camera geometry is diced for. Production
renderers separate the two with a dicing camera. Crust has no way to name one.

## What Changes

- **Dicing camera.** A new relationship, `rel crust:dicingCamera` on the
  `RenderSettings` prim, names the camera that tessellation reads: adaptive
  subdivision levels, per-face edge rates and the radial count of implicit
  shapes. Unauthored, the dicing camera is the render camera, so every
  existing stage imports and renders exactly as before. A target that is not
  a camera is ignored with a warning (a new `WarningCode`), and the render
  camera dices.
- **The render camera's transform is a `settings` edit.** A value edit to
  `xformOp:*` or `xformOpOrder` on the render camera prim is applied in place
  through the settings rebuild. It falls back to `full` when an imported prim
  (geometry, light or material) lies below the camera, or when the rebuild
  refuses: the dicing camera's projection moved, the shutter changed, or a
  warning would change.
- **The in-place update re-reads the dicing camera.** `StageFacts` records
  which camera tessellation read. Moving the render camera keeps the scene in
  place when the dicing camera is another prim. Moving the dicing camera, or
  retargeting `crust:dicingCamera`, is `full`.
- **No CLI flag.** `--camera` still names the render camera only. A
  `--dicing-camera` override is left out until a user needs it.
- **No throughput change.** The integrator, BVH and kernel are untouched.
  Without `crust:dicingCamera`, the only change is which edits the session
  classifies as `settings`.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `usd-scene-import`: a new requirement, "The dicing camera". Tessellation
  that reads a camera (adaptive subdivision, per-face rates, implicit radial
  counts) reads `crust:dicingCamera` when it is authored, else the render
  camera.
- `mcp-session`: "Edits apply in place when they are confined". A transform
  value on the render camera prim is in the `settings` class, and the
  tessellation clause reads the dicing camera's projection.

## Impact

- **`crust-core`**:
  - `scene/usd_import/mod.rs`: `read_camera_ahead` and `subdiv_policy` take the
    dicing camera, and the render settings reader reads the relationship.
  - `scene/usd_import/live.rs`: `refresh_settings` compares the dicing camera's
    keys and checks that the dicing camera is unchanged.
  - `live.rs`: `StageFacts` gains the dicing camera's path.
  - `warnings.rs`: one new code.
  - tests in `tests/live.rs` and `tests/usd_scene.rs`.
- **`crust-render`**:
  - `mcp/delta.rs`: `classify` puts a transform on the render camera in the
    `settings` class.
  - `mcp/tests.rs`: tests for that classification.
- **Docs**:
  - `site/content/docs/usd/render-settings.md`: the new attribute.
  - `site/content/docs/reference/warnings.md`: the new code.
  - `openspec/specs/usd-scene-import/design.md`: adaptive subdivision.
  - `openspec/specs/mcp-session/design.md`: the in-place classes. The
    known gap "Transforms ... re-import" gets narrower.
- **Downstream**: `hydra-render-delegate` depends on this change. Its free
  camera is the render camera, and the stage's own camera is the dicing
  camera.
