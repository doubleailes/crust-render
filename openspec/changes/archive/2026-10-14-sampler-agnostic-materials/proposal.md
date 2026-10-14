## Why

`crust_core::PathSampler` is a type alias (`openqmc::SobolSampler`), and it is part of the
signature of every `Material` method that samples. So the QMC sequence is fixed at compile
time, and the `dyn Material` boundary keeps it that way: a trait object cannot take a generic
sampler. openqmc-rs ships six samplers behind one API (`SobolSampler`, `LatticeSampler`,
`PmjSampler` and their blue-noise `*Bn` variants), but trying one means editing the alias and
rebuilding.

That is how the 2026-10-13 spike was run. It measured against Sobol, unclamped, over 8 seeds,
with a reference built from 64 × 16 spp:

| vs Sobol | render instructions (cornellbox, 2 spp) | wall time (`bench_ab.sh`, min) | relMSE at 4 / 16 spp |
|---|---|---|---|
| Lattice | −3.1% | −1.1 to −7.3% | −9% on cornellbox, **+20% on materialx_basic** |
| SobolBn | +3.7% | **+3.1 to +10.2%** | equal; error after a 4×4 downsample −8 to −12% at 4 spp, error only slightly blue (0.038–0.045 of its power below ¼ Nyquist vs 0.049 white) |

No sampler wins everywhere, so no sampler should replace Sobol. But the best sampler depends
on the scene, and blue noise has a small low-spp effect (previews, MCP progressive renders, a
denoiser pass). Both are choices a user or `crust diagnostic` should be able to make
**without a rebuild** and **without the default paying for it**. The spike also found that
materials barely use the sampler. Every implementation draws exactly one 4D block from the
domain the tracer hands it, and none derives a sub-domain. So the coupling is easy to remove
cleanly.

## What Changes

- **Materials stop seeing the sampler.** `Material::scatter_importance` and every
  `scatter*` / `straight_transmittance` beneath `ShadingPoint` take the numbers they draw
  instead of a `PathSampler`. The `dyn Material` arm gets a concrete `BsdfDraw` (four
  uniforms, extensible without a breaking change). Lazy draws on the concrete arms take a
  closure, so a material that draws nothing today still draws nothing.
  **BREAKING** (crate API): the `Material` trait signature changes. Every implementer is
  inside `crust-core`.
- **The integrator becomes generic over the sampler.** `trace_path`, NEE, the pass-through
  and medium helpers, `subsurface::random_walk` and `render_pixel` are generic over
  `S: openqmc::SamplerImpl`. Each pass dispatches once on the selected sampler, the same way
  it already dispatches `PROFILE` / `MEDIA` / `TWINS`.
- **A render setting picks the sampler.** `crust:sampler` on `RenderSettings` and
  `--sampler` on `render`, `check` and `diagnostic`. Values: `sobol` (default), `sobolBn`,
  `lattice`, `latticeBn`, `pmj`, `pmjBn`. An unknown value warns (`SettingsInvalidValue`) and
  falls back to `sobol`. The value is stamped into every EXR as `crust:sampler`, and
  `crust diff` warns when the two stamps differ.
- **The default is bit-identical.** With `sobol`, every image, AOV and `--stats` count is
  identical to the renderer before this change. The default render's instruction count stays
  within callgrind noise.
- **The learned light-selection pre-pass stays on Sobol** whatever the setting. It is a
  training pass, not an image sample, and keeping it fixed keeps the learned table identical
  across samplers.
- The `rendering` and `materials` design records, the CLI reference, the render-settings
  page and `docs/architecture.md` describe the setting, the new material contract and the
  spike's numbers.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `rendering`: the low-discrepancy sampling requirement is restated for the current
  domain-tree API (its text still describes a `Sampler` trait and an `RngSampler`
  fallback, which no longer exist). Adds a selectable sampler with a bit-identical Sobol
  default, and the rule that the light-selection pre-pass is sampler-independent.
- `materials`: adds the rule that a BSDF sample is a pure function of the numbers it is
  given, which also makes tests with exact uniforms possible.
- `cli`: `--sampler` joins the flags shared by `render`, `check` and `diagnostic`.
- `image-output`: EXRs record the sampler as `crust:sampler`.
- `image-comparison`: a sampler mismatch between two stamps is a comparability warning.

## Impact

- **Code:** `crust-core/src/lib.rs` (`PathSampler` survives as the default sampler's
  name), `material/{material,openpbr/mod,closure/mod,materialx,pattern,emissive}.rs`,
  `subsurface.rs`, `tracer/{mod,path,settings}.rs`, `scene/usd_import/settings.rs`,
  `stamp.rs`, `compare.rs`, `diagnostic/mod.rs`, `crust-render/src/main.rs`.
  `light_cache.rs` keeps the concrete Sobol type. `volume.rs` and `guiding/` already use
  `pcg::Rng` and are untouched.
- **Tests:** 24 test sites build `PathSampler::new(0, 0, 0, i)` just to feed a material. They
  become `BsdfDraw` values, and some can pin exact uniforms. New tests: default-render bit
  identity, each sampler converging to the Sobol mean, and the learned table being equal
  across samplers.
- **Build:** `trace_path` already has up to 16 monomorphisations
  (`PROFILE × AOV × MEDIA × TWINS`). Six samplers would make that 96. Release compile time and
  binary size are measured and gated (design.md, D4). The documented fallback is shipping
  fewer samplers.
- **Dependencies:** none new. `openqmc-rs` stays at `0.2.4`.
- **Docs:** `site/content/docs/usd/render-settings.md`,
  `site/content/docs/reference/command-line.md`, the `rendering` and `materials` design
  records, and `docs/architecture.md`.
