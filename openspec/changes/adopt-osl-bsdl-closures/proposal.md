# Proposal

## Why

Crust's MaterialX BSDF leaves are a port of MaterialX's GLSL library
(`closure/mx.rs`), with analytic albedo fits, the GLSL `layer` rule and a BSDL
table taken from NVIDIA Typhoon's vendored copy. No reference pins them: the OSL
oracle covers pattern nodes only. MaterialX's OSL backend, rendered by OSL's
`testrender` over **BSDL**, is the reference that has energy-conserving
tabulated albedos and runs on the CPU in a path tracer, as crust does. Matching
it gives crust a reference it can check the leaves against, number by number.

## What Changes

- **BREAKING (rendered output):** `layer` follows testrender's opacity rule. A
  top's opacity is linear in every weight: a leaf contributes `weight · E(ωo)`
  if it lets light through (a reflection-only dielectric or generalized
  Schlick, or a sheen), `weight` if it is opaque (everything else, including a
  dielectric or Schlick that refracts). `mix` blends opacities, `add` and a
  nested `layer` sum them, and `multiply` scales them. The base is weighted by
  `1 − clamp(opacity, 0, 1)`. An opaque leaf at weight 0.3 in a top now lets
  70% of its base through, where today it hides the base completely.
- **BREAKING (rendered output):** every MaterialX BSDF leaf is ported from
  upstream BSDL's `mtx::` lobes instead of genglsl. That covers
  `dielectric_bsdf` (R, T, RT), `conductor_bsdf`, `generalized_schlick_bsdf`,
  `sheen_bsdf` (both modes), `oren_nayar_diffuse_bsdf` (with and without energy
  compensation), `burley_diffuse_bsdf` and `translucent_bsdf`, each with BSDL's
  value, multiple-scattering compensation and layering filter. Each leaf's
  tables (GGX, dielectric reflect-front / both-front / both-back, Conty–Kulla
  sheen, Zeltner sheen) are regenerated from upstream OSL at a pinned commit.
  The dielectric table's source moves off Typhoon's copy.
- A **BSDL oracle**: a script builds upstream BSDL and a probe and records each
  leaf's value, pdf, albedo and layering filter over a grid of cases into a
  committed fixture. A Rust test replays that fixture, as the Adobe OpenPBR
  oracle does. A second test pins the `layer` opacity rule against a
  transcription of testrender's `evaluate_layer_opacity` on fixed trees.
- Kept, by decision, and recorded as deviations from testrender: crust's own
  importance sampling, with no path-roughness regularization (testrender's is
  biased); the random-walk `subsurface_bsdf` (testrender renders it as a
  Lambertian); genglsl's Chiang hair (genosl has only a placeholder).
- Out of scope: VDF combinators and media (they stay as specified); the
  surface-shader nodegraphs (unchanged: no built-in graph puts an opaque leaf
  or a `multiply` in a `layer` top, so the opacity rule moves none of their
  pixels on its own).

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `materials`: "MaterialX closure-tree semantics" changes its `layer`
  throughput to testrender's linear opacity. "MaterialX transmission and
  interior media" takes its `T`-mode weighting from BSDL instead of GLSL. A new
  requirement says the MaterialX BSDF leaves are checked against BSDL at a
  pinned OSL commit, with named deviation rules.

## Impact

- **Code:** `crates/crust-core/src/material/closure/` (`mod.rs`'s
  `walk` opacity, a new `bsdl.rs` replacing most of `mx.rs`, new and
  regenerated tables beside `bsdl_tables.rs`), `closure/tests.rs`,
  `crates/crust-core/tests/mtlx_surfaces.rs` and `resolve.rs`. `crust-mtlx` is
  unchanged.
- **Tooling:** new `scripts/bsdl_oracle.py` + `scripts/bsdl_oracle/probe.cpp`,
  `scripts/tables/` regenerated from upstream OSL, and a new
  `crates/crust-core/tests/bsdl_oracle.rs` with its committed fixture. CI
  compiles no C++.
- **Output:** every MaterialX material's image changes (leaf tables), and
  hand-authored documents with weighted opaque leaves or a `multiply` in a
  `layer` top change more. Goldens are re-recorded. Native `OpenPBR` and
  `UsdPreviewSurface` do not change.
- **Performance:** table lookups replace rational fits on the leaves. This is
  measured with callgrind and `bench_ab.sh` on `materialx_basic` and the
  shipped surfaces before the change is accepted.
- **Docs:** `openspec/specs/materials/design.md` (the "trap" section reverses,
  the leaves' sources, the known gaps), `docs/material_fidelity.md` (re-scored
  against `materialx-osl`), `THIRD-PARTY.md` (BSDL from upstream OSL),
  `site/content/docs/usd/materials.md`.
