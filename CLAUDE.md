# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Crust Render is a toy, physically-based path tracer written in safe Rust (edition 2024),
inspired by PBRT, *Ray Tracing in One Weekend*, and Autodesk Standard Surface / OpenPBR.
Scenes are loaded exclusively from **USD** (`.usda` / `.usdc` / `.usdz`) via the pure-Rust
[`openusd`](https://github.com/mxpv/openusd) crate — RON support was removed.

## Commands

```bash
# Build / render (single binary in the workspace, so bare cargo run works)
cargo run --release -- -i samples/openpbr_showcase.usda -o out.exr
cargo run --release -- -i samples/cornellbox.usda
cargo run --release                 # no -i → hard-coded procedural fallback (world::simple_scene)
cargo run --release -- --bucket -i samples/cornellbox.usda   # tiled/bucket rendering

# CLI flags: -i/--input, -o/--output (default output.exr), -l/--level (log level), -b/--bucket

# Tests (integration tests live in crust-core/tests/usd_scene.rs, load sample USD files)
cargo test
cargo test -p crust-core loads_cornellbox_usda     # run a single test by name

# Benchmarks (criterion)
cargo bench -p crust-core            # bench targets: "vec3 dot", "simple world"

# CI runs: cargo build --verbose && cargo test --verbose
```

Logging uses `tracing`; set verbosity with `-l debug|info|warn|error|trace` (default `info`).

## Workspace layout

Three crates under `crates/`:

- **`crust-core`** — the whole engine as a library (`crust_core`): renderer, integrator,
  materials, primitives, lights, BVH, USD import. Everything of substance lives here.
  UI-free by design: no progress-bar or image-encoding dependencies; progress is
  reported through a `ProgressCallback`, and fallible entry points return
  `crust_core::Error` instead of exiting.
- **`crust-render`** — the thin CLI binary. Parses args, builds a `Scene`, calls the
  `Renderer` (wiring an `indicatif` bar to the progress callback), writes the EXR and
  the tone-mapped PNG. `main.rs` is the only file.
- **`utils`** — math/RNG helpers (`random*`, `random_cosine_direction`, `align_to_normal`,
  `balance_heuristic`, `clamp`, `Lerp`). Depended on by `crust-core`.

`crust-core/src/lib.rs` re-exports the public surface (`Renderer`, `Scene`, `Camera`, the
material types, `simple_scene`, `get_settings`). Prefer importing from `crust_core::` roots.

## Render pipeline (the big picture)

1. **`main.rs`** builds a `Scene { camera, world, lights, settings }` — either from USD
   (`Scene::from_usd`) or the procedural fallback (`world::simple_scene` + `get_settings`).
2. **`Renderer`** (`tracer.rs`) drives sampling. Two entry points, both Rayon-parallel:
   - `render()` — parallel over pixels within each scanline row.
   - `render_with_tiles()` — parallel over 16×16 tiles (the `--bucket` path).
3. **`trace_path()`** (`tracer.rs`, public wrapper `ray_color()`) is the integrator — an
   **iterative** path tracer in two passes: a forward walk that traces one segment per
   bounce and records a `VertexRec` per vertex, then a backward gather that folds the
   records into the radiance estimate and emits guiding training samples (which need
   the radiance from the rest of the path — the reason for the backward pass). Features:
   - **MIS** combining direct light sampling and BRDF sampling via `balance_heuristic`.
     Emission at a bounce-arrival vertex is owned by the *previous* vertex's record
     (`next_emit` + MIS weight); counting it at the vertex itself too would double it.
   - **Russian roulette** from the 4th vertex on (`RR_START_BOUNCE`): survival tracks
     path throughput with a probability floor (`RR_MIN_PROB`), factor divided out on
     survival.
   - Volumetric scattering (Henyey-Greenstein) and Beer-Lambert attenuation when a ray
     carries `Some(Medium)` (set by transmissive OpenPBR refraction — see `ray.rs` /
     `medium.rs`). Free-space rays are unaffected.
   - A sky-gradient background when nothing is hit.
4. **Path guiding** (opt-in via `crust:pathGuiding`, `guiding/` module): a pure-Rust
   Practical Path Guiding SD-tree (`GuidingField`). `render_guided()` runs training
   passes at 1, 2, 4, … spp, splats `(position, direction, luminance·cos²)` samples
   into the field between passes, then renders the final pass with one-sample MIS
   between the frozen field and the BSDF (mixture pdf; secondary bounces only —
   primary vertices sit far below the field's spatial resolution). All passes
   (training + final) are blended into the output weighted by inverse variance, so
   the training budget is not discarded. Delta/transmissive
   materials (`Material::eval` → `None`) and untrained regions fall back to pure BSDF
   sampling. The NEE weight competes against the same mixture pdf — keep the two sides
   consistent or emission gets double-counted.
5. **Adaptive sampling**: pixels stop early once they hold `crust:minSamplesPerPixel`
   samples and the relative standard error of the pixel mean drops below
   `crust:varianceThreshold` (0 disables). Applies to main/final passes, never to
   guiding training passes.
6. The CLI writes the linear EXR to the `-o` path and a tone-mapped sRGB PNG next to it
   (same path, `.png` extension) — e.g. `-o renders/foo.exr` produces `renders/foo.exr`
   and `renders/foo.png`. Tone mapping and PNG encoding live in `main.rs`; the engine
   crate only produces the `Buffer`.

## Core traits (extension points)

- **`Hittable`** (`hittable.rs`) — `hit(ray, t_min, t_max) -> Option<Hit>` + `bounding_box()`.
  `HitRecord` is `Copy` geometry only (point, normal, `t`, `front_face`); `Hit` pairs it with
  a **borrowed** `&dyn Material`, so traversal never touches an `Arc` refcount. Implemented by
  `Sphere`, `Triangle`, `SmoothTriangle`, `Mesh`, `HittableList`, `Bvh`. Rendering uses a
  **two-level BVH**: `Renderer::new` builds a top-level `Bvh` (`bvh.rs`) over the scene's
  `HittableList`, and each imported mesh carries its own nested `Bvh` over triangles
  (`usd_import.rs`). `Bvh` is a flat node array with binned-SAH splits and iterative
  traversal — deterministic for a given scene.
- **`Material`** (`material/material.rs`) —
  `scatter_importance(r_in, rec) -> Option<ScatterSample>` used by the integrator
  (`ScatterSample.delta` marks singular lobes like transmission: never mixed with a
  continuous density, no tracer cosine, emission carried at full weight),
  `eval(r_in, rec, wi) -> Option<(value, pdf)>` (evaluate the *continuous* component
  toward a given direction — what NEE and guided MIS need; `None` = no continuous
  component at all, and per its contract that decision must never depend on `wi`),
  and `emitted()`. Exactly two implementations: **`OpenPBR`**,
  the single übershader for all surfaces (with `diffuse`/`metal`/`glass`/`glossy` preset
  constructors used by `world.rs` and the USD fallback), and **`Emissive`**, a pure
  emitter with no geometry knowledge. Shared microfacet helpers (aniso GGX VNDF sampling,
  Schlick Fresnel, sheen, thin-film) live in `material/brdf.rs`.
- **`Light`** (`light.rs`) — `sample_point`/`pdf`/`emission`/`material`. The one
  implementation is **`AreaLight`**: a `LightShape` (pure emitting geometry —
  `SphereShape`, `RectShape`) paired with the `Arc<Emissive>` its scene geometry carries.
  Lights are stored in a `LightList` and their surfaces are also added to `world` as
  emissive geometry (Cornell-box semantics: a light is both light and visible object) —
  sharing one `Emissive` Arc, which is how the integrator attributes a bounce-hit
  emissive surface to its light (`LightList::find_by_material`, address identity).
  **NEE samples one light per vertex** (uniform pick), so the light strategy's MIS
  density is `light.pdf / n_lights` — the bounce side evaluates the exact same
  expression for the light it hit; keep the two sides identical or emission is
  double-counted. Emissive geometry with no light-list entry is handled: the bounce
  keeps its emission at full weight.

Sampling uses **Correlated Multi-Jittered (CMJ)** patterns from `sampler.rs`
(`generate_cmj_2d`) for camera and light rays, falling back to plain RNG past the CMJ budget.

## USD import (`scene/usd_import.rs`)

The only scene format. `load_scene` opens the stage, imports `RenderSettings` first (the
camera needs the aspect ratio), then traverses prims with an explicit stack that bakes the
Xform hierarchy into world matrices. Schema mapping:

- `UsdGeomMesh` → world-baked triangles wrapped in a `BVHNode`; `UsdGeomSphere` → analytic `Sphere`.
- Materials resolve via `MaterialBindingAPI`, dispatched on the bound shader's `info:id`:
  - `UsdPreviewSurface` → mapped into `OpenPBR` (portable; `diffuseColor→baseColor`,
    `metallic→baseMetalness`, `roughness→specularRoughness`, etc.).
  - `crust:openpbr` → decoded 1:1 into `OpenPBR`; every input is the camelCase mirror of the
    Rust field name (lossless but non-portable). Reference scene: `samples/openpbr_showcase.usda`.
  - Unbound geometry → grey diffuse `OpenPBR`.
- `UsdLuxSphereLight` → emissive `Sphere` geometry + `AreaLight(SphereShape)`;
  `UsdLuxRectLight` → two emissive `Triangle`s + `AreaLight(RectShape)` (local XY plane,
  emitting along -Z per UsdLux; effectively one-sided). Sample scene: `samples/rectlight.usda`.
  Other lux types (`DiskLight`, `DistantLight`, `DomeLight`, `CylinderLight`) warn once and
  are skipped.
- `UsdRenderSettings` gives `resolution`; per-render params live as custom attrs in the
  `crust:` namespace (`crust:samplesPerPixel`, `crust:maxDepth`, `crust:minSamplesPerPixel`,
  `crust:varianceThreshold`, `crust:frame`). Missing attrs fall back to defaults (128 spp,
  depth 32, 640×360) defined as consts at the top of the file.

Note: `openusd` is a hard dependency and USD is always compiled in. Docstrings in `scene.rs`
that mention a `usd` **feature flag** are stale — there is no such feature.

## Known incomplete work

- **Upstream `openusd` xformOp bug, worked around locally.** `openusd` 0.5.0 (latest as
  of 2026-06) composes multi-op `xformOpOrder` stacks in the wrong order (the authored
  translate comes back multiplied by the scale), which used to make
  `samples/cornellbox.usda` render as floating objects against sky. `usd_import.rs`
  therefore composes the individual `xformOp:*` attributes itself
  (`compose_xform_ops`: translate/scale/rotateX·Y·Z/rotate-Euler-triples/orient/
  transform, `!invert!` prefixes, namespaced suffixes), falling back to openusd's
  composition — with a warning — only for op kinds it cannot decode. Regression test:
  `cornellbox_transforms_compose_correctly`. If upstream fixes the bug, the fallback
  (`local_matrix_via_openusd`) and possibly the whole composer can be dropped.
- USD lux light schemas beyond `SphereLight`/`RectLight` are skipped (see above). Disk
  lights need a disk primitive; distant/dome lights need non-area `Light` impls and
  integrator support for lights without scene geometry.
- **Path guiding** covers surfaces only (no volume/phase guiding) and trains on luminance
  (no chromatic distributions). Thick transmission — dispersive or not — is a
  continuous Walter et al. 2007 microfacet BTDF — sampled via VNDF + Snell, evaluable
  over the full sphere, and part of the NEE/guide mixtures (guide-chosen directions
  cross the interface via `Material::make_ray`, which tags the interior medium).
  Dispersion is continuous per-channel: each RGB channel refracts with its own IOR,
  sampling picks one channel's IOR uniformly, and evaluation runs three per-channel
  BTDF evaluations whose sampling pdfs average into the channel-mixture density. Only
  thin-walled transmission remains a per-sample delta lobe (`ScatterSample::delta`),
  excluded from continuous mixtures. The guide-vs-BSDF selection probability is fixed (no learned α), and
  spatial lookups are not parallax-compensated.
