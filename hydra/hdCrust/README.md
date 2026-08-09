# hdCrust — Crust Render as a Hydra render delegate

A C++ `HdRenderDelegate` plugin that lets Hydra hosts (usdview, Solaris,
Maya, …) render their viewport through Crust Render's CPU path tracer, via
the `crust-capi` C ABI (`crates/crust-capi/include/crust.h`).

This is the **Phase 1 MVP** of `docs/hydra_delegate.md`:

- Rprims: meshes (triangulated via `HdMeshUtil`; vertex normals honored).
- Instancing: point instancers flatten to one crust mesh per placement
  (real kernel instancing through the C API is the Phase 2 seam).
- Lights: sphere, rect, distant, dome — UsdLux `intensity`/`exposure`/
  `color` respected; **dome textures render as the uniform color** for now.
- Shading: **displayColor only** — materials are accepted and ignored
  (`HdMaterialNetwork` → OpenPBR translation is Phase 2).
- AOVs: color (float32/float16/unorm8 RGBA), depth, primId (picking works).
- Edits: **any** change (camera orbit included) rebuilds the whole crust
  scene — correct, not yet fast. Progressive refinement between edits:
  every Hydra `Execute` adds a few samples until the budget converges.

## Building

Requirements: an OpenUSD C++ build with imaging enabled (GL not required —
a `build_usd.py --no-python --imaging` build is enough), CMake ≥ 3.20, a
C++17 compiler, and the crust-capi cdylib.

```bash
# 1. The Rust side (from the repository root)
cargo build --release -p crust-capi

# 2. The plugin
cmake -S hydra/hdCrust -B build/hdCrust \
      -DCMAKE_PREFIX_PATH=$USD_ROOT \
      -DCRUST_CAPI_DIR=$PWD/target/release \
      -DCMAKE_INSTALL_PREFIX=$PWD/install
cmake --build build/hdCrust --target install
```

## Running

```bash
export PXR_PLUGINPATH_NAME=$PWD/install/plugin/usd/hdCrust/resources:$PXR_PLUGINPATH_NAME
export LD_LIBRARY_PATH=$PWD/target/release:$USD_ROOT/lib:$LD_LIBRARY_PATH
usdview samples/cornellbox.usda   # then View > Renderer > Crust
```

Environment knobs (all `TF_ENV_SETTING`s):

| variable | default | meaning |
| --- | --- | --- |
| `HDCRUST_SAMPLES_PER_PIXEL` | 64 | total per-pixel budget (convergence target) |
| `HDCRUST_SAMPLES_PER_STEP` | 4 | samples added per Hydra `Execute` call |
| `HDCRUST_MAX_DEPTH` | 8 | path length bound |

## Design notes

- The renderer is driven **synchronously**: one `crust_renderer_step` per
  `_Execute`, and Hydra's own convergence loop (usdview re-executes until
  `IsConverged()`) provides progressive refinement. A background
  `HdRenderThread` wrapper can be added later without touching the ABI —
  cancellation already crosses threads through `CrustStopToken`.
- Framebuffer rows are bottom-up on both sides (crust's native layout and
  Hydra/GL's), so color/AOV copies are straight `y*width+x` loops.
- Matrices pass from `GfMatrix4d::GetArray()` into the C API **without
  transposition**: USD's row-major row-vector storage equals the
  column-major column-vector layout crust expects, element for element.
- Depth converts crust's camera-forward distance to `[0,1]` NDC depth
  through the projection matrix in the render pass.
- primId: the pass maps crust's per-geometry ids back to Hydra rprim ids,
  so viewport picking resolves to the right prim even through flattened
  instancing.
