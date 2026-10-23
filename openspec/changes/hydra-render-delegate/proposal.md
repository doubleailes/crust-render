## Why

Crust renders USD only through its own CLI and the MCP session. Artists look
at USD in Hydra hosts: usdview first, and later Houdini Solaris, Maya-USD or
Omniverse. A Hydra render delegate lets them pick "Crust" in the viewport's
renderer menu, tumble the camera and watch the path tracer refine. They see
exactly the image `crust render` would produce from the same scene.

OpenUSD 26 makes the Hydra 2.0 scene index the default data path, and
delegates written for the older scene-delegate path run behind an adapter
that is being deprecated. A delegate built now should observe the scene
index.

## What Changes

- **`hdCrust`**: a C++ Hydra renderer plugin for OpenUSD 26, built with CMake
  outside `cargo build`. It registers a renderer named "Crust", observes the
  terminal scene index, and turns what it sees into USD:
  - **First population**: a flattened USD layer. Meshes, curves, points and
    implicit shapes with world transforms; materials as `Material`/`Shader`
    networks with direct bindings; UsdLux lights; instancers as
    `PointInstancer`s; render settings, products and vars from the viewport's
    AOV bindings.
  - **Each dirtied prim**: opinions on an override layer.
  - **The free camera**: the render camera. The stage's own render camera
    becomes `crust:dicingCamera`, or the free camera at first load when the
    stage has none.
  - **Output**: the progressive beauty is shown in the viewport's render
    buffers, and the AOVs when a render completes.
- **`crust-hydra`**: a new Rust crate, a `staticlib` with a C ABI that
  `hdCrust` links. Opaque handles take layer bytes, overrides, camera and
  viewport changes, and render, snapshot, cancel and AOV-read calls, all
  returning error codes. It is `deny(unsafe_code)` with audited blocks, like
  `crust-jit`, and every entry point catches panics.
- **`[profile.hydra]`**: inherits `release` with `panic = "unwind"`, so a crust
  panic becomes an error the host can report instead of aborting usdview. The
  CLI's `release` profile keeps `panic = "abort"`, so its goldens and
  instruction counts do not move.
- **`crust-render` gains a library target.** A `session` feature (openusd, no
  MCP SDK, no async runtime) holds the session core and the CLI's
  import-and-check policy: `load_scene`, `apply_scene_overrides`,
  `check_scene`, product selection, layer diff, `classify`, in-place
  `update`, kept assets. `crust mcp` and `crust-hydra` both use it, so a
  scene seen through the delegate is the scene `crust render` imports from
  the same layers, and an edit classifies the same in both hosts. There is no
  behaviour change for the `crust` binary.
- **CI**: a new optional workflow job builds OpenUSD 26 (cached), `hdCrust`
  and `crust-hydra`, then runs the delegate's tests headless. The four
  existing jobs are unchanged.
- **Depends on `live-camera-update`**: a free camera move is an in-place
  `settings` update, and tessellation is diced for the stage's camera.

## Capabilities

### New Capabilities

- `hydra-delegate`: crust as a Hydra 2.0 renderer. What a Hydra host sees:
  - registration;
  - which scene it renders, and that it equals `crust render` of the
    flattened layers;
  - how edits, camera moves and time changes apply;
  - progressive display, AOVs, convergence and cancellation;
  - render settings exposed to the host;
  - errors and panics surfaced without killing the host;
  - the data it does not carry yet, reported as warnings.

### Modified Capabilities

None. `crust mcp` keeps its behaviour. The move of its core into
`crust-render`'s library is internal, and the spec-level camera change
belongs to `live-camera-update`.

## Impact

- **New code**:
  - `crates/crust-hydra/` (Rust, C ABI, a hand-written header
    `include/crust_hydra.h`);
  - `hydra/hdCrust/` (C++, CMake, `plugInfo.json`), outside the Cargo
    workspace.
- **`crust-render`**:
  - `src/lib.rs` (new), with `main.rs` thinned to argument parsing and exit
    codes;
  - `mcp/` split into `session` (lib) and the MCP server (`mcp` feature);
  - `Cargo.toml` features `session` and `mcp`, with `mcp` implying
    `session`.
- **Workspace `Cargo.toml`**: `[profile.hydra]`. The comment "no
  `catch_unwind` anywhere in the workspace" becomes "outside `crust-hydra`".
- **`unsafe`**: `crust-hydra` is the second crate with audited `unsafe`
  blocks, and adding it is the project decision `CLAUDE.md` asks for. No
  `unsafe` enters crust-core or the hot path.
- **Build dependency**: Pixar's C++ OpenUSD 26, for `hdCrust` and the
  optional CI job only. `cargo build` and the four required CI jobs do not
  need it.
- **Performance**:
  - **The CLI renders exactly as before.** Its throughput is unchanged
    (same profile, same code path).
  - **The `hydra` profile is slower.** It pays unwinding landing pads,
    measured once with callgrind on cornellbox at 2 spp and recorded in the
    design.
  - **Memory**: the flattened layer duplicates the scene's geometry in the
    host process while crust imports it.
- **Docs**:
  - `site/content/docs/help/usdview.md` (new): build, install, use.
  - `site/content/docs/architecture/` (the delegate).
  - `docs/architecture.md`: crate table and seams.
  - `CLAUDE.md`: the workspace line, `unsafe` rules, the commands.
  - `openspec/specs/cli/design.md`: a cookbook recipe for building and
    running `hdCrust` locally.
  - The known gaps "No Hydra render delegate" in the `usd-scene-import` and
    `mcp-session` design records are retired.
