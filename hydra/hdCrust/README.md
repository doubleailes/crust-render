# hdCrust — Crust Render as a Hydra render delegate

A C++ `HdRenderDelegate` plugin that lets Hydra hosts (usdview, Solaris,
Maya, …) render their viewport through Crust Render's CPU path tracer, via
the `crust-capi` C ABI (`crates/crust-capi/include/crust.h`).

This is **Phase 2** of `docs/hydra_delegate.md`:

- Rprims: meshes (triangulated via `HdMeshUtil`; vertex normals honored).
- Instancing: point instancers place a **shared kernel prototype** per
  instance (one committed BVH per mesh, N placements).
- Lights: sphere, rect, distant, dome — UsdLux `intensity`/`exposure`/
  `color` respected; **dome textures load** (.exr/.hdr linear, LDR
  sRGB→linear) with uniform-color fallback on decode failure.
- Shading: **UsdPreviewSurface constants** via `HdMaterialNetwork`
  (diffuseColor, metallic, roughness, ior, opacity, clearcoat,
  clearcoatRoughness, emissiveColor — same mapping as crust's USD
  importer); texture-connected inputs warn and use defaults; unbound
  meshes shade from displayColor.
- AOVs: color (float32/float16/unorm8 RGBA), depth, primId (picking works).
- Edits are cheap: a **camera orbit restarts sampling with zero scene
  work** (`crust_renderer_update_camera`); geometry/material/light edits
  rebuild only the top-level BVH over instance bounds — mesh triangles and
  their inner BVHs live in a cross-rebuild prototype cache keyed by prim
  path + geometry version. Progressive refinement between edits: every
  Hydra `Execute` adds a few samples until the budget converges.

## Building

Requirements on every platform:

- An **OpenUSD C++ build with imaging enabled** (GL not required — a
  `build_usd.py --no-python --imaging` build is enough). The plugin must be
  compiled with the **same compiler family and C++ runtime as that USD
  build** — USD's C++ ABI is not stable across toolchains.
- **CMake ≥ 3.20** and a **C++17 compiler**.
- A **Rust toolchain** (rustc ≥ 1.85, the edition-2024 floor) for the
  crust-capi library the plugin links against.

`$USD_ROOT` / `%USD_ROOT%` below is the USD install prefix (the directory
containing `pxrConfig.cmake`).

### Linux (and macOS)

Any recent gcc or clang works (match the one that built USD).

```bash
# 1. The Rust side (from the repository root) — produces
#    target/release/libcrust_capi.so
cargo build --release -p crust-capi

# 2. The plugin
cmake -S hydra/hdCrust -B build/hdCrust \
      -DCMAKE_PREFIX_PATH=$USD_ROOT \
      -DCRUST_CAPI_DIR=$PWD/target/release \
      -DCMAKE_INSTALL_PREFIX=$PWD/install
cmake --build build/hdCrust --target install
```

### Windows

Use **MSVC** (the toolchain USD requires on Windows) from an *x64 Native
Tools Command Prompt for VS*, and the MSVC Rust toolchain
(`x86_64-pc-windows-msvc`, rustup's default on Windows). Cargo's cdylib
produces `crust_capi.dll` plus its import library `crust_capi.dll.lib`;
CMake links the import library (preferred automatically over the
also-produced static `crust_capi.lib`) and the DLL is loaded at run time.

```bat
:: 1. The Rust side (from the repository root) — produces
::    target\release\crust_capi.dll (+ .dll.lib)
cargo build --release -p crust-capi

:: 2. The plugin (multi-config generator: pick Release at build time)
cmake -S hydra\hdCrust -B build\hdCrust ^
      -DCMAKE_PREFIX_PATH=%USD_ROOT% ^
      -DCRUST_CAPI_DIR=%CD%\target\release ^
      -DCMAKE_INSTALL_PREFIX=%CD%\install
cmake --build build\hdCrust --config Release --target install
```

Notes:
- `plugInfo.json` is configured by CMake with the platform's library file
  name (`libhdCrust.so` / `.dylib` / `.dll`), so the same source tree
  builds everywhere.
- The C smoke test (`scripts/test_capi_c.sh`) is a bash script; on Windows
  run the Rust-side twin instead: `cargo test -p crust-capi`.

## Running

Linux/macOS:

```bash
export PXR_PLUGINPATH_NAME=$PWD/install/plugin/usd/hdCrust/resources:$PXR_PLUGINPATH_NAME
export LD_LIBRARY_PATH=$PWD/target/release:$USD_ROOT/lib:$LD_LIBRARY_PATH
usdview samples/cornellbox.usda   # then View > Renderer > Crust
```

Windows (DLL resolution goes through `PATH` — it must reach both
`crust_capi.dll` and USD's own DLLs):

```bat
set PXR_PLUGINPATH_NAME=%CD%\install\plugin\usd\hdCrust\resources;%PXR_PLUGINPATH_NAME%
set PATH=%CD%\target\release;%USD_ROOT%\lib;%USD_ROOT%\bin;%PATH%
usdview samples\cornellbox.usda   :: then View > Renderer > Crust
```

Only the Linux path is exercised by this repository's checks (the headless
harness below runs against a from-source Linux USD build); the Windows
instructions follow USD's standard plugin conventions but are not covered
by CI — please report anything that doesn't hold.

The headless harness (`-DHDCRUST_BUILD_TESTS=ON` → `testHdCrust`, run with
the same two environment variables) covers what usdview would exercise:
registry discovery, a converging beauty render, material bind + edit,
transform / camera / instancer edits re-converging.

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
