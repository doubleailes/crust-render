+++
title = "Overview"
description = "The crates that make up Crust Render, and how a render flows through them."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-14T08:00:00+00:00
draft = false
weight = 10
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Crust Render is a Rust workspace of eight crates. Each owns one job and knows nothing about the others’ internals.'
toc = true
top = false
+++

This section is for readers who want to know how Crust Render works inside: to judge
whether it suits a use, to read its code, or to contribute. You don't need it to render.

The [Design choices](@/docs/architecture/design-choices.md) page explains why it is built
this way, and [Limitations](@/docs/architecture/limitations.md) lists what it doesn't do.
The contributor-level map, with every module and invariant, is
[`docs/architecture.md`](https://github.com/doubleailes/crust-render/blob/main/docs/architecture.md)
in the repository.

## The crates

```text
                    crust-render  (the CLI; binary `crust`)
                     │         │
                     │         ▼
                     │    crust-assets ──► ptex-rs
                     │    (file decoders, texture caches)
                     ▼         │
                    crust-core ◄┘
         (USD import, integrator, materials, lights, volumes, guiding)
          │       │        │         │        │         │          │
          ▼       ▼        ▼         ▼        ▼         ▼          ▼
      crust-rt crust-mtlx crust-jit crust-nn utils  openqmc-rs  openusd,
      (kernel) (MaterialX) (JIT)   (networks) (math) (sampling)  opensubdiv-rs
```

| crate | owns | knows nothing about |
|-------|------|---------------------|
| `crust-rt` | geometry, the BVH build, ray intersection, instancing, motion blur | materials, lights, USD |
| `crust-mtlx` | reading `.mtlx` documents, compiling node graphs into programs, the BSDF closure tree | any Crust type |
| `crust-jit` | compiling MaterialX programs to machine code (optional, the `jit` feature) | everything but `crust-mtlx` |
| `crust-nn` | small neural networks — a hash-grid encoding, dense layers, their training — that `neural` light selection learns visibility with | any Crust type |
| `crust-core` | USD import, the scene, the integrator, materials, lights, volumes, path guiding, statistics, the diagnostic and its report | decoding images, textures or IES files |
| `crust-assets` | every file decoder (EXR, PNG/HDR, Ptex, IES, `.tx`), the texture tile caches, `.tx` conversion | the integrator |
| `crust-render` | the command line, logging, the progress bar, writing the EXR and PNG, printing and saving the diagnostic's report | decoding anything |
| `utils` | stateless math: sampling warps, MIS heuristics, luminance | everything |

Three libraries come from outside the workspace, all pure Rust:

- [`openusd`](https://github.com/mxpv/openusd) reads and composes USD stages.
- [`openqmc-rs`](https://crates.io/crates/openqmc-rs) is a port of the Academy Software
  Foundation's [OpenQMC](https://github.com/AcademySoftwareFoundation/openqmc)
  quasi-Monte Carlo sampler. It produces exactly the same samples as the C++ library.
- `opensubdiv-rs` and [`ptex-rs`](https://github.com/doubleailes/ptex-rs) refine
  subdivision surfaces and read Ptex files.

`openqmc-rs` started inside this repository and was extracted once it had no Crust types
in its interface. `crust-rt`, `crust-mtlx` and `crust-nn` are kept the same way, so that
they could be extracted too.

## A render, end to end

1. **Command line.** `crust-render` parses its flags and builds the asset loader, which
   reads the `CRUST_*` texture settings.
2. **USD import** (`crust-core`). A light "index" stage is opened with payloads unloaded,
   to read the render settings, pick the camera and list the top-level subtrees. Each
   subtree is then composed on its own masked stage, traversed and dropped. Prims become
   meshes, spheres, curves, instances, lights, volumes or the camera. Materials are
   resolved and cached. Every image, Ptex or IES file is decoded by `crust-assets`.
3. **Geometry build** (`crust-rt`). Meshes are either baked into world space or kept as
   instances, then the top-level acceleration structure is built.
4. **Renderer setup.** Light-selection tables are built, plus the learned light cache when
   [`crust:lightSelection`](@/docs/usd/render-settings.md#crust-lightselection) is
   `learned`, or the visibility network (`crust-nn`) when it is `neural`.
5. **Rendering.** Tiles run in parallel. For each pixel and sample, a path is traced:
   intersect, shade the hit once, sample a light, pick the next direction, repeat. Path
   guiding (training passes) and adaptive sampling (stopping pixels early) wrap that same
   per-pixel routine. The whole frame is taken to 1 sample per pixel first, then 2, 4, 8,
   … up to the adaptive minimum, so a render watched with
   [`--checkpoint`](@/docs/reference/command-line.md#checkpoint) shows the full image early;
   the order changes no pixel. Ctrl-C [stops the render](@/docs/reference/command-line.md#interrupting-a-render)
   and keeps what it traced.
6. **Output** (`crust-render`). The linear EXR and the tone-mapped PNG are written, and
   the `--stats` report is printed.

`--stats` times each of these phases, and `--profile` breaks the rendering phase down
further. See [Command line](@/docs/reference/command-line.md#stats).

### Several passes from one import

[`--all-passes`](@/docs/reference/command-line.md#all-passes) runs steps 2 and 3 once for
every group of [render passes](@/docs/usd/render-passes.md) whose settings agree on what
the import builds (subdivision and dicing, the shutter, the working colour space, the
primvars the products keep), and steps 4 to 6 once per pass. The traversal judges every
pass's collections as it meets each prim and records the answers, three bits per prim:
held out, hidden from the camera, hidden from every ray. After it, each pass's answers
become its own copy of the per-geometry ray masks, holdout flags and light list, with
light linking resolved over what that pass keeps. Before each pass renders, those masks
are written into the built acceleration structure in place: its tree and bounds never
read a mask, so the result is the structure an import of that pass alone builds, and the
pass renders bit for bit as `--pass` renders it. That same function decides a single
`--pass` too. Nothing is consulted per ray: a pass costs the render nothing, but hidden
geometry is still traversed.

## Where things plug in

The crates meet at a few interfaces. Each has one contract that both sides keep:

| interface | contract |
|-----------|----------|
| the kernel's `Geometry` / `SceneBuilder` / `Scene` | modelled on Embree: attach geometry, `commit()`, then `intersect` or `occluded`. A hit is only a geometry id and a primitive id. |
| `AssetLoader` | the host decodes files. Returning nothing means "fall back to the constant value", never an error. |
| `Texture2D`, `PtexTexture` | texture values come out linear. UDIM tile addressing is the host's job. |
| `Material` | shaded once per path vertex into a `ShadingPoint`, which answers every later question about that hit |
| `Light` | light sampling and BSDF sampling compute the same density for the same point on a light |
| `ProgressCallback` | the engine reports progress and never prints |
| `RenderControl` | the host owns it: it can read the image so far from any thread, and cancel the render, which then returns what it traced |
