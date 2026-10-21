# Proposal

## Why

Production renders are split into passes: a character pass holds out the props
rendered in another pass, and a background pass holds out the characters. Crust
has no holdout (matte) object, so today the split takes two extra renders and a
depth comparison in compositing. That comparison is wrong wherever the occluder
is partly transparent or the edges interpenetrate (issue #340). OpenUSD already
describes passes: a `UsdRenderPass` carries a `matte` collection ("matte objects
render with zero alpha") and a `cameraVisibility` collection. Reading them lets
one stage hold every pass, with no asset edits.

## What Changes

- **Active render pass.** A new `--pass <PRIM_PATH>` option on `render`,
  `check`, `diagnostic` and `light-paths` (the shared scene arguments), and an
  optional `pass` argument on the MCP `open_session`, name the `RenderPass` prim
  to render. With no pass named, no pass is active and every render is
  bit-identical to today, even on a stage that defines passes (Hydra's rule: the
  application, not the stage, chooses the active pass). A path that is not a
  `RenderPass` stops the render, as an unknown `--camera` does.
- **The pass chooses its settings.** The active pass's `renderSource`, when it
  targets a `RenderSettings` prim, is the settings the render reads. Otherwise
  it is the stage's `renderSettingsPrimPath` settings, as today.
- **`collection:matte` → holdouts.** Geometry in the active pass's `matte`
  collection is a holdout. A camera ray whose first hit (after the cutout and
  pass-through walk) is a holdout ends there. That sample contributes zero
  colour, zero alpha, zero to every light path expression, and every data AOV's
  clear value, as an escaped camera ray does. Shadow, indirect and light-sampling
  rays see the holdout as ordinary geometry, so it still casts shadows and
  appears in reflections and refractions. The beauty and its alpha stay
  premultiplied and consistent. Passes that hold each other out split every
  pixel's samples between them, so they recombine exactly with **plus**,
  edges, transparency and motion blur included. `over` applies when the back
  pass hides the front object from the camera instead of holding it out.
- **`collection:cameraVisibility` → camera bit.** Geometry and lights outside
  the active pass's `cameraVisibility` collection are hidden from camera rays.
  Every other ray category sees them unchanged. A pass only removes camera
  visibility: it never shows to the camera what `crust:rayMask` or the
  hidden-light rule already hides. For lights at infinity it clears camera
  visibility from that light's escape mask.
- **Collections are read with full `UsdCollectionAPI` semantics**, including
  `membershipExpression`, which openusd now evaluates. Membership is judged on the
  prim that brings the geometry in (the instance or instancer prim), as light
  linking does.
- **Refused or not read, with a warning** (one `WarningCode` per cause): an
  active pass whose `prune` or `renderVisibility` collection restricts anything
  (not read in this change); a volume region in either collection (ignored);
  collection targets inside an instance prototype (judged on the instance).
- **`crust ls pass`** lists the stage's `RenderPass` prims, one path per line,
  as `ls camera` does.
- **`crust check`** reports the active pass, its settings source, and how many
  geometries are holdouts or hidden from the camera.
- **User documentation**: a new `site/` page for render passes, the `--pass` flag
  in the command-line reference, and the new warnings in the warnings reference.

No breaking change: every existing command line and stage renders as before.

## Capabilities

### New Capabilities

- `render-passes`: choosing an active `UsdRenderPass`, how it chooses its
  `RenderSettings`, how its `matte` and `cameraVisibility` collections are
  resolved against the imported geometry and lights, and the collections it does
  not read yet.

### Modified Capabilities

- `rendering`: a camera ray that reaches a holdout ends with no contribution.
  Secondary rays are unchanged. A world with no holdout runs the integrator
  exactly as before.
- `aovs`: every source's value for a sample that ended on a holdout (alpha 0,
  colour and LPEs 0, data AOVs at their clear value).
- `cli`: the `--pass` scene argument and `crust ls pass`.
- `scene-check`: the render described includes the active pass and its holdout
  and camera-hidden counts.
- `mcp-session`: `open_session` accepts an optional `pass`.

## Impact

- **crust-core**: `scene/usd_import/` (a new sibling, `passes.rs`, beside
  `light_links.rs`, judging membership at the same dispatch site and by the same
  stage path; `settings.rs` choosing the settings prim through the pass;
  `UsdImportOptions::pass`), `rt_world.rs` (a per-geometry holdout flag with a
  `has_holdouts()` fast path, and camera-bit edits through `set_mask`),
  `tracer/path.rs` (one guarded check after `pass_cutouts` on camera rays only),
  `aov.rs` (a holdout sample is recorded as an escape), `warnings.rs`,
  `check.rs`.
- **crust-render**: `SceneArgs::pass`, `LsKind::Pass`, the MCP `open_session`
  argument.
- **Performance**: a world with no holdout pays a single predictable branch per
  camera ray. The zero-AOV instruction count and the golden images must not
  move. Held-out pixels get cheaper: their paths end at vertex 0, and they
  converge at the minimum sample count.
- **Dependencies**: none new. `openusd-schemas` already defines `RenderPass`,
  `COLLECTION_MATTE` and `COLLECTION_CAMERA_VISIBILITY`.
- **Ordering constraint**: the pass's camera visibility must be applied before
  light-link resolution, because demoting a light linked to nothing reads the
  camera bit to decide between a backdrop and dropping the light.
