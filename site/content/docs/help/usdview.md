+++
title = "Rendering in usdview"
description = "Pick Crust in usdview's renderer menu: hdCrust, the Hydra render delegate, renders the viewport with crust and shows the image as it refines."
date = 2026-10-23T08:00:00+00:00
updated = 2026-10-23T08:00:00+00:00
draft = false
weight = 35
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = '<code>hdCrust</code> is a Hydra render delegate. Pick <strong>Crust</strong> in usdview’s renderer menu: the viewport is path traced by crust, refining while you look. The delegate writes what usdview shows as two USD layers, which <code>crust render</code> renders to the same image.'
toc = true
top = false
+++

## What it is

usdview, like other Hydra hosts, draws a stage through a *render delegate*, a plugin
it loads at startup. `hdCrust` is crust's. The delegate does not hand crust the scene
in memory. It writes what Hydra presents as two USD layers in a *session directory*:

| file | holds |
|------|-------|
| `scene.usdc` | the stage, flattened: every mesh, curve, implicit shape, light, material and camera at its world transform, instancers as `PointInstancer`s, composition and variants already resolved |
| `edits.usda` | `subLayers = [@./scene.usdc@]`, the viewport camera, the render settings, and every later change as an `over` |

crust renders `edits.usda` exactly as `crust render -i edits.usda` would. Set a session
directory (below) to keep the layers, and the CLI reproduces the viewport.

The delegate is `hydra/hdCrust` in the repository, a C++ plugin built with CMake
against OpenUSD 26. It links crust itself as a static library (`crates/crust-hydra`).
It is built separately from `cargo build`.

## Building it

The plugin must be built against the OpenUSD the host runs. The repository pins both
in one [pixi](https://pixi.sh) environment, `hydra/pixi.toml`. That is the `openusd`
26.08 package, with usdview, from
[Anders Langlands' ASWF channel](https://github.com/anderslanglands/aswf-pixi), on
Linux and Windows. You also need Rust (the toolchain in `rust-toolchain.toml`) and a
C++17 compiler: GCC or Clang on Linux, Visual Studio 2022 Build Tools on Windows.

```bash
cd hydra
pixi install                                    # OpenUSD 26.08 + usdview + CMake + Ninja
pixi run cmake -S hdCrust -B build -G Ninja -DCMAKE_BUILD_TYPE=Release
pixi run cmake --build build                    # also runs: cargo build --profile hydra -p crust-hydra
pixi run cmake --install build --prefix install
```

On Windows, run these from a Visual Studio *x64 Native Tools* prompt (or after
`vcvars64.bat`), so CMake finds MSVC. The first build compiles crust under the `hydra`
profile, which takes several minutes. Later builds are quick.

The install puts the plugin where OpenUSD's own plugins live:

```text
install/plugin/usd/hdCrust.dll (or hdCrust.so)
install/plugin/usd/hdCrust/resources/plugInfo.json
install/bin/hdCrustTest            the headless test harness
```

To build against your own OpenUSD 26 instead, point `pxr_DIR` at the directory that
holds its `pxrConfig.cmake`. Run usdview from that same build.

## Running usdview

```bash
cd hydra
export PXR_PLUGINPATH_NAME=$PWD/install/plugin/usd/hdCrust/resources   # PowerShell: $env:PXR_PLUGINPATH_NAME = "$PWD\install\plugin\usd\hdCrust\resources"
pixi run usdview ../samples/cornellbox.usda --renderer Crust
```

You can also leave out `--renderer` and pick **Crust** from the renderer menu.

Open the stage's `.usda` (or `.usdc` / `.usdz`), not a `.mtlx`. usdview opens a
MaterialX document as a stage of material definitions, with no geometry, lights or
camera, and that renders black.

## What happens as you work

Every change becomes an edit of `edits.usda`. crust applies it the way a
[Claude Desktop session](@/docs/help/claude-desktop.md) applies one, in place when it
can:

| you… | crust… |
|------|--------|
| tumble, pan, zoom, resize the viewport | rebuilds the camera and settings only, and restarts the render |
| change a renderer setting | the same |
| change a light's inputs (intensity, colour, texture…) | rebuilds the lights in place |
| change a material's shader inputs | rebuilds the materials in place |
| move a mesh, sphere or light, reshape a mesh placed once, resize an area light | places that geometry again and rebuilds the top-level BVH |
| add, remove or hide a prim, change a binding, move anything else | imports the scene again |
| move the time slider | rewrites both layers at the new frame and imports them |

After every restart, the viewport waits for the first full pass (every pixel one
sample, at most two seconds) before showing it. A tumble or a frame of playback
therefore shows a whole noisy image of its own. The image then refines while nothing
changes, and the render reports *converged* when it completes. Playback shows each
frame's first pass. On a large viewport or a heavy stage that is not real time.

## Renderer settings

The renderer settings in usdview's Hydra settings dialog are crust's:

| setting | written as | default |
|---------|-----------|---------|
| Samples per pixel | [`crust:samplesPerPixel`](@/docs/usd/render-settings.md) | 128 |
| Minimum samples per pixel | `crust:minSamplesPerPixel` | 32 |
| Variance threshold | `crust:varianceThreshold` | 0.05 |
| Max depth | `crust:maxDepth` | 32 |
| Indirect clamp | `crust:indirectClamp` | 10 |
| Sampling strategy | `crust:samplingStrategy` | `power` |
| Sampler | [`crust:sampler`](@/docs/usd/render-settings.md#crust-sampler) | `pmjBn` (a final render's is `sobol`) |
| Session directory (kept when set) | (not written) | a temporary directory |

The stage's own RenderSettings come first, those its `renderSettingsPrimPath` names
or else `/Render/settings`. A setting you change in the dialog overrides only that
one, and a setting you never touch keeps the stage's value. Every value is written
as the `crust:*` attribute on `/__hdCrust/settings` in `edits.usda`.

**Sampler.** The viewport draws from PMJ with blue noise across pixels (`pmjBn`)
unless the stage authors `crust:sampler`. At the few samples per pixel a viewport
mostly shows, its noise reads as a finer grain than Sobol's. `crust render` of the
session's `edits.usda` uses the same sampler, since the delegate writes it there.

**Session directory.** Unset, the layers go to a temporary directory removed with the
viewport. Set it, and they are written there and kept:

```bash
cargo run --release -- render -i <session dir>/edits.usda
```

renders the viewport's last state, at the viewport's resolution and camera. For a
16-spp render, `crust render -s 16` of the layers gives the image the delegate showed
at 16 spp, bit for bit.

## The dicing camera

Adaptive subdivision and the segments of curved implicit shapes are diced for a
camera. The delegate dices for the stage's render camera, the one its RenderSettings
name. When the stage names none, it dices for the viewport camera as it was when the
stage was loaded, or, if you deactivate the render camera, as it was then. Either way the dicing camera does not move when you tumble, so a
camera move never re-tessellates and never re-imports. Seen much closer than the
dicing camera, diced geometry can look coarse. See
[`crust:dicingCamera`](@/docs/usd/render-settings.md#crust-dicingcamera).

## AOVs

The beauty fills the viewport's colour as the render refines. Other AOVs fill their
buffers once the render completes:

| usdview AOV | crust |
|-------------|-------|
| `depth` | `depth`, converted to Hydra's normalized depth in [0, 1] (what usdview composites with) |
| `normal` | `N` |
| `Neye`, `Peye` | `Neye`, `Peye` |

Any other AOV (`primId`, `instanceId`, …) is reported once and stays at its clear
value.

## crust's own attributes

Hydra carries schema data and primvars, but not arbitrary attributes. So hdCrust also
registers a small UsdImaging adapter that carries every `crust:*` attribute of every
prim (`crust:volume:*` fog and smoke, `crust:light:*`, `crust:rayMask`,
`crust:subdivisionLevel`, …) to the delegate, which writes them back. A stage that
renders a homogeneous fog with `crust render` renders it in usdview too.

## What it does not carry yet

Each of these is reported once in usdview's console, naming the first prim, and the
rest of the scene renders:

- **Light linking.** A linked light lights everything.
- **Volume fields** (UsdVol `Volume`, OpenVDB). `crust:volume:*` volumes are carried.
- **Motion over the shutter.** The frame's values are rendered, with no motion blur.
- **Points** (`UsdGeomPoints`). crust does not render them.
- **Sheared instance transforms.** A `PointInstancer` stores position, orientation and
  scale only.
- **Any other prim type** crust does not read.

crust's own import warnings reach the console too, once per import, with their code
and prim (`crust: [material.fallback_default] …`). A panic inside crust is reported
once, as an error. usdview keeps running and stops refining, and the next change
renders again.

## Troubleshooting

- **"Crust" is not in the renderer menu.** `PXR_PLUGINPATH_NAME` must point at the
  `resources` directory that holds `plugInfo.json`, and usdview must be pixi's (the
  OpenUSD the plugin was built against).
- **The viewport is black.** Check that you opened the stage, not its `.mtlx`, and
  that the stage has lights: crust ignores usdview's default camera light. Set a
  session directory and render `edits.usda` with the CLI. If that is black too, the
  stage is.
- **The scene looks simpler than `crust render` of it.** usdview draws the
  `proxy` purpose by default, not `render`: production assets show their
  stand-in geometry, with their preview materials. Turn on **Display ▸
  Display Purposes ▸ Render** (and off Proxy) to see what a final render
  shows; `usdrecord` takes `--purposes render`.
- **A production stage fills the disk.** The flattened `scene.usdc` is about as large
  as the stage's geometry: ALab's is 2.2 GB. Unset, the session directory is in the
  system's temporary directory. For a large stage, start usdview with that pointed at
  a disk with room. That is `TMP` on Windows (PowerShell: `$env:TMP = "D:\tmp"`) and
  `TMPDIR` on Linux. Or set "Session directory" in the settings dialog. When the disk
  fills, the viewport stays black and the console shows `Failed writing usdc data`. A write that
  fails is reported and retried at the next change, not on every redraw.
- **A rebuild changes nothing.** usdview keeps the plugin it loaded. Close every
  usdview window before installing a rebuilt plugin. On Windows the install fails on
  a plugin that is still loaded.
- **References with absolute paths** from another machine (for example
  `materialx_showcase.usda`'s teapot and lion) do not resolve. usdview and crust both
  leave them out.

## Testing it

`hydra/hdCrust/test` holds three scripts. Each runs in the pixi environment after an
install:

```bash
cd hydra
pixi run bash hdCrust/test/compare.sh install /tmp/compare          # 8 samples: delegate vs `crust render`, bitwise; and vs the original stage
pixi run bash hdCrust/test/interaction.sh install /tmp/interact     # camera, edits (hide/show, remove/restore), time, playback, settings, AOVs, layer contents
pixi run bash hdCrust/test/usdc_roundtrip.sh /tmp/roundtrip         # every sample through Pixar-written USDC
```

They drive `hdCrustTest`, which renders a stage through the delegate the way usdview
does, without a window.
