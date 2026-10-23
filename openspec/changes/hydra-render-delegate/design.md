## Context

See proposal.md for the motivation. The state this design builds on:

- **The import reads files.** crust reads USD only through openusd, by path
  (`stage_builder().open(path)`). No in-memory stage can be handed to it, so
  the delegate's layers are files.
- **The MCP session already follows a layer.** It owns a saved override
  layer, diffs it before and after each batch (`mcp/delta.rs`
  `layer_diff`), classifies the change (`classify` → `settings` / `lights`
  / `materials` / `full`), applies it in place through
  `crust_core::live` or re-imports, and keeps decoded assets across imports.
- **It imports exactly as `crust render` does.** It calls the CLI's own
  import and check path (`mcp/scene.rs` → `crate::check_scene`,
  `CheckArgs`). All of it lives in the `crust` binary crate.
- **Progressive rendering exists.** `RenderControl` (`tracer/control.rs`)
  is pulled, not pushed. `snapshot()` clones the region-sized beauty under
  a lock and is tagged with a monotonic `generation`. `cancel` is sticky,
  so each restart takes a fresh control. AOVs are gathered only when the
  render returns.
- **The camera dependency is handled upstream.** `live-camera-update` makes
  a render camera move a `settings` update and adds `crust:dicingCamera`.
  This change depends on it.
- **The release profile aborts on panic** (`Cargo.toml` `[profile.release]`).
- **OpenUSD 26.** The scene index is UsdImaging's default path, and a
  render delegate runs behind an `HdRenderDelegateAdapterRenderer`, which
  hands it the terminal scene index. Task 1.1 settled what a renderer
  implements on 26.x (D1).
- **The OpenUSD `hdCrust` builds against comes from pixi.** It is the
  `openusd` package (26.08, with usdview, headers and `pxrConfig.cmake`) from
  Anders Langlands' ASWF channel (`hydra/pixi.toml`). The plugin must be
  built against the OpenUSD the host runs, and this gives both from one
  environment, on Linux and on Windows. `build_usd.py` stays the fallback
  (cookbook).

## Goals / Non-Goals

**Goals:**
- usdview on Linux with OpenUSD 26: pick Crust, tumble, see progressive
  refinement, edit lights, materials and settings in place, scrub time with
  a re-import.
- One import, one classifier and one in-place update, shared with `crust
  render` and `crust mcp`. The delegate's image is `crust render`'s image of
  the layers it wrote.
- No change to the `crust` binary's output, throughput or required CI.

**Non-Goals:**
- **Out of v1, reported as warnings by the spec:**
  - motion blur (time samples over the shutter);
  - light linking;
  - volume fields;
  - AOVs published while the render refines.
- **Other hosts:** Houdini Solaris, Maya-USD, Omniverse and `husk` are
  untested. Nothing here is usdview-specific on purpose, but only usdview is
  verified.
- **Other platforms:** macOS is not attempted. Linux (CI) and Windows (the
  development host, MSVC with pixi's OpenUSD) are built. The CMake is
  written portably.
- **Reading Hydra data without going through a layer**: option (C) from the
  exploration, kept as the fallback if memory proves (D) too costly (Risks).

## Decisions

### D1. Observe the terminal scene index; Sync calls do nothing

`hdCrust` registers an `HdSceneIndexObserver` on the render index's terminal
scene index. It reads prims through data sources (`HdMeshSchema`,
`HdXformSchema`, `HdMaterialSchema`, `HdLightSchema`, `HdInstancerTopologySchema`,
`HdPrimvarsSchema`, …):
- `PrimsAdded`, `PrimsRemoved` and `PrimsDirtied` (with locators) drive the
  layers, except for the host's own prims: usdview's task controller, free
  camera and default light, all under `/_UsdImaging_<renderer>_<address>`
  (`UsdImagingGLEngine::_ComputeControllerPath`). Only that prefix is the
  host's; a stage root that merely starts with `_` (`/_World`) is written;
- a prim removed or hidden is made inactive in the overs, and active again
  when the next write of it finds it present and visible. A path reported
  removed that the scene index still holds (a resync, removal and addition
  in one sync) is not deactivated;
- Rprim/Sprim `Sync` calls, if the 26.x interface still makes them, do
  nothing.

**The entry point (task 1.1, read from the 26.08 headers that pixi's
`openusd` package installs, and from the v26.08 and v26.11.1 sources):**

- **26.x has no usable native 2.0 renderer interface.** `HdRenderer`
  (`pxr/imaging/hd/renderer.h`) is a stub ("TODO: Add API here"). Its only
  surface is `GetLegacyRenderControl()`, which returns the "transitory"
  `HdLegacyRenderControlInterface` (`hd/legacyRenderControlInterface.h`).
  That interface takes over task execution, AOV buffers, render settings,
  commands and pause/stop. A plugin overriding
  `HdRendererPlugin::_CreateRenderer` (`hd/rendererPlugin.h`) would have to
  reimplement usdview's whole Hdx task graph. No renderer in Pixar's tree
  overrides it, in 26.08 or 26.11.
- **`HdRenderDelegate` is the entry point, and it already sits on the scene
  index.** usdview's engine (`usdImagingGL/engine.cpp`, scene index mode, the
  default) calls `plugin->CreateRenderer(terminalSceneIndex, args)`. For a
  plugin without `_CreateRenderer`, the base class builds an
  `HdRenderDelegateAdapterRenderer` (`hd/renderDelegateAdapterRenderer.h`).
  That adapter creates the delegate through `CreateRenderDelegate()`, and
  then a render index through `HdRenderIndex::NewForBackendEmulation`
  (`hd/renderIndex.h`). The render index then:
  - calls `HdRenderDelegate::SetTerminalSceneIndex(terminalSceneIndex)`
    (`hd/renderDelegate.h`, "Hydra 2.0 API") once the scene index graph
    exists. `hdCrust` registers its observer there;
  - calls `HdRenderDelegate::Update()` at the start of every `SyncAll`,
    before any prim sync. `hdCrust` drains its collected changes there.
- **No deprecation notice.** The only deprecation warning on this path is
  `USDIMAGINGGL_ENGINE_ENABLE_SCENE_INDEX_DEPRECATION_WARNING`. It fires
  only when the scene index is turned *off* (the `UsdImagingDelegate`
  path). The adapter renderer prints nothing.
- **Supported prim types filter the render index, not the scene index.**
  The observer sees the terminal scene index whole. Only the emulation
  (`HdSceneIndexAdapterSceneDelegate`) consults `GetSupported{R,S,B}primTypes`
  when it inserts prims into the render index. So `hdCrust` declares:
  - no rprims, which saves a no-op `HdRprim` per mesh;
  - the sprim `camera`, which the render pass state's `HdCamera` needs;
  - the bprim `renderBuffer`, for AOV bindings.

  It reads every geometry, material, light and instancer prim from the
  scene index.

Implementing `HdRenderer` directly is revisited once 26.x gives it an API
that does not assume the Hdx task graph.

**Alternative rejected:** the `HdRprim::Sync` path. It is deprecated on 26,
it gives per-prim callbacks with no locators, and it would be rewritten
within a release cycle.

### D2. Two layers on disk: a flattened `scene.usdc`, an override `edits.usda`

The session directory holds the layers:

```
<session dir>/scene.usdc   flattened at first population (and at a time change)
<session dir>/edits.usda   subLayers = [@./scene.usdc@]
                           the render settings, products, vars,
                           /__hdCrust/camera and every later change
```

**`scene.usdc` is written by `hdCrust` with Pixar's `SdfLayer`.** Composition
arcs, variants, inherits and instancing are already resolved by Hydra. Each
prim is written `def`, at its Hydra path, with:
- its world transform as one `xformOp:transform`;
- its schema attributes from the data sources;
- a direct `material:binding`;
- asset paths resolved to absolute paths.

USDC rather than USDA for size and write speed. openusd reading Pixar-written
USDC is covered by tests (Risks).

**`edits.usda` is written the same way, and crust-hydra owns it as the MCP
session owns its layer.** It is USDA because the session's diff parses USDA
(`openusd::usda::parse`).

Each batch of changes (D5) is written as `over`s with the changed prim's
current values:
- **Rewriting every attribute of a dirtied prim is safe.** An unchanged value
  makes no diff, so the classifier sees only real changes. Locators decide
  which prims to rewrite, not which class the change belongs to.
- **A removed prim** is authored `active = false`.
- **An added prim** is authored as a `def`.

Both are `full` by the session's classes. At a time change both layers are
rewritten from scratch, which also compacts `edits.usda`.

**Alternatives considered:**
- *One layer rewritten whole per change.* Every change would diff the whole
  scene, and a single light edit would cost a full write of production
  geometry.
- *Bytes over the C ABI, no files.* The import needs paths (Context), and the
  spec requires the layers to be reproducible with `crust render`.

### D3. Prim translation: what Hydra resolved is written as USD's simplest form

| Hydra | written as |
|-------|-----------|
| mesh (`HdMeshSchema`, topology, subdivision tags, primvars) | `Mesh` with `subdivisionScheme`, creases/corners/holes, primvars with interpolation and indices |
| `basisCurves`, `points` | `BasisCurves`, `Points` |
| implicit `cube`/`sphere`/`cylinder`/`cone`/`capsule` | the same schema type (crust tessellates as GeomUtil does) |
| material network (`HdMaterialNetworkSchema`) | `Material` with `Shader` prims, `info:id` from the node type, connections from the network; terminals `surface`, `displacement`, `volume` and `mtlx:*` kept |
| `HdMaterialBindingsSchema` (resolved) | direct `material:binding` |
| lights (`HdLightSchema` + params) | the UsdLux type, `inputs:*` from the params, shaping and textures kept; a `domeLight` whose `domeOffset` puts its pole on +Z (a `DomeLight_1` with `poleAxis` `Z`, or `scene` on a Z-up stage) as `DomeLight_1` with `poleAxis = "Z"`, since a `DomeLight` ignores the pole |
| instancer (`HdInstancerTopologySchema`, `instanceTransforms`) | `PointInstancer` with `protoIndices`, `positions`/`orientations`/`scales` decomposed from the matrices, prototypes as children |
| camera sprims | `Camera` |
| `HdRenderSettingsSchema` (if the stage has one) | read for its camera path, which becomes `crust:dicingCamera` |

- **crust's own prim attributes.** Hydra carries schema data and primvars
  only, never arbitrary attributes, so `crust:volume:*` (fog, smoke),
  `crust:light:*`, `crust:rayMask`, `crust:subdivisionLevel` and
  `crust:motion:*` would never reach the delegate. Nor can the delegate
  reach the stage: `UsdImagingStageSceneIndex` has no `GetStage()`. So
  `hdCrust` registers a *keyless* UsdImaging API schema adapter
  (`apiSchemaName ""` in its `plugInfo.json`), which UsdImaging consults
  for every prim. It publishes every authored `crust:*` attribute under a
  `crust` container, as UsdImaging attribute data sources, so time samples
  and invalidation behave as for UsdImaging's own attributes. The writer
  puts them back on the prim it writes. A prim it writes for nothing else
  (an Xform carrying an inherited `crust:*`) is written typeless with them.
  The adapter is host-side UsdImaging code: it serves any UsdImaging host,
  and is inert for renderers that ignore the container.
- **Instances.** Instancer prims become `PointInstancer`s:
  - each instance's Hydra transform (`instanceTransform × scale × rotate
    × translate`) is factored into positions, orientations and scales;
  - masked instances become `invisibleIds`;
  - each prototype's prims are copied under `<instancer>/Prototypes/p<i>`
    with their Hydra matrices, which are relative to the instancer
    (verified on `instancing.usda`);
  - prims marked `instancedBy` are written only there, never at their own
    path;
  - prototypes are copied 8 instancers deep, as far as crust's import
    expands them (`MAX_INSTANCE_NESTING`); a deeper one is reported and left
    unexpanded, rather than copied for the import to refuse.
  A shear in an instance transform cannot be factored, and is reported.
- **Light links** arrive as the light's `lightLink` / `shadowLink`
  collections. An unlinked light's membership is `//`, or `~//*.*` as
  UsdCollectionAPI expands its default; anything else is reported once.
- **Visibility and purpose.** Hydra resolves both. Only visible prims whose
  render tag the render pass includes are written (spec).
- **Unsupported data.** Data with no row above is skipped, and one warning
  per kind is raised, with the first prim (spec).
- **Instances lose their identity.** Native instances arrive as Hydra
  instancers and become `PointInstancer`s. crust's "shared vs placed-once"
  rule for adaptive tessellation then counts placements per instancer, which
  can dice differently from `crust render` of the original stage. The spec's
  equality is with the written layers, which is what the bitwise test pins.

### D4. The viewing camera is a prim in `edits.usda`; the stage's camera dices

`HdRenderPass::_Execute` receives the viewing camera and framing through the
render pass state (`hd/renderPassState.h`, 26.08):
- `GetWorldToViewMatrix()` and `GetProjectionMatrix()` (the framing and
  window policy applied);
- `GetCamera()`, the `HdCamera` sprim (`hd/camera.h`), for its clipping
  range and lens;
- `GetFraming()` (`CameraUtilFraming`), whose data window is the viewport
  size;
- `GetAovBindings()`, the render buffers to fill.

Each execution, `hdCrust` turns the view and projection
matrices into `/__hdCrust/camera`:
- the inverse view matrix becomes `xformOp:transform`;
- `focalLength`, apertures and offsets come from the projection, at the
  `metersPerUnit` the stage reports;
- the clipping range is kept.

The render settings prim `/__hdCrust/settings` names it as `camera`, sets the
framebuffer size as `resolution`, and sets `crust:dicingCamera` to:
- the stage's render camera, when the stage names one, either through
  `HdRenderSettingsSchema` or as the host's scene camera;
- otherwise `/__hdCrust/dicing`, a copy of the viewing camera written once
  at first population.

A drag is thus a `settings` edit (`live-camera-update`): the settings rebuild
runs, with no re-import. Because the dicing camera never moves, adaptive
scenes stay in place.

**Alternative rejected:** a `crust_hydra_set_camera(matrix)` ABI call that
bypasses the layer. It would be faster, but `edits.usda` would no longer
reproduce the image, and the spec's `crust render` equality would break.
The settings rebuild's cost is the trigger for revisiting this (Risks).

### D5. One change at a time, applied between renders

`crust-hydra` owns a session thread (as `crust mcp` does) and the render
thread. `hdCrust`'s observer only collects dirtied paths. Collection is cheap
and runs on Hydra's thread.

Each `_Execute`:

1. If anything changed since the last execution, write one batch to
   `edits.usda`, and call `crust_hydra_apply`. The session thread then:
   1. cancels the running render;
   2. joins it;
   3. diffs, classifies and updates or re-imports;
   4. starts a fresh render with a fresh `RenderControl`.
2. Otherwise, poll `crust_hydra_snapshot`. When its `generation` moved, copy
   the beauty into the color `HdRenderBuffer`.

   After a restart (step 1), `_Execute` instead waits, capped at 2 s, for
   the new render's first full pass (every pixel at 1 spp), and shows that.
   usdview's player redraws once per frame without waiting for
   convergence. Returning at once left the previous frame on screen until
   playback paused (found in usdview). Waiting for a first publish instead
   would show one tile on black. `hdCrustTest --playback` pins it: every
   frame of `animation.usda` shows its own image, about 45 ms per frame at
   320×180.

   Convergence is read *before* the snapshot. Read after, a render
   finishing between the two calls reported converged over its last
   published image, one tile short of the final one (seen as 256 pixels
   off in `compare.sh`).
3. Report `IsConverged` exactly when the render returned `Completed`. At that
   point, read every bound AOV once into its buffer.

Crust renders only the AOVs a RenderProduct asks for. So the viewport's
bindings besides color become one product, `/__hdCrust/aovs`, in
`edits.usda`, with one RenderVar per AOV crust renders. Each var is named
after Hydra's AOV (`driver:parameters:aov:name`):

| Hydra AOV | crust source | type |
|-----------|--------------|------|
| `depth` | `depth`, converted to Hydra's convention (below) | `float` |
| `normal` | `N` | `normal3f` |
| `Neye` | `Neye` | `normal3f` |
| `Peye` | `Peye` | `point3f` |

Any other AOV (`primId`, `instanceId`, ...) is reported once and keeps its
clear value. A change in the bound set is an edit of the override layer,
classified `full` (it adds or removes prims). `crust render -i edits.usda`
writes the same product, so the AOVs are as reproducible as the beauty.
`interaction.sh` pins `Peye` bitwise.

**`depth` is Hydra's normalized device depth, not crust's camera depth.**
The plan copied crust's camera-space depth (5.0, or +inf for a miss) into
the buffer, and every image in usdview turned black (found in usdview).
The host composites the renderer's color with its depth: HgiInterop writes
the depth to `gl_FragDepth` under a depth test (`hgiInterop/opengl.cpp`),
which only [0, 1] can pass. So the copy projects each depth `d` with the
pass's projection, as hdEmbree's `_ComputeDepth` does:
`0.5 · (−d·P₂₂ + P₃₂) / (−d·P₂₃ + P₃₃) + 0.5`, clamped, and misses become
1. `crust render`'s own `depth` AOV stays camera-space. The headless
harness binds no depth unless asked, which is why it missed the black
image. `hdCrustTest --aov depth` now fails on any value outside [0, 1].

Many drag events between two executions collapse into one camera write, so
the rebuild runs at most once per displayed frame.

### D6. The session core moves into `crust-render`'s library

`crust-render` becomes lib + bin:
- `src/lib.rs` holds the CLI's import-and-check policy (`load_scene`,
  `apply_scene_overrides`, `check_scene`, product selection) and the
  `session` module, which holds what was `mcp/{layer,delta,scene,sources}.rs`
  plus the session's import, apply and render core.
- `mcp/` keeps the MCP server: tools, JSON, base64 PNGs, rmcp and tokio.

The features:
- `session` needs openusd only;
- `mcp` implies `session`;
- `crust-hydra` depends on `crust-render` with
  `default-features = false, features = ["session", "jit", "mmap"]`.

This is a move, not a rewrite. Every MCP test keeps passing, and `crust`'s
output does not change (verified by `check_images.sh`).

One seam was added: the session core takes its loader as `Arc<dyn
crust_render::Assets>`, not as `crust-assets`' concrete `FileAssets`.
- `Assets` is `AssetLoader` plus what an import and a kept-decode session
  need: the preloaded-texture count, `revalidate` and `sweep`.
- `FileAssets` is its only production implementation. `crust mcp` and
  `crust-hydra` both use it.
- It exists so a test can bring its own loader. `crust-hydra`'s D8 test
  loads a scene through a loader whose UV textures panic when a material
  samples them.

**Alternatives considered (asked and decided):**
- *A new `crust-session` crate.* It has the cleanest boundary, but it moves
  the whole CLI import policy into a ninth crate.
- *`crust-hydra` driving `crust-core` directly.* That is a second classifier
  and a second import policy that can drift from `crust render`.

### D7. The C ABI: opaque handle, error codes, a hand-written header

`crates/crust-hydra` is a `staticlib` with `#![deny(unsafe_code)]`. The audited
`#[allow(unsafe_code)]` blocks are limited to:
- the `extern "C"` functions' pointer arguments (`CStr::from_ptr`, a mutable
  `slice::from_raw_parts_mut` for buffers);
- `Box::into_raw` / `Box::from_raw` of the handle.

Each block carries a `// SAFETY:` comment, and the count is recorded as
`crust-jit`'s is.

The surface (`include/crust_hydra.h`, `CRUST_HYDRA_ABI_VERSION`):

```
crust_hydra_abi_version() -> u32
crust_hydra_new(session_dir, log_cb, user) -> *Session      // null on error
crust_hydra_free(*Session)
crust_hydra_load(*Session, edits_path) -> Status            // full import, starts a render
crust_hydra_apply(*Session) -> Status                       // edits.usda changed: classify + update
crust_hydra_snapshot(*Session, *f32 rgba, w, h, *generation, *spp) -> Status
crust_hydra_converged(*Session) -> i32
crust_hydra_aov(*Session, name, *f32, w, h, channels) -> Status
crust_hydra_cancel(*Session)
crust_hydra_last_error(*Session, *char, len) -> usize
```

- **Status codes:** `Ok`, `Busy`, `ImportFailed`, `Panicked`, `InvalidArgument`.
- **The header is hand-written** (no cbindgen dependency). The plugin's build
  compiles against it, and a Rust test checks
  `crust_hydra_abi_version()` against the header's constant.
- **Logging.** A `tracing` layer installed by `crust_hydra_new` forwards
  `WARN` and above (and `DEBUG` when the host asks) to `log_cb`, which
  `hdCrust` maps to `TF_WARN` / `TF_STATUS`. Each `Scene::warnings` entry is
  forwarded once per import, with its code and prim.

### D8. Panics: an unwinding profile, caught at every boundary

```toml
[profile.hydra]
inherits = "release"
panic = "unwind"
```

- **Every entry point** runs its body in `catch_unwind`, and returns
  `Panicked` with the message kept for `last_error`.
- **The render and session threads** are joined. A join `Err` is the same
  `Panicked`.
- **After a panic,** the session drops its scene, and the next `apply`
  re-imports from the layers (spec "Failures never take the host down").
  The first poll returns `Panicked`; later ones are `Busy`, and the stopped
  render counts as converged. The host raises an error per failed poll and
  redraws while a render is unconverged, so reporting the panic on every
  poll printed it on every redraw, forever.
- **The panic hook** forwards to `log_cb` instead of printing to stderr.

The `release` profile keeps `abort`. The cost of unwinding is measured once,
with callgrind on cornellbox at 2 spp under each profile, and recorded here.

### D9. Build and CI

`hydra/hdCrust/CMakeLists.txt`:
- `find_package(pxr 26 REQUIRED)`;
- an `add_custom_command` that runs `cargo build --profile hydra -p
  crust-hydra` and imports `libcrust_hydra.a`;
- builds `hdCrust.so` against `hd`, `hdx`, `usdImaging`, `sdf` and `tf`;
- installs `hdCrust/resources/plugInfo.json`.

No Corrosion or other CMake module is added.

`.github/workflows/hydra.yml`:
- **Triggers:** runs on changes to `crates/crust-hydra/`, `hydra/`,
  `crates/crust-render/src/{lib.rs,session/}`, `crates/crust-core/src/live.rs`
  and `scene/usd_import/live.rs`, and nightly.
- **OpenUSD:** `pixi install` in `hydra/` (the same `openusd` package as a
  local build, D9's environment), cached on `pixi.lock`. No OpenUSD build
  runs in CI.
- **Steps:** builds the plugin and runs `hdCrustTest`, a headless C++
  executable:
  1. it builds UsdImaging's scene indices for a sample stage;
  2. it creates a render index on the Crust delegate;
  3. it executes a render pass to completion at 16 spp;
  4. it writes the color buffer to EXR;
  5. it compares it with `crust render` of the written layers through
     `crust diff`.
- **Status:** not a required check.

## Risks / Trade-offs

- [**The 26.x native renderer interface differs from D1's assumptions**] →
  Task 1.1 is a spike before any translation code. Its finding is written
  into D1 before group 3 starts.
- [**Memory: the flattened layer duplicates the geometry**, Hydra's data plus
  crust's import] → v1 accepts it. ALab is measured (peak RSS, with
  `--stats`-equivalent phases forwarded to the log). If it is the wall,
  option (C) (crust builders fed from Hydra values) replaces D2/D3, with
  D3's table as its specification.
- [**openusd misreads a Pixar-written USDC** (a type, a list op, a newer
  crate version)] → Task 4.6 round-trips every `samples/*.usda` through
  `SdfLayer::Export` to USDC and renders both with `crust render`, bitwise
  (`usdc_roundtrip.sh`). Measured with OpenUSD 26.08: 42 of 43 identical.
  The 43rd, `cornellbox_guided`, renders one of two images from run to run
  from its own USDA too (path guiding is not deterministic), so it is no
  USDC difference.
- [**A settings rebuild per drag is too slow on production stages**: it
  re-opens the stage, 18–60 ms on ALab] → D5's coalescing bounds it to once
  per frame. `live-camera-update` records the number. A camera fast path in
  `live` is the follow-up if tumbling stutters.
- [**Snapshot copies at viewport rates**: a full-frame clone under the lock,
  then a copy into the render buffer] → measured at 1920×1080 at usdview's
  draw rate. A region- or tile-dirty snapshot is the follow-up.
- [**Hydra data that USD cannot express back exactly**: instancer flattening
  (D3), light-link categories, implicit-to-mesh filters a host may insert]
  → equality is pinned against the written layers. Differences from
  `crust render` of the original stage are listed in the design record's
  known gaps as they are found.
- [**A panic leaves rayon workers mid-render**] → the render returns through
  the cancelled path only when the panic is on the driving thread. A panic
  on a worker propagates through rayon's join to the driving thread, which
  `catch_unwind` catches. Covered by a test under `[profile.hydra]`
  (`tests_ffi.rs`). Its scene is `usdpreview_textured.usda`, imported
  through a test-only loader whose UV textures panic in `Texture::eval`:
  a material bug, met on a rayon worker mid-render. The test then checks:
  - the C ABI returns `Panicked`;
  - the panic reaches the host's callback;
  - with the texture fixed, the next `apply` re-imports and renders to
    convergence.
- [**`unsafe` and a C++ build enter the project**] → confined to one crate
  and one optional job. `cargo build`, `cargo test --workspace` and the
  required CI never compile `hdCrust`. `crust-hydra` itself compiles and is
  tested with the workspace, through its Rust API.

### Measured (task 5.6)

`hdCrustTest --measure 5` on ALab 2.3 (`entry.usda`, `renderCam`) at
1920×1080, Windows, 16 cores, OpenUSD 26.08, session directory on a separate
disk. The per-redraw costs are the delegate's `crust:syncMs`,
`crust:firstPassMs` and `crust:copyMs` render stats:

| measure | ALab | cornellbox (for scale) |
|---------|------|------------------------|
| stage open (UsdStage) | 0.6 s | 0.04 s |
| first redraw: `scene.usdc` written + crust's import | 57.0 s | 0.07 s |
| … then the first full pass | 2.0 s, capped (the image is partial) | 0.68 s |
| time to first image, from process start | 63.2 s | 0.93 s |
| a drag: settings rebuild | 2.0 s mean, 2.2 s max | 0.11 s |
| a drag: redraw, first pass included | 4.1 s | 0.80 s |
| snapshot copy at 1920×1080, per redraw | 133 ms | 124 ms |
| host process peak memory | 11.1 GiB | 0.38 GiB |
| `crust render` of the same layers: peak working set | 4.1 GiB | — |
| `crust render` of the same layers: import | 20.4 s | — |
| `scene.usdc` on disk | 2.2 GB | 20 KB |

What these say, risk by risk:

- **Memory is the wall the design expected.** The host holds 2.7× what
  `crust render` of the same layers needs: UsdImaging's stage and scene
  indices, the in-memory scene layer the overlay diffs against, and
  crust's import. Dropping the in-memory scene layer (diffing against the
  file instead) is the first saving. Option (C) is the structural one.
- **Disk:** the flattened layer is 2.2 GB. Under the default session
  directory (the system temp, on the system disk) it filled the disk. A
  failed write was then retried on every redraw, until the delegate learned
  to wait for the next change. Production stages need the session
  directory set to a disk with room (documented in the usdview help).
- **Writing the layer costs about twice crust's import of it** (57 s
  against 20 s). The flattened write is the first-image bottleneck, not
  crust.
- **The settings rebuild per drag is 2 s on ALab**, as the risk feared. It
  re-opens the 2.2 GB layer's stage to classify the edit. A camera fast path
  in `live`, or classifying a camera-only `edits.usda` change without
  opening the stage, is the follow-up.
- **The 2 s first-pass cap is reached on ALab**, so a drag shows a partial
  first pass that then fills in.
- **The snapshot copy is about 130 ms per redraw at 1080p** on either scene.
  The image is copied four times: crust's buffer, the RGBA vector, the C
  buffer, and Hydra's, with a per-pixel format switch and a flip. That caps
  progressive refresh near 8 Hz. A region- or tile-dirty snapshot, written
  straight into the host's format, is the follow-up.

## Migration Plan

Additive. The `crust` binary, `crust mcp` and their outputs are unchanged,
which `check_images.sh` and the MCP tests verify after D6's move. Rollback
removes `crates/crust-hydra`, `hydra/` and the workflow. D6's move can stay.

## Open Questions

- The rayon thread count inside a host: whether to expose it as a render
  setting or leave it to `RAYON_NUM_THREADS`. It doesn't change the specs.
- Whether usdview's "Pause"/"Stop" renderer controls map to `cancel` alone or
  also need resume. v1 maps Stop to cancel and treats Pause as Stop.
