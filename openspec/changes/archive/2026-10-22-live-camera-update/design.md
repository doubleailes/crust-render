## Context

Two pieces exist already and are kept.

- **Settings rebuild.** `live`'s settings rebuild
  (`scene/usd_import/live.rs`, `refresh_settings`) re-opens the stage,
  re-reads the settings, products and camera through the import's own
  readers, and builds the new `Camera` with `build_camera`. It refuses with
  `Structural` whenever anything the import baked into geometry would move:
  - the subdivision level;
  - the adaptive `ScreenKey`;
  - the implicit-shape projection key;
  - the shutter key;
  - dome visibility;
  - product primvars;
  - the camera path.
  A refusal falls back to `full`. The rebuild never reads the camera's
  ancestors' transforms any differently from the import, because
  `build_camera` calls `local_to_world`.
- **Camera read before traversal.** Tessellation reads one camera projection,
  read before the traversal by `read_camera_ahead`
  (`usd_import/mod.rs:1553`). That function is called with the render
  camera's `CameraChoice`, and `subdiv_policy` turns its result into the
  `SubdivPolicy` and the implicit shapes' `ScreenProjection`. Nothing else in
  the import reads the camera's position.

The only reason a render camera move is `full` today is the session's
classifier: `classify` in `crust-render/src/mcp/delta.rs` puts the camera in
`settings` only when `!is_xform`.

## Goals / Non-Goals

**Goals:**
- A render camera move on a scene with no camera-dependent tessellation is a
  `settings` update, bit-identical to a `full` application.
- With `crust:dicingCamera` authored, a render camera move is a `settings`
  update whatever the tessellation.
- Without `crust:dicingCamera`, every image is unchanged.

**Non-Goals:**
- A camera-only fast path that skips re-opening the stage. The settings
  rebuild costs 18–60 ms on ALab (`mcp-session` design record), which is
  enough to restart a progressive render per drag event. Measure before
  optimising.
- Moving the dicing camera in place (re-dicing). It moves geometry, so it is
  `full`.
- Camera motion blur. The camera itself is still not moved over the shutter.
- A `--dicing-camera` CLI flag.

## Decisions

### D1. The dicing camera is a relationship on `RenderSettings`

`rel crust:dicingCamera`, read beside `crust:subdivisionEdgeLength` and with
the same pass resolution (`render_settings_subdiv_edge_length(index, pass)`'s
sibling). A relationship rather than a string, so that composition remaps it
as it remaps `RenderSettings.camera`.

**Alternatives considered:**
- An import option (`UsdImportOptions`). A Hydra host could set it, but the
  session's layer would not record it, so `crust render <output>` would dice
  differently from the session. That breaks the session ↔ full-import
  bit-identity.
- A per-product override. Dicing is a property of the scene, not of an
  output, and every product of one render shares the scene.

### D2. `read_camera_ahead` reads the dicing camera; the shutter stays the render camera's

`read_camera_ahead` returns `(projection, shutter)`. It is split so that:
- the projection comes from the dicing camera, when one resolves;
- the shutter still comes from the render camera (or, with none named, from
  the first camera).

`subdiv_policy`'s `wanted_camera` argument becomes "the camera tessellation
reads". Its "needs a camera" warning fires only when neither camera
resolves, so a stage with a dicing camera but no named render camera is
adaptive (spec scenario). The dicing camera's projection uses the render's
resolution and framing (`products.framing`), as the render camera's does
today.

A dicing target that is not a camera raises a new
`WarningCode::CameraDicingNotCamera` (`camera.dicing_not_a_camera`,
`Approximated`, `Each`), and the render camera dices.

### D3. `StageFacts` records the dicing camera's path; the rebuild refuses when it moves

`StageFacts` gains `dicing: Option<String>`. `refresh_settings` compares
`read.subdiv.adaptive` and `read.projection` against the facts exactly as
now, since those keys are now the dicing camera's. It also refuses when the
resolved dicing path differs from `facts.dicing`, for example after
retargeting the relationship. The existing "render camera changed" refusal
stays.

Because the keys hold the projection's bits, a render camera move with no
dicing camera authored still refuses on an adaptive scene. Nothing has to
special-case "is the render camera also the dicing camera".

### D4. The classifier admits transforms on the render camera, guarded by provenance

In `classify`, the condition `provenance.camera() == Some(prim) && !is_xform`
drops `!is_xform`, with one guard: an imported prim must not lie below the
camera. `Provenance` already lists materials and lights by path and geometry
by `prim_of_geometry`; it gains the prims that emitted a volume region, which
are not geometry runs. The guard is a prefix walk over those
(`Provenance::any_below(path)`, new), computed once per classification.

An edit to an ancestor of the camera stays `full`, since the ancestor may
carry geometry. It would reach the rebuild and be re-read correctly, but
classifying an ancestor needs the same prefix walk in reverse. That is left
for when someone needs it. `!resetXformStack!` authored through the camera's
own `xformOpOrder` is an edit on the camera, so it is `settings`: `build_camera`
composes through `local_to_world`, which honours the reset.

**Alternative considered:** treating any transform edit as "try in place, fall
back on refusal". That is wrong because the settings rebuild re-reads only
the camera. A mesh's transform would be silently ignored, not refused.

## Risks / Trade-offs

- [The rebuild re-opens the stage per move, which is slow on production
  stages] → 18–60 ms measured for ALab confined edits. This change records
  the figure for a camera move on ALab in the `mcp-session` design record.
  A camera-only fast path is a follow-up if Hydra tumbling shows it is the
  bottleneck.
- [A dicing camera far from the render camera gives coarse geometry in view]
  → That is the contract of a dicing camera. Documented on the render-settings
  page with a picture of when to use it.
- [Something the import reads from the camera other than the projection and
  shutter is missed, so an in-place move would diverge from `full`] → The
  bit-identity tests in tasks 3.x compare in-place against `full` on:
  - cornellbox;
  - an adaptive scene with a dicing camera;
  - a scene with on-screen implicit shapes;
  - a scene with `domeLightCameraVisibility`.

## Migration Plan

No migration. Unauthored stages behave as before. Rollback is a revert: the
attribute is ignored by older builds, which dice through the render camera.
