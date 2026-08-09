# Hydra render delegate (hdCrust): decision record and roadmap

Status: **Phase 0 in progress** (engine groundwork). Phases 1–2 are planned, not started.

## The decision

Crust Render will grow a USD **Hydra render delegate** — the plugin form
(`HdRenderDelegate`) that lets usdview, Houdini/Solaris, Maya and other Hydra hosts
render their viewport through crust. To make that possible, the project's safe-Rust
constraint is amended with exactly **one sanctioned exception**:

> All engine crates (`crust-rt`, `crust-core`, `crust-render`, `utils`) remain 100%
> safe Rust, enforced by `forbid(unsafe_code)` in each crate root (test-only
> carve-outs excepted). The **Hydra delegate boundary** — a future `crust-capi`
> C-ABI crate and the `hdCrust` C++ plugin — may use `unsafe`/FFI. No engine crate
> may ever depend on a boundary crate, so the exception cannot leak inward.

## Why the exception is unavoidable (and why `openusd` is not the answer)

- `HdRenderDelegate` is a **C++ plugin ABI** in OpenUSD's imaging stack
  (`pxr/imaging/hd`). Hosts load delegates as C++ plugins (`plugInfo.json`) against
  *their own* OpenUSD build. No pure-Rust crate can be that plugin; the boundary is
  C++ by definition.
- The pure-Rust [`openusd`](https://github.com/mxpv/openusd) crate covers the data
  model (`sdf`/`pcp`/`usd` + schemas) and has no imaging half — and doesn't need
  one, because **a delegate never parses USD**. Hydra hands it already-composed
  scene data through `HdSceneDelegate`/scene indexes (`GetMeshTopology()`, points,
  `HdMaterialNetwork2`, …). The importer (`scene/usd_import.rs`) is therefore
  *bypassed* by the delegate path, not extended; it keeps serving the CLI.
- What a delegate drives instead is crust-core's programmatic API, which is already
  public and openusd-free: `WorldBuilder::attach/commit`, the `Material` trait,
  `LightList`, `Camera`, `Renderer` (`rt_world.rs`, `lib.rs` re-exports).

## Roadmap

### Phase 0 — engine groundwork (pure Rust, CLI-testable; this phase)

Everything the delegate needs that requires no FFI, gated by the repo's
golden-image discipline (`scripts/check_images.sh` — batch renders stay
bit-identical):

- **Raw framebuffer access**: `Buffer::as_slice` / `rows_top_down` so a host can
  copy rows without per-pixel calls.
- **Matrix camera**: `Camera::from_view_projection(view, proj, aperture, focus)` —
  Hydra hands matrices, including off-axis frustums, not lookat/vfov.
- **AOVs**: depth / world normal / `[geom_id, prim_id]` / alpha via a per-pixel
  primary-hit probe pass (`Renderer::render_aovs`) — ids point-sampled at pixel
  centers, which is what Hydra picking requires. CLI: `--aovs` writes EXR sidecars.
- **Cancellation**: `StopToken` checked per row/tile;
  `Renderer::render_with_control`. A stopped render is a coherent partial image.
- **Progressive rendering**: `Renderer::begin_progressive` → `ProgressiveRender`
  (`step(spp)` / `snapshot()` / `finish()`), persisting per-pixel accumulation so a
  progressive render run to completion is **bit-identical** to the batch render
  (the QMC sampler is stateless per sample index and the adaptive-stop predicate is
  a pure function of the persisted accumulators). CLI: `--progressive`, `--preview`,
  `--time-limit`.

### Phase 1 — `crust-capi` + `hdCrust` MVP

- `crates/crust-capi`: `cdylib` exposing scene build (mesh/camera/light/material
  subset), render start/stop, framebuffer + AOV reads over a C ABI (cbindgen).
  This is where `unsafe` first appears — `extern "C"` exports and raw-pointer
  buffer views, nothing else.
- `hydra/hdCrust/`: C++ plugin — `HdRenderDelegate`, `HdRenderPass`,
  `HdRenderBuffer`, `HdMesh`/`HdCamera`/light adapters, `plugInfo.json`, CMake
  against a host OpenUSD build. MVP semantics: rebuild the whole crust scene on any
  dirty bit; color + depth AOVs; verify in usdview.
- Note: this repo's CI container has no OpenUSD C++ build; the C++ side is
  compile-verified against a local USD install, documented in the plugin's README.

### Phase 2 — incrementality and materials

- Retained scene: per-geometry replace, transform-only refit, `Arc<dyn Material>`
  slot swap — so camera orbits and material tweaks stop paying a full SBVH rebuild.
- `HdMaterialNetwork2` → OpenPBR translation (transplant the existing
  UsdPreviewSurface mapping out of `usd_import.rs`'s USD-prim reads).
- Instancer sync, light edits, render-settings sync.

## Non-goals

- No GPU backend (unchanged project constraint).
- No Storm/Hgi integration — hdCrust is a CPU renderer plugin presenting AOV
  buffers, like hdEmbree.
- The kernel stays safe Rust; Embree bindings remain a hypothetical discussed in
  `docs/embree_comparison.md`, not part of this roadmap.
