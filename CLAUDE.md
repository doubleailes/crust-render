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
- **`crust-render`** — the thin CLI binary. Parses args, builds a `Scene`, calls the
  `Renderer`, writes the EXR. `main.rs` is the only file.
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
3. **`ray_color()`** (`tracer.rs`) is the integrator — recursive path tracer with:
   - **MIS** combining direct light sampling and BRDF sampling via `balance_heuristic`.
     Bounce-hit emission is owned by the MIS-weighted `add_emission` term; the recursion
     suppresses the next vertex's self-emission (`suppress_emission`) to avoid counting
     it twice.
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
6. Output is written to the `-o` EXR, then **`convert()`** (`convert.rs`) tone-maps to sRGB PNG.

### Gotcha: EXR/PNG output paths are partly hard-coded
`convert()` reads a **hard-coded `"output.exr"`** and writes a timestamped PNG under
**`./test_images/`** — it ignores the `-o` value. Rendering with `-o something_else.exr`
still produces that EXR, but the PNG conversion step reads a stale/missing `output.exr`.
Keep `-o output.exr` (the default) if you want the PNG.

## Core traits (extension points)

- **`Hittable`** (`hittable.rs`) — `hit(ray, t_min, t_max, &mut HitRecord)` + `bounding_box()`.
  `HitRecord` carries the point, normal, `Option<Arc<dyn Material>>`, `t`, and `front_face`.
  Implemented by `Sphere`, `Triangle`, `SmoothTriangle`, `Mesh`, `HittableList`, `BVHNode`.
  The top-level `world` is a **linear `HittableList`** (no acceleration); `BVHNode` is only
  built to wrap the triangles of an imported mesh (`usd_import.rs`). BVH build picks a random
  split axis, so it is non-deterministic.
- **`Material`** (`material/material.rs`) —
  `scatter_importance(r_in, rec) -> Option<(Ray, brdf_value, pdf)>` used by the integrator,
  `eval(r_in, rec, wi) -> Option<(value, pdf)>` (evaluate toward a *given* direction — what
  NEE and guided MIS need; `None` = delta/transmissive, and per its contract that decision
  must never depend on `wi`), and `emitted()`. Exactly two implementations: **`OpenPBR`**,
  the single übershader for all surfaces (with `diffuse`/`metal`/`glass`/`glossy` preset
  constructors used by `world.rs` and the USD fallback), and **`Emissive`**, which doubles
  as the `Light` implementation. Shared microfacet helpers (aniso GGX VNDF sampling,
  Schlick Fresnel, sheen, thin-film) live in `material/brdf.rs`.
- **`Light`** (`light.rs`) — `sample`/`sample_cmj`/`pdf`/`color`. Lights are stored in a
  `LightList` and are also added to `world` as emissive geometry (Cornell-box semantics:
  a sphere light is both light and visible object).

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
- `UsdLuxSphereLight` → an `Emissive` sphere (light + geometry). Other lux types
  (`RectLight`, `DiskLight`, `DistantLight`, `DomeLight`, `CylinderLight`) warn once and are skipped.
- `UsdRenderSettings` gives `resolution`; per-render params live as custom attrs in the
  `crust:` namespace (`crust:samplesPerPixel`, `crust:maxDepth`, `crust:minSamplesPerPixel`,
  `crust:varianceThreshold`, `crust:frame`). Missing attrs fall back to defaults (128 spp,
  depth 32, 640×360) defined as consts at the top of the file.

Note: `openusd` is a hard dependency and USD is always compiled in. Docstrings in `scene.rs`
that mention a `usd` **feature flag** are stale — there is no such feature.

## Known incomplete work

- Non-sphere USD lux light schemas are skipped (see above).
- **Path guiding** covers surfaces only (no volume/phase guiding) and trains on luminance
  (no chromatic distributions). Transmissive OpenPBR surfaces have no `Material::eval`,
  so NEE and guiding skip them (delta treatment). The guide-vs-BSDF selection probability is fixed (no learned α), and
  spatial lookups are not parallax-compensated.
