## Why

Crust writes one image, an RGB EXR of the beauty pass, plus a PNG preview. It has
no AOVs (arbitrary output variables, or render passes). That rules out the
production uses a path tracer is normally put to: compositing light passes,
denoising (which needs albedo and normal), depth and position passes, and ID
mattes.

USD already defines how a scene asks for AOVs, in the `UsdRender` schema:

- a `RenderSettings` prim lists `RenderProduct`s (output files);
- each product lists `RenderVar`s (channels) in order;
- each var names what to compute (`sourceName`), how to interpret that name
  (`sourceType`: `raw` / `primvar` / `lpe` / `intrinsic`), and its type
  (`dataType`).

Crust reads only `resolution` and `camera` from that schema today and ignores
the rest silently. Scenes exported from Houdini/Solaris, or written for
usdrecord or hdPrman, author products and vars that crust drops without a
warning. The `scripts/material_fidelity` fixtures already author a
`RenderProduct` that crust skips.

The schema standardises the *plumbing* (settings → product → var, and how
products override settings) but deliberately not the *vocabulary*. It says
"USD does not yet enforce a set of universal RenderVar names", and leaves
`intrinsic` unimplemented. So this change has two jobs:

1. implement the plumbing faithfully;
2. choose and document which vocabulary crust honours.

`design.md` is the full evaluation behind that choice.

## What Changes

Phases are listed in the order they can land. Each phase ships on its own.

- **Phase 1: products, vars and geometric AOVs.**
  - Resolve `RenderSettings.products` → `RenderProduct` → `orderedVars` →
    `RenderVar`. This mirrors `UsdRenderComputeSpec`: a product overrides only
    the settings it authors. Values are evaluated at the render's time code, so
    a time-sampled `productName` gives per-frame file names.
  - Write one EXR per `raster` product to its `productName`, creating parent
    directories. The EXR is single-part with named channels, as follows:
    - colour AOVs use `R/G/B/A`;
    - vector data uses `X/Y/Z`;
    - scalars use one channel named after the AOV;
    - precision follows `dataType`, or `driver:parameters:aov:format` when
      authored;
    - the header carries `colorInteropID` and the software name.
  - First AOVs, all available from the camera ray's first hit and the existing
    film:
    - beauty (`color`), with alpha (coverage) when the var is a 4-channel type;
    - camera depth;
    - world and camera position;
    - world and camera shading normal;
    - `st` / UV;
    - per-pixel sample count;
    - per-pixel variance.
  - Two accumulation modes:
    - **filtered**: the beauty's own pixel-filter weights. This is the default
      for colour, normal and UV.
    - **closest**: the value of the nearest sample to the camera. This is the
      default for depth, position and IDs.

    Mapped from `driver:parameters:aov:multiSampled` and the Arnold, Karma and
    RenderMan filter attributes.
  - Unknown sources, `sourceType = "intrinsic"`, `deepRaster` products, and a
    product whose camera or resolution differs from the render's are refused
    with one `WARN` each. A refused var gets no channel, rather than a black
    channel that looks valid.
  - `-o/--output` becomes an override of the first product's path, as husk
    does. With no products authored, output is byte-for-byte today's: the same
    EXR at `-o`, and the same PNG beside it.
- **Phase 2: lobe-aware AOVs and light path expressions.**
  - `sourceType = "lpe"` with the OSL Light Path Expression grammar.
  - An event alphabet for crust:
    - `C`, `R`/`T`, `D`/`G`/`S`/`s`, `V`, `L` (light-list entries), `O`
      (emissive geometry outside the light list);
    - lobe labels named after OpenPBR (`'diffuse'`, `'specular'`, `'coat'`,
      `'sheen'`, `'transmission'`, `'subsurface'`).
  - Every LPE var is compiled into one shared DFA.
  - Each contribution is routed by splitting the bounce and NEE values per
    lobe. This is exact and unbiased under crust's one-sample-mixture BSDF
    estimator; labelling a bounce by the lobe that was picked is not (see
    `design.md` D11).
  - A first-non-delta-hit `albedo` AOV, for denoisers.
  - Light groups via `crust:light:lpeTag`, used as `<L.'tag'>`.
- **Phase 3: identity.**
  - Keep a `geom_id` → prim-path table past import.
  - `primId` / `instanceId` AOVs.
  - Cryptomatte v1.2 (`crypto_object`, `crypto_material`, `crypto_asset`)
    with in-header manifests.
  - `sourceType = "primvar"` for primvars kept at import because a var asked
    for them.
- **Guarantees across all phases:**
  - With no products, or only a beauty var, the render is bit-identical and
    costs the same instructions (callgrind, cornellbox).
  - The beauty never changes because an AOV was requested.
  - The full-path LPE `C.*[LO]` reproduces the beauty bit-for-bit.
  - A partition of LPEs sums to the beauty, within floating-point tolerance.

## Capabilities

### New Capabilities

- `aovs`: what each supported RenderVar source means in crust. This covers:
  - the canonical names and aliases;
  - definitions, spaces, units and clear values;
  - accumulation modes;
  - the LPE grammar and event alphabet;
  - light groups and Cryptomatte;
  - the guarantees that tie AOVs to the beauty.

### Modified Capabilities

- `usd-scene-import`: "Render settings from USD with defaults" now also reads
  `products`. A new requirement resolves `RenderProduct` and `RenderVar`.
- `image-output`: "EXR output" becomes per-product, multi-channel EXRs. New
  requirements cover channel naming, precision, colour tagging and directory
  creation. The PNG preview follows the first product's beauty.
- `cli`: `-o/--output` overrides the first product's path. Its default,
  `output.exr`, applies only when the stage authors no product.

## Impact

- **Code**
  - `crust-core/src/scene/usd_import/settings.rs`: product and var resolution.
  - `crust-core/src/tracer/{mod,path,settings}.rs`:
    - an `AovRequest` on `RenderSettings`;
    - per-unit AOV accumulators beside `PixelState`;
    - `trace_path` monomorphised on an AOV const-generic, as it already is on
      `PROFILE`;
    - a masked backward gather.
  - `crust-core/src/buffer.rs`: a multi-channel film type returned beside
    `Buffer`.
  - `crust-core/src/material/`: per-lobe evaluation and an LPE label per lobe
    (Phase 2). This touches OpenPBR `lobes.rs` and `closure/mod.rs`.
  - `crust-core/src/lpe/`: a new module for the parser and DFA (Phase 2).
  - `crust-render/src/main.rs`: an `exr` `AnyChannels` writer, following
    `crust-assets/src/tiled/exr_write.rs`.
  - `examples/exr_diff`: learns to diff named layers.
- **Public API**
  - The `render*` entry points gain an AOV-returning sibling. The existing
    ones keep their signatures.
- **Performance**
  - None when no AOV is requested; pinned by an instruction-count check.
  - With AOVs, the cost scales with the number of vars and LPE DFA states.
    Measured with `bench_ab.sh` and callgrind, and recorded in the `aovs`
    design record.
  - Memory: one full-frame f32 plane per channel. For example, 4K × 30
    channels ≈ 1 GB. Cryptomatte adds per-pixel ID lists during accumulation.
- **Docs**
  - A new `openspec/specs/aovs/` (spec and design) and
    `openspec/specs/image-output/design.md`.
  - A `site/content/docs/usd/aovs.md` page.
  - Updates to `usd/render-settings.md`, `reference/command-line.md` and
    `docs/architecture.md`.
  - A new row in CLAUDE.md's documentation table.
  - Samples: `samples/aovs.usda` and `samples/aovs_lpe.usda`.
- **Dependencies**: none new. `exr` already writes named channels, half/float/
  u32 samples and custom header attributes. MurmurHash3 for Cryptomatte is
  about 30 lines of safe Rust and is written in-tree. A denoiser (OIDN) is not
  part of this change; it would be an `unsafe`/FFI dependency, which is a
  project decision.
