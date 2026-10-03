# Proposal

## Why

Crust's MaterialX reader reduces only graphs built from standalone BSDF nodes, and
reduces them lossily. `bsdf::flatten` pools every leaf into one set of OpenPBR
parameters: roughness is averaged, a `layer` is guessed into "coat or base" by tree
shape, transmission maps to nothing, and thin film pools as a plain dielectric. The
three surface-shader nodes nearly every real `.mtlx` uses — `standard_surface`,
`open_pbr_surface` and `gltf_pbr` — are not read at all, and fall back to the
default material. The full Material Fidelity run (`docs/material_fidelity.md`)
measures the cost: **826 of 826** materials render as the fallback, 14.4 dB mean
PSNR against the suite's Cycles at 26.4 dB.

NVIDIA's Typhoon (the hdEmbree reference path tracer in NVIDIA's OpenUSD fork,
`typhoon/main`) shows what reading them *truthfully* takes. Each surface node is
built as the closure tree its MaterialX nodegraph describes — `layer`, `mix` and
`multiply` over individual BSDF leaves — and evaluated with MaterialX's
albedo-scaled layering, rather than being mapped onto one übershader. Crust
already has the leaf BSDFs (GGX with VNDF, EON, F82 Schlick, Charlie, thin film).
What it lacks is a real layer/mix evaluator over them.

## What Changes

- **The MaterialX lobe path becomes a closure-tree evaluator.** A MaterialX
  material compiles to a tree of BSDF leaves combined by `layer`, `mix`, `add`
  and `multiply`, and every leaf keeps its own parameters, normal and tangent.
  `layer(top, base)` evaluates as `f_top + f_base · (1 − E_top(ωo))`, MaterialX's
  and Typhoon's albedo scaling. The top layer's directional albedo comes from
  tables ported from BSDL (OpenShadingLanguage) and MaterialX.
- **BREAKING**: the pooled reduction onto OpenPBR (`reduce()`, its coat
  promotion, alpha pooling and the MaterialX-only `coat_darkening = 0`) is
  retired for *every* MaterialX document. Standalone-BSDF graphs
  (`samples/materialx_basic`, the DPEL Teapot and Lion) now render with real
  layering, and their images change.
- **Surface-shader nodes are read.** `open_pbr_surface`, `standard_surface`
  and `gltf_pbr` each expand into the closure tree of their MaterialX 1.39
  nodegraph (`libraries/bxdf/*.mtlx`), node for node, the way Typhoon's
  `openPbr.cpp` / `standardSurface.cpp` / `gltfPbr.cpp` do. Every input is
  honoured whether it is connected, authored, or left at its nodedef default.
- **Newly supported through the tree**: transmission and refraction with an
  interior medium (from the surface's transmission parameters or a VDF),
  per-leaf thin film, coat normal and authored tangents, a distinct coat IOR and
  roughness, and emission (EDF terms kept as today).
- **Still refused and reported** (one `WARN` per material, when authored away
  from the default): opacity / alpha (no cutout), anisotropy rotation, and glTF
  `occlusion`.
- **Known approximations, stated rather than hidden**: `subsurface_bsdf` shades
  as a diffuse-like leaf (no random walk yet); `sheen_bsdf` is Charlie in both
  modes (Zeltner is not implemented, which is also what Typhoon actually
  evaluates); layering is non-reciprocal by the model's own definition.
- **Not in this change**: crust's native `OpenPBR` (`crust:openpbr`,
  `UsdPreviewSurface`) is unchanged. A MaterialX `open_pbr_surface` and a
  `crust:openpbr` with the same values will now render differently, and aligning
  the two is a follow-up. So are the missing pattern nodes (`separate2`,
  `fract`, `range`, `ifgreater`, `combine4`, noises, …), which gate the suite's
  `nodes/*` group.

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `materials`: the "Supported shading models" requirement changes. A MaterialX
  material no longer delegates to one `OpenPBR`; it is evaluated as a closure
  tree. The change adds requirements for the tree's layering semantics, the three
  surface-shader nodes, per-leaf normals, transmission, and the reporting of
  inputs the tree cannot represent.

## Impact

- **`crust-mtlx`**: `bsdf::flatten`'s pooled `Vec<Lobe>` becomes a closure-tree
  IR (leaves plus combinators, parameters as program slots). There are
  per-model surface builders and nodedef default tables. Some new program
  operators are needed for derived quantities, each added to the interpreter and
  to `crust-jit` with bit-identity tests. It stays free of crust types.
- **`crust-core`**:
  - a closure-tree material and its resolved form, a `Resolved::Closure` variant
    beside `ResolvedOpenPBR` in `ShadingPoint`;
  - leaf evaluators over `brdf.rs`;
  - the ported directional-albedo tables;
  - `reduce()` and the pooled-lobe `MtlxMaterial` path removed.
- **Third-party data**: the BSDL tables (OpenShadingLanguage `libbsdl`,
  BSD-3-Clause) and the MaterialX GLSL albedo fits (Apache-2.0), vendored with
  attribution in a `THIRD-PARTY` notice.
- **Probes and tests**:
  - `examples/mtlx_shade` prints the resolved leaf list instead of OpenPBR
    parameters;
  - `crust-core/tests/resolve.rs`, the MaterialX unit tests in
    `material/materialx.rs`, and the `tests/usd_scene.rs` MaterialX assertions
    are rewritten;
  - a new fixture exercises each surface model.
- **Performance**: the tree is collapsed to a weighted leaf list once per path
  vertex (throughput depends only on ωo). Each BSDF query then sums a handful of
  leaves. MaterialX shading cost may rise against today's single pooled OpenPBR.
  It is measured with `bench_ab.sh` and callgrind, and recorded.
- **Docs**: `openspec/specs/materials/design.md` (MaterialX section rewritten,
  Known gaps), `docs/material_fidelity.md` (re-run), and the README's MaterialX
  section.
