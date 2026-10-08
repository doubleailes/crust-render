# Proposal

## Why

crust's native `OpenPBR` (which `UsdPreviewSurface` and `crust:openpbr` shade with)
is described as aligned with Adobe's `openpbr-bsdf` reference. That alignment is only
checked in prose. `docs/openpbr_reference_alignment.md` lists eight known gaps, and
nothing measures them, so a gap can widen or close without anyone noticing.

The largest gap is the fuzz. crust's fuzz is Charlie D with Neubelt's visibility,
under a scalar `(1 − fuzz_weight)` layer. Adobe's is Zeltner–Burley–Chiang's
LTC sheen, with a directional layer. They differ by more than their noise:
- At roughness 0.3 facing the camera, Adobe reflects 0.0008 of the light and crust's
  Imageworks albedo 0.052.
- At roughness 1.0 facing the camera, Adobe reflects 0.342 and crust 0.157.

The MaterialX side has the same model. `sheen_bsdf mode="zeltner"`, which every
`open_pbr_surface` graph's fuzz expands to, is evaluated as Charlie and reported
as approximated. Typhoon does the same thing silently: it parses the mode and
never reads it.

This change is the first step toward making native OpenPBR track Adobe as a whole:
1. It gives the alignment a measurement, an oracle.
2. It closes the largest gap against it, the fuzz.

## What Changes

- **An Adobe oracle.**
  - A small C++ probe is built against Adobe's header-only `openpbr-bsdf` at a pinned
    commit, plus GLM. A regeneration script runs it to produce a committed fixture:
    eval (diffuse and specular), pdf, directional albedo and emission, for sampled
    material parameters and direction pairs.
  - A `crust-core` test replays the fixture through native `OpenPBR`. Every known
    gap is a named **deviation rule**: an input condition under which a difference
    is excused, with its measured bound, in the same spirit as the
    `crust-mtlx` OSL oracle's exceptions.
  - The deviation list becomes the roadmap. Each later change that closes a gap
    deletes its rule.
  - CI runs the replay only. It needs neither a C++ compiler nor Adobe's sources.
- **Native OpenPBR fuzz becomes Adobe's fuzz layer.** **Image change** on every
  surface with `fuzz_weight > 0`:
  - The lobe is a Zeltner LTC sheen from Disney's 32×32 "Volume" table: bilinear in
    (cos θ_o, `fuzz_roughness`), evaluated, sampled and pdf'd exactly, with no
    roughness clamp.
  - The base under the fuzz is scaled by `1 − fuzz_weight · R(ω_o)`, where R is the
    lobe's own view-dependent albedo. This is Adobe's default, view-side-only
    attenuation. The attenuation by `(1 − fuzz_weight)` alone is retired.
  - Emission is attenuated by the same `1 − fuzz_weight · R(ω_o)`, after the coat
    passage.
  - The fuzz raises the coat's roughness by Adobe's empirical coupling.
  - Unlike Adobe's, the fuzz stays on a back-facing hit of a surface that is not
    thin-walled, as the coat and the emission do: most cloth is an open mesh.
  - The lobe-selection weight becomes `fuzz_weight · R(ω_o) · max(fuzz_color)`.
- **One Zeltner for both paths.** MaterialX `sheen_bsdf` in `zeltner` mode
  evaluates, samples and layers with the same table:
  - Its layer throughput is `1 − weight · R(ω_o)`.
  - It is no longer reported as approximated.
  - `conty_kulla` mode is unchanged.
  - This departs from MaterialX's own analytic fits by up to about 0.01 in R. That
    departure is recorded as a known gap.
- **Licensing.** The table's 3072 values originate in Disney's `ltc-sheen` (Apache-2.0)
  and are identical, bit for bit, in Adobe's and BSDL's copies. They get a
  `THIRD-PARTY.md` entry.
- **Documentation.** The alignment doc's "Fuzz" gap is retired and its other gaps point
  to their deviation rules. The materials design record and the user documentation
  (`site/content/docs/usd/materials.md` § Fuzz) describe the new model.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `materials`:
  - "Supported shading models" changes: OpenPBR's fuzz is a directional
    Zeltner layer.
  - "Shared microfacet BRDF helpers" changes: Zeltner LTC joins Charlie.
  - "Unrepresentable and approximated MaterialX inputs are reported" changes: a
    `zeltner` sheen is no longer an approximated closure.
  - New requirements: native OpenPBR fuzz behaviour, and native OpenPBR checked
    against the Adobe reference.

## Impact

- **Code:**
  - `crust-core/src/material/openpbr/` (`lobes.rs`: eval, split, pdf, pmf, filter,
    albedo; `mod.rs`: sampler, `emitted_directional`, resolve).
  - `material/brdf.rs` gains the LTC lobe and table.
  - `material/closure/mod.rs` gains the `Sheen` leaf's mode.
  - `crust-mtlx/src/bsdf.rs` carries the mode to the leaf and drops the report.
- **Tests:**
  - New `crust-core/tests/adobe_oracle.rs` and its fixture.
  - `tests/resolve.rs`, the OpenPBR and closure white-furnace tests, and
    `mtlx_surfaces.rs`, whose zeltner-reported assertion is inverted, are re-pinned.
- **Scripts:** a new `scripts/adobe_oracle/` (probe and regeneration script; needs a
  C++17 compiler and network access to fetch Adobe and GLM; never run by CI).
- **Images:**
  - `samples/openpbr_showcase.usda`, `samples/materialx_lion.usda` and
    `samples/materialx_surfaces.mtlx` change where they carry fuzz or sheen.
  - Every other golden is bit-identical. `fuzz_weight = 0` keeps the existing skip
    path.
- **Performance:**
  - A fuzz surface trades one `powf` for a bilinear fetch of three floats and a
    rotation, which is neutral to slightly faster.
  - The default material's zero-fuzz path must keep its instruction count. A
    callgrind check on `cornellbox` gates this.
- **Dependencies:** none in the workspace. The oracle's C++ dependencies live outside
  Cargo.
