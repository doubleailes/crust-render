## Why

A guided render darkens caustics. `render_guided()` combines its training passes and
its final pass using inverse-variance weights, and it estimates each pass's variance
from **that pass's own samples**. When a pass happens to miss rare, very bright samples
(caustic paths through glass), its estimated variance is low *and* its image is dark,
so it gets a large weight. The combination is no longer unbiased: the weights depend
on the very values they weight.

Measured on a rough glass ball on a white floor, lit by a small sphere light
(`specularRoughness` 0.05, 256², `--indirect-clamp 0`, compared with a 32k-spp unguided
reference):

- On seed 0, the weight shares came out `[0, 80, 0, 0, 20]`: a 2-spp training pass
  outweighed the 256-spp final pass 4 to 1.
- The caustic came out at **25%** of the reference energy at 256 spp and **50%** at
  1024 spp.
- Averaged over 4 seeds at 1024 spp, the caustic is at **69%** and its shadow region at
  **84%**. Rendering only the final pass of the same guided render gives 94% and 97%;
  unguided gives 98% and 99%.
- At `specularRoughness` 0.2 the bias is not visible, and it does not show on
  `cornellbox_guided.usda`, which is why `guided_render_is_unbiased` never caught it.

The documented claim, "every pass is an unbiased image, so the inverse-variance blend
is too", only holds when the weights are independent of the pass values. The same
noisy low-spp variances also feed the reference image of the guiding efficiency
estimate.

## What Changes

- **The pass blend uses weights fixed before rendering.** Each pass is weighted by its
  sample budget (`spp`) as a share of the total budget, so no weight depends on a
  rendered value. Training passes still contribute; nothing is discarded.
- **The efficiency estimate's shared reference image** (`blend_luminance`) uses the same
  budget weights, so it no longer favours whichever training pass missed the bright
  samples.
- **Guided images change.** Scenes without heavy-tailed transport (for example
  `cornellbox_guided.usda`) change by noise only. Caustic scenes regain the energy
  they lost. Unguided renders are bit-identical.
- **A caustic sample scene and a regression test**: the glass-ball scene used above
  becomes `samples/caustic_guided.usda`. An end-to-end test fails on the old blend and
  passes on the new one.
- **Not in scope:** the `ΔEff` decision itself still compares 2–8-spp variance
  estimates, which are unreliable under heavy tails. This change only stops that
  decision, or any variance estimate, from biasing the image. The remaining
  unreliability is recorded as a known gap.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `rendering`: "Opt-in path guiding" — passes are blended with weights fixed by their
  sample budgets, not by estimated inverse variance, and the blend is required to stay
  unbiased when a pass's samples are heavy-tailed.

## Impact

- `crates/crust-core/src/tracer/mod.rs`: `blend_passes`, `blend_luminance`, the
  `render_guided` doc comment. `PassStats::variance` stays only if the debug log still
  needs it.
- `crates/crust-core/tests/guiding.rs`: a new caustic regression test, plus a unit-level
  check of the weights.
- `samples/caustic_guided.usda` (new).
- Docs: `openspec/specs/rendering/design.md` (the guiding paragraph, a measurement
  note, and the "Known gaps: path guiding" entry for `ΔEff`);
  `site/content/docs/usd/render-settings.md` (one sentence on how training passes are
  combined).
- Output: guided renders change; unguided renders and the golden images
  (`check_images.sh`, unguided at 16 spp) do not.
- Performance: none expected. The blend is one pass over the image either way.
