# Proposal

## Why

Crust's MaterialX reader reduces only graphs built from standalone BSDF nodes
(`oren_nayar_diffuse_bsdf`, `dielectric_bsdf`, `layer`, `mix`, …). The three
surface-shader nodes nearly every real `.mtlx` document uses —
`standard_surface`, `open_pbr_surface` and `gltf_pbr` — are reported as
unsupported, and the surface falls back to the default material. The full run of
Ben Houston's Material Fidelity suite (`docs/material_fidelity.md`) makes the
cost concrete: **826 of 826** materials render as the fallback ball, and crust
scores 14.4 dB mean PSNR where the suite's Cycles renderer scores 26.4 dB. Until
these nodes are read, that suite (and any production MaterialX look) measures
crust's scene setup, not its shading. Crust is OpenPBR-native, so reading these
nodes is mostly a parameter mapping onto the model it already implements.

## What Changes

- A MaterialX material whose surface is a `standard_surface`,
  `open_pbr_surface` or `gltf_pbr` node SHALL render with that node's
  parameters instead of the fallback material. Every input is honoured whether
  it is authored as a value, connected to a node graph (evaluated per shading
  point, like existing pattern graphs) or left at its MaterialX 1.39 nodedef
  default.
- `open_pbr_surface` maps onto crust's OpenPBR parameters one to one.
- `standard_surface` maps onto OpenPBR exactly as MaterialX's published
  translation graph `standard_surface_to_open_pbr_surface`
  (`libraries/bxdf/translation/standard_surface_to_open_pbr.mtlx`) does,
  including that graph's own documented approximations.
- `gltf_pbr` maps onto OpenPBR by its glTF 2.0 (and `KHR_materials_*`)
  semantics. MaterialX publishes no translation for it, so crust defines and
  documents the mapping.
- A surface's `normal` input (`geometry_normal` for OpenPBR) drives the shading
  normal, as a BSDF node's `normal` does today.
- Surface-shader materials can **transmit**. Their transmission, interior media,
  thin-film and coat use OpenPBR's own lobes, which the lobe-pooling reduction
  never reaches today, where MaterialX transmission renders opaque.
- An authored input the mapping cannot represent is reported once per material
  at `WARN` and otherwise ignored. That covers `opacity` / `geometry_opacity` /
  `alpha`, anisotropy rotation, coat normal, authored tangent and glTF
  `occlusion`.
- The existing standalone-BSDF reduction is unchanged, and so are its renders.
- **Not in this change**: the pattern nodes `crust-mtlx` has no operator for
  (`separate2`, `fract`, `range`, `ifgreater`, `combine4`, the noise family,
  `place2d`, …). They gate most of the suite's `nodes/*` samples and are the
  next change. This one is measured on the suite's `surfaces/*` and
  `showcase/*` groups.

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `materials`: adds requirements for how a MaterialX surface-shader node
  (`standard_surface`, `open_pbr_surface`, `gltf_pbr`) is interpreted: its
  inputs and defaults, its mapping to OpenPBR, its shading normal, transmission,
  and the reporting of inputs it cannot represent. The existing "Supported
  shading models" requirement is unchanged: these materials still delegate to
  `OpenPBR`.

## Impact

- **`crust-mtlx`**: recognises the three surface nodes, compiles every input to
  a program slot with the nodedef default, and exposes them beside the existing
  flattened lobes. It stays free of crust types: it names MaterialX inputs, not
  OpenPBR fields.
- **`crust-core` `material/materialx.rs`**: a per-model mapping from those slots
  to `OpenPBR`, bypassing `reduce()` for surface-shader materials. `make_ray`
  evaluates the graph when transmission can be non-zero, so refracted rays carry
  the interior medium.
- **`crust-jit`**: no new operators. The mapping runs after the program, so JIT ↔
  interpreter bit-identity is unaffected, but the pinning tests gain a
  surface-shader program.
- **Tests / fixtures**: a new self-contained sample document with one material
  per model. `materialx_basic` is pinned at exactly three materials, so it is not
  extended. `tests/resolve.rs` gains the new materials.
- **Docs**: `openspec/specs/materials/design.md` (MaterialX section and Known
  gaps) and `docs/material_fidelity.md` (re-run and new baseline).
- **Performance**: one graph run per path vertex, as today. Unconnected inputs
  fold to constants under `CRUST_MTLX_OPT`, and the mapping is a few dozen scalar
  operations. Existing BSDF-graph materials take exactly the code path they take
  today.
- **Behaviour change**: documents that render as the fallback today will render
  with their authored material. No scene that renders correctly today changes.
