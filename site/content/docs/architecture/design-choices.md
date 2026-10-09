+++
title = "Design choices"
description = "The architectural decisions behind Crust Render, why each was made, and what it costs."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-01T08:00:00+00:00
draft = false
weight = 20
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'The decisions that shape Crust Render: what was chosen, why, and what it costs.'
toc = true
top = false
+++

Each choice below gives the decision, the reason for it, and its trade-off. The full
reasoning, with measurements and history, is in the design records
(`openspec/specs/<capability>/design.md`) in the repository.

## USD is the only scene format

**Decision.** Scenes are read only from USD (`.usda`, `.usdc`, `.usdz`), through the
pure-Rust [`openusd`](https://github.com/mxpv/openusd) crate. There is no OBJ, glTF or
custom scene loader, and none will be added.

**Why.** USD is what production pipelines exchange. With a single input format, every
feature is defined once, in USD terms: a light is a UsdLux prim, a setting is an
attribute on `RenderSettings`. Crust's own settings are ordinary custom attributes in the
`crust:` namespace, so a stage that sets them still opens in any other USD application.

**Trade-off.** `openusd` has no plugin system, so it can't resolve a MaterialX reference
the way the C++ USD library does. Crust reads `.mtlx` files itself, in `crust-mtlx`. Gaps
in `openusd` are worked around inside the importer and reported upstream.

## Safe Rust, written from scratch

**Decision.** Every crate is `forbid(unsafe_code)`, with two audited exceptions:

- `crust-core` allows `unsafe` for one test-only memory allocator.
- `crust-jit` has four `unsafe` blocks, needed to call the machine code it generates.

The ray tracing kernel, BVH, materials, MaterialX reader, volumes and path guiding are all
implemented here. Nothing comes from Embree, OpenPGL or another renderer.

**Why.** Safe Rust rules out a whole class of memory bugs in a codebase that is mostly
pointer-heavy data structures. Writing each part from scratch keeps the project dependency
light and fully readable.

**Trade-off.** SIMD stops at 128 bits (SSE2/NEON width). Wider vectors would need
`std::simd`, which is nightly-only, or `unsafe` intrinsics. An 8-wide BVH was tried on
nightly behind the `bvh8` feature. There is no GPU path.

## An Embree-shaped kernel in its own crate

**Decision.** All ray intersection lives in `crust-rt`, behind an interface modelled on
Embree's: attach geometry, `commit()`, then `intersect` (closest hit) or `occluded`
(shadow rays, which stop at the first hit). A hit is just a geometry id and a primitive
id. The kernel never sees a material.

Inside:

- triangles use a **watertight** intersection test (Woop et al. 2013), so rays can't slip
  through shared edges;
- the acceleration structure is an **SBVH** with spatial splits (Stich et al. 2009),
  collapsed into 4-wide nodes tested four boxes at a time;
- leaf triangles are packed into 4-wide SIMD packets;
- the build runs in parallel and is **deterministic**: the same scene always produces the
  same tree.

**Why.** A narrow, Embree-like interface keeps the kernel independent: it could be
swapped for Embree bindings behind the same seam, or reused elsewhere. Determinism means a
render can be reproduced exactly.

**Instancing only when it pays.** Meshes that share points, topology and material share
one BVH under instance transforms. A mesh placed only once is baked into world space
instead, because instancing it shares nothing and costs every ray a transform and a second
tree. On the Cornell box, baking cut instance descents from 3.85 to 0.13 per camera ray.
`CRUST_MESH_BAKE=0` restores instancing everything.

## One material model: OpenPBR

**Decision.** Every surface is shaded by one übershader, the
[OpenPBR Surface](https://academysoftwarefoundation.github.io/OpenPBR/) model: diffuse,
metal, glass, coat, fuzz, thin film, subsurface and emission in one parameter set. Every
material format maps onto it:

| authored as | becomes |
|-------------|---------|
| `crust:openpbr` | OpenPBR, parameter for parameter |
| `UsdPreviewSurface` | OpenPBR, with its inputs mapped (`diffuseColor` → `baseColor`, …) |
| `PxrDisneyBsdf` | OpenPBR, mapped |
| MaterialX graphs | a closure tree that collapses, at each hit, into at most eight weighted OpenPBR-style lobes |

**Why.** One BSDF means one implementation of sampling, evaluation and MIS to get right,
and one set of formulas to check against the reference (the MaterialX node graph and
Adobe's `openpbr-bsdf`).

**Shade once per hit.** A textured material does its expensive work (graph evaluation,
texture lookups) once per path vertex. The result is a `ShadingPoint`, which then answers
every question about that hit: emission, scattering, light sampling, path guiding. A test
checks, for every material type, that this gives bit for bit what shading each query
separately would.

## MaterialX compiled, then JIT-compiled

**Decision.** A MaterialX node graph is read once and compiled into a flat, slot-indexed
program that runs with no name lookups and no allocation. The program is then optimized
(constants folded, invariant work hoisted, unused nodes removed) and, in the default
build, compiled to machine code with [Cranelift](https://cranelift.dev/).

**Why.** Look-dev graphs run at every shading point of every path, so their cost is
paid billions of times.

**Bit-identical by construction.** The JIT emits only exactly-rounded IEEE operations
(add, multiply, compare, select, …) inline, and calls back into the interpreter for
anything else (`pow`, trigonometry, `min`/`max`, normal maps). A test compares every slot
of the JIT against the interpreter, bitwise. That is why
[`CRUST_SHADER_JIT=0`](@/docs/reference/environment-variables.md#crust-shader-jit) renders
the same image, only more slowly.

## Quasi-Monte Carlo sampling, and nothing else

**Decision.** Every random number comes from
[OpenQMC](https://github.com/AcademySoftwareFoundation/openqmc), through the `openqmc-rs`
port: an Owen-scrambled Sobol sequence, decorrelated per pixel. Sampling follows OpenQMC's
**domain tree**: each sample starts from a root keyed on `(pixel, frame, sample index)`,
each path vertex derives its own sub-domain, and each sampling event (light, BSDF, guide,
phase function) derives a further keyed one. There is no other random number generator in
the renderer.

**Why.** Stratified, low-discrepancy samples converge faster than independent random
numbers. Keying every draw also makes the render **reproducible**: a sample's random
numbers depend only on where it is, not on which thread runs it or in which order. This is
why tiles and scanlines (`--scanline`) give bit-identical images, and why
[`crust:frame`](@/docs/usd/render-settings.md#crust-frame) decorrelates the noise of
successive frames.

The port reproduces the C++ library's samples exactly. Sampling was once 16–24% of render
time; evaluating it from lookup tables cut render time by 6–20%, with every image
bit-identical.

## A unidirectional path tracer with MIS

**Decision.** The integrator is a forward path tracer. At each vertex it samples a light
(next-event estimation) and the BSDF, and combines the two with multiple importance
sampling: power heuristic by default, see
[`crust:samplingStrategy`](@/docs/usd/render-settings.md#crust-samplingstrategy). Russian
roulette ends paths, and the pixel filter is applied by filter importance sampling.

**Why.** MIS keeps both small bright lights (where light sampling wins) and sharp
reflections (where BSDF sampling wins) noise-free in one estimator. Filter importance
sampling draws sample positions from the filter itself, so filtering costs nothing per
sample and adaptive sampling still works per pixel.

**One density on both sides.** The light-sampling side and the BSDF side must compute the
same probability density for the same point on a light, or the image is biased. The code
derives both from one function, and a point whose density isn't finite is refused on both
sides rather than given a stand-in.

**Unbiased by default, with one exception.** The firefly clamp
([`crust:indirectClamp`](@/docs/usd/render-settings.md#crust-indirectclamp), default 10)
is the only biased default. Set it to 0 for reference renders.

**Options on top.** Adaptive sampling stops converged pixels early. Path guiding
(*Practical Path Guiding*, Müller et al. 2017, reimplemented in Rust) is opt-in. On the
bundled `cornellbox_guided.usda`, where all light arrives indirectly, guiding cuts the
error by about 20% at the same sample count. Light selection can learn which lights reach
each region of the scene (`learned`).

## The engine decodes no files

**Decision.** `crust-core` parses USD, but never decodes an image, texture, Ptex or IES
file. Every such read goes through the `AssetLoader` interface to `crust-assets`. A loader
that returns nothing means "use the constant value", never an error.

**Why.** The engine library has no codec dependencies, and a host can supply its own
loader (a test passes a loader that decodes nothing). The diagnostic example programs
decode through the same code, so what they report is exactly what the renderer sees.

## Memory scales with caches, not with the scene

**Decision.** The parts of a production scene that grow without bound are read
incrementally, under fixed budgets:

- **USD import is streamed.** The stage is composed one top-level subtree at a time,
  through a masked stage that is dropped afterwards. Composing the whole Moana Island costs
  `openusd` 75.74 GiB, but one element costs 1.10 GiB. Streaming cut the island's peak from
  117.10 GiB to 43.76 GiB and its import from 13:20 to 9:19, with a pixel-identical image.
- **UV textures stream from `.tx` files** (tiled, mip-mapped TIFF or EXR) in 64×64 tiles,
  through a cache with a byte budget
  ([`CRUST_TEX_CACHE_MB`](@/docs/reference/environment-variables.md#crust-tex-cache-mb)).
- **Large Ptex files stream** through the `ptex-rs` reader's own cache
  ([`CRUST_PTEX_STREAM`](@/docs/reference/environment-variables.md#crust-ptex-stream)).
  Below the per-face cap, the mip levels are the ones a loaded texture builds in linear
  light, held in that same cache, so streaming changes no texel the loaded texture
  has.

**Why.** Preloading makes memory track the scene's total texture footprint. Capping
resolution to fit throws authored detail away and still fails on a scene that binds more
than fits. Production renderers stream behind a bounded cache, so memory tracks the cache.

## Correct, or refused

**Decision.** Every colour input is converted to linear light once, at import or load
time, and the colour space each input is assumed to be in is documented. The conversions
are OpenColorIO's: every transfer curve, gamut conversion and colour-space name comes from
one OCIO config (the builtin ACES CG config unless
[`--ocio-config`](@/docs/reference/command-line.md#ocio-config) or the
[`OCIO`](@/docs/reference/environment-variables.md#ocio) variable names another), read by a
pure-Rust port of OpenColorIO. Crust Render renders in a scene-linear working space —
`lin_rec709` by default, ACEScg or another wide gamut when
[`renderingColorSpace`](@/docs/usd/render-settings.md#renderingcolorspace) asks — and a
colour that names its space is converted into it, while one that names none is taken as
already in it. When Crust Render
can't do something correctly, it refuses it and says so, rather than rendering something
plausible but wrong:

- A `.tx` whose mip levels were reduced in a different colour space is refused (see
  [`.tx` files](@/docs/usd/materials.md#tx-files)): its full-resolution level would look
  right while its coarser levels were wrong, which looks exactly like a filtering bug.
- MaterialX nodes Crust can't represent, such as `conical_edf`, are reported rather than
  approximated.
- Every refusal, approximation or skipped input is a `WARN` line. `INFO` stays a few lines
  per render, so warnings stand out.

**Why.** In a renderer, most mistakes produce an image that still looks reasonable: a
wrong albedo decode, the wrong mip level, a swapped texture. So Crust Render checks shading
in numbers, not by eye, and makes its silent failures loud.

## Every optimization keeps its old path

**Decision.** Each optimization that changes how a result is computed keeps the old way
behind a `CRUST_*` environment variable. With the switch set, the output is either
bit-identical, or differs in a documented way that has been shown to be noise rather than
bias. See [Environment variables](@/docs/reference/environment-variables.md).

Several pairs are pinned bit for bit by tests: SIMD triangle packets and the scalar
triangle test, JIT and interpreter, streamed and preloaded 8-bit textures, tiles and
scanlines.

**Why.** An optimization is only trustworthy if it can be compared against what it
replaced, on any scene, by anyone. Timing alone can't settle it: run-to-run spread reaches
15% on a busy machine. So changes are measured by interleaved A/B runs and instruction
counts, and their images are compared at 16 samples per pixel, where every pixel takes
exactly the same number of samples.
