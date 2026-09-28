# Design

## Context

See `proposal.md` § Why. The current state that shapes the approach:

- `crust-mtlx` compiles a material in two halves. A pattern `Program` (slot-indexed
  ops, optimised by `CRUST_MTLX_OPT`, JIT-compiled by `crust-jit`) and a flattened
  closure tree (`Lobe`s and `Emission` terms whose parameters are program slots).
  `bsdf::flatten` walks from the `surfacematerial`. When it meets a category it
  does not know, such as `standard_surface`, it stops. The compiler therefore never
  visits the node graph behind that surface, so the log names only the surface node.
- `crust-core`'s `MtlxMaterial::run` evaluates the program once per path vertex
  (`Material::resolve`), pools the lobes onto one `OpenPBR` in `reduce()`, and
  delegates the BSDF to it through `PatternMaterial`. `reduce()` is lossy by design
  (`openspec/specs/materials/design.md` § MaterialX). It pools roughness, maps
  transmission to no lobe, pools `thin_film_bsdf` as a dielectric, and so on.
- Crust's `OpenPBR` already carries every parameter the three surface nodes need:
  transmission and interior media, subsurface, fuzz, coat, thin film (thickness in
  µm, like OpenPBR's nodedef), emission as `color · luminance`, and dispersion.
  `geometry_opacity` exists as a field but is not implemented, and it has no
  tangent rotation or coat normal (`openpbr/mod.rs` § Not implemented).
- MaterialX 1.39 publishes `standard_surface_to_open_pbr_surface`, a translation
  node graph from Standard Surface to OpenPBR. It publishes none for `gltf_pbr`.
- The suite's documents: 826 surfaces, all at the nodedefs' default versions (19
  name `ND_standard_surface_surfaceshader` explicitly). None mixes
  `surfaceshader`s, and none has more than one surface node.

## Goals / Non-Goals

**Goals:**

- Read the three surface nodes with **no loss beyond what the target OpenPBR
  parameters cannot express**. The lossy lobe pool is used only for documents that
  really are standalone-BSDF graphs.
- One auditable reference per mapping: OpenPBR's own nodedef, MaterialX's
  translation graph, and the glTF specification.
- Leave every existing MaterialX render bit-identical.

**Non-Goals:**

- The missing pattern operators (`separate2`, `fract`, `range`, `ifgreater`,
  `combine4`, noises, `place2d`, …). These are the next change, and are where
  the suite's `nodes/*` group moves.
- A general `<nodedef>` / `<nodegraph>` implementation instantiator. That remains a
  known gap. This change does not build it, even though it could run the
  translation graph directly (D1).
- Opacity cutout (`opacity`, `geometry_opacity`, `alpha`, `alpha_mode`): an
  integrator feature (stochastic pass-through, shadow rays included), not a mapping.
- Anisotropy rotation, authored tangents and coat normals. OpenPBR in crust has none
  of these.
- `mix` of `surfaceshader`s, and the other surface nodes (`disney_principled`,
  `UsdPreviewSurface` in `.mtlx`, LaMa). They remain "unsupported node" as today.
- Making an emissive surface-shader material a light-list entry. MaterialX emission
  keeps today's rule: found by bounce sampling only, and never an `AreaLight`.
- Non-default nodedef versions. A `version` other than the default is warned about
  and mapped as the default.

## Decisions

### D1. Map parameters directly; do not expand the surface into BSDF nodes

A surface node becomes a set of **input slots** in the program, and `crust-core`
maps the evaluated slots onto `OpenPBR` fields at the shading point.

Alternatives considered:

- **Expand through MaterialX's `libraries/bxdf/*.mtlx` implementation graphs**,
  then flatten and `reduce()`. Rejected. It routes the one lossless case
  (`open_pbr_surface` → crust's OpenPBR) through the lossy pool. Transmission would
  still map to no lobe, and `coat_color` / thin film would pool wrongly. It also
  needs nodedef instantiation and the stdlib shipped at run time.
- **Compile the published translation graph at run time** (bind its
  `interfacename`s to the surface's inputs, then map `open_pbr` 1:1). This is
  elegant, and it would make the graph literally the reference. Rejected for now:
  it needs the general nodedef instantiator (a non-goal), plus the `ifgreater` /
  `ifequal` / `dot` / `dotproduct` operators the pattern-node change adds. It also
  cannot cover `gltf_pbr`. It is recorded as the natural refactor once both exist.
  D4's tests keep the Rust mapping honest against the graph until then.

### D2. The split: `crust-mtlx` knows MaterialX inputs; `crust-core` knows OpenPBR

- **`crust-mtlx`**: `flatten`, reaching `standard_surface`, `open_pbr_surface` or
  `gltf_pbr`, records a `SurfaceShader { model, inputs }` on `Compiled` in place of
  lobes. `inputs` holds one program slot per nodedef input, named by its MaterialX
  name. Each slot is compiled with the existing input path: a connection, a value,
  or the **nodedef default** from a per-model table in `crust-mtlx`. It also records
  which inputs were authored away from their default, for D7. `Compiled::roots()`
  and `optimize()` remap these slots like lobe slots, so unconnected inputs fold to
  constants.
- **`crust-core`**: `MtlxMaterial` gains a surface-shader mode. Its `run` evaluates
  the program as now, then calls `map_open_pbr` / `map_standard_surface` /
  `map_gltf_pbr` in place of `reduce()`. `crust-mtlx` stays free of crust types, as
  the workspace rule requires. It does not need OpenPBR's field list, only
  MaterialX's.
- **Why the defaults live in `crust-mtlx`**: they are MaterialX facts (the
  nodedef), and the suite's documents never carry the stdlib nodedefs. A missing
  input must still read the right default. To stop transcription drift, the three
  nodedefs (`ND_standard_surface_surfaceshader`, `ND_open_pbr_surface_surfaceshader`
  1.1, `ND_gltf_pbr_surfaceshader`) are **vendored as test fixtures** with their
  Apache-2.0 header. A test asserts the table has exactly their inputs, types and
  defaults.
- `gltf_pbr`'s `attenuation_distance` has **no** nodedef default: glTF's
  "infinite". The table represents it as `+∞`, and the mapping treats a
  non-finite distance as "no attenuation" (D5).

### D3. `open_pbr_surface` is one to one, including `coat_darkening`

Each input sets the `OpenPBR` field of the same name. The one deliberate difference
from today's lobe path is `coat_darkening`. `load()` zeroes it for lobe-built
materials, because MaterialX's `layer` is single-scattering. An `open_pbr_surface`
states its own `coat_darkening` (default 1), and MaterialX's reference graph for
the node implements it. So the authored value is used unchanged. `geometry_normal`
goes to the shading normal (D6). `geometry_opacity`, the tangents and the coat
normal are reported (D7).

### D4. `standard_surface` mirrors the translation graph node for node

`map_standard_surface` is written as the graph's nodes in order, each commented
with the graph's node name. That keeps it reviewable against the file, including
the graph's approximations the spec lists: coat tint on `base_color`, `coat_weight`
zeroed for coated metals, `specular_weight = 1` for any metalness,
`fuzz_roughness = sheen_roughness^0.4`, the nm → µm thin film,
`coat_darkening ← coat_affect_roughness`, and opacity's first channel. Tests pin the
mapping at representative inputs whose outputs are derived by hand from the graph.
Once the D1 alternative exists, one test runs the real graph and asserts equality.

**Improving on the graph is out of scope here.** A deviation, for example Standard
Surface's own `coat_affect_color`, is a later change justified by a suite
measurement. Otherwise "why does crust's Standard Surface differ" has no single
answer.

### D5. `gltf_pbr` is defined by the glTF specification

There is no published translation, so the mapping is crust's and is documented in
the MaterialX section of `design.md` field by field. Traps it must handle:

- **Roughness is already perceptual.** glTF's `α = roughness²` is OpenPBR's
  convention, so it maps unchanged. (The lobe path's `√alpha` conversion is only
  for BSDF nodes' `roughness` inputs.)
- **Transmission tint.** glTF tints transmitted light by `base_color` and
  attenuates by `attenuation_color` over `attenuation_distance`. OpenPBR has one
  `transmission_color`. With a finite distance, the volume wins
  (`transmission_color = attenuation_color`, `transmission_depth = distance`) and
  the interface tint is lost. Otherwise `transmission_color = base_color` at depth
  0. It is thin-walled exactly when `thickness == 0`, which is glTF's own rule.
- **Specular colour on metals.** OpenPBR's `specular_color` is also the metal's
  F82 edge tint. glTF's metal has no edge tint. So `specular_color` is
  `mix(specular_color, 1, metallic)`: exact at metallic 0 and 1, approximate
  between.
- **Sheen has no weight.** `fuzz_weight = max(sheen_color)`,
  `fuzz_color = sheen_color / fuzz_weight` (0 → no fuzz), and `fuzz_roughness =
  sheen_roughness`. This is the unit the lobe path already feeds Charlie with, to
  be confirmed by probe against `gltf_pbr.mtlx`'s `sheen_bsdf` wiring.
- **Clearcoat** → coat at IOR 1.5, `coat_color = 1`, `coat_darkening = 0`.
- **Iridescence** → thin film, `iridescence_thickness` nm → µm.
- **Anisotropy.** glTF's `α_t = mix(α, 1, s²)` and OpenPBR's anisotropy
  parametrise the stretch differently. `specular_roughness_anisotropy` is chosen to
  reproduce glTF's `α_b / α_t` ratio, and the resulting `α_t` mismatch is recorded.
  `anisotropy_rotation` is reported (D7).
- **Emission**: `emission_color = emissive`, `emission_luminance =
  emissive_strength`, unclamped.
- **Dispersion**: glTF `dispersion = 20 / V_d`, so it maps to
  `transmission_dispersion_scale` with OpenPBR's Abbe default of 20.
- **`occlusion`** is reported and ignored: the path tracer computes occlusion.

### D6. The surface's normal input reuses the lobe path's shading-normal rule

`run` already applies an optional graph normal: it normalises it, and ignores it if
it faces away from the geometric normal. The surface mode feeds the node's
`normal` / `geometry_normal` slot through that same code. The slot is `None` when
unconnected, which keeps the interpolated normal and skips the work.

### D7. Unrepresentable inputs are decided at compile time, reported at load

"Authored away from the default" is static: the input is connected, or its value
differs from the nodedef default. `crust-mtlx` returns the list of such inputs
drawn from a fixed per-model "unrepresentable" set. The importer logs them in one
`WARN` per material, beside the existing unsupported-node warning. That is the
same cardinality, and the logging rule's meaning of `WARN` ("authored, refused").
An explicit default (`alpha = 1`) is silent. The suite authors
`alpha_mode` / `geometry_opacity` at their defaults in dozens of documents, and a
warning there would be noise.

### D8. Transmission needs `make_ray` to know the medium, so the graph runs there, but only when it can matter

`pattern_make_ray` never runs the graph today, because the lobe path cannot
transmit. For a surface-shader material, `load()` computes `may_transmit`: false
exactly when the transmission-weight slot is a program constant equal to 0 after
optimisation. When it is true and `wi` points below the geometric surface,
`make_ray` runs the graph and mapping and delegates to `OpenPBR::make_ray`, which
attaches the interior medium. Every other `make_ray` stays the free
`Ray::new(rec.p, wi)`. That is the spec's "no extra per-ray cost" for opaque
materials.

### D9. Verification is numeric first, then the suite

- **Probe**: `examples/mtlx_shade` already prints the `OpenPBR` a material reduces
  to. It is the unit test's oracle and the tool to check each mapping by hand
  (`CLAUDE.md`: numbers, not eyes).
- **Fixture**: a new `samples/materialx_surfaces.mtlx` / `.usda` holds one material
  per model, plus a glass and a normal-mapped one. It is kept separate because
  `materialx_basic` is pinned at exactly three materials.
- **Pinned pairs**: `tests/resolve.rs` gains the fixture's materials. The JIT ↔
  interpreter test gains a surface-shader program. The mapping runs outside the
  program, so no new JIT operators are needed.
- **Suite re-run**: `scripts/material_fidelity/run.py`, with results recorded in
  `docs/material_fidelity.md`. The bar for archiving is on the `surfaces/*` and
  `showcase/*` groups: a group whose mean falls more than 3 dB below `blender-new`
  (Cycles) is investigated as a probable mapping bug before archiving. The target is
  not a spec requirement, because PSNR against a rasteriser's IBL mixes shading with
  lighting-model differences (prefiltered environment, no occlusion). It is still a
  tripwire.

## Risks / Trade-offs

- [The translation graph's approximations become crust's behaviour: coated metals
  lose their coat, `coat_darkening ← coat_affect_roughness`, and `specular` is
  ignored on any metal] → deliberate (D4), listed in the spec, and each has a
  suite sample (`input_coat_affect_roughness`, the metal showcases) that measures
  it. Deviations go through a later change.
- [Hand-transcribed defaults drift from MaterialX] → vendored nodedefs and an
  exact-match test (D2).
- [glTF approximations (transmission tint, metal edge tint, anisotropy stretch)
  look plausible and are wrong] → each is named in `design.md`'s Known gaps. The
  fixture pins its numbers, and the suite's `surfaces/gltf_pbr` samples isolate
  each one.
- [Throughput: a surface node compiles ~40 input slots] → unconnected inputs fold
  to constants under `CRUST_MTLX_OPT`. The mapping is scalar arithmetic once per
  vertex. The existing lobe path is untouched, and `bench_ab.sh` on
  `materialx_basic` / the DPEL teapot must show no change. Callgrind instruction
  counts on the new fixture are recorded in the materials `design.md`.
- [`make_ray` for transmissive materials now runs the graph a second time per
  refraction] → bounded to `may_transmit` materials and below-surface directions
  (D8). If it shows up in profiles, the later fix is to carry the resolved
  `OpenPBR` from `resolve` into `make_ray`, which is a trait change.
- [Document `colorspace` on constant colour inputs] → this change adds no colour
  rule. Surface inputs go through the compiler's existing input path and inherit
  exactly its handling. The suite is `lin_rec709` throughout, so it cannot catch a
  mistake here. `docs/color_management.md` should say so when this lands.
- [Opacity-heavy samples stay wrong] → out of scope (a cutout is integrator work).
  They are reported by D7 and listed in the suite write-up so they are not read as
  mapping bugs.

## Migration Plan

No migration. Documents that fall back today start rendering their authored
material, and nothing that renders correctly today changes. The lobe path is
pinned bit-identical by the unchanged-render scenario. Rollback is a revert: no
file format, CLI flag or environment switch is added. This is a feature, not an
optimisation, so there is no old behaviour to A/B behind a `CRUST_*` switch.
