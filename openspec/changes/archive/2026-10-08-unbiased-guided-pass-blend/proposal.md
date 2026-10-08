## Why

Guided renders of ALab come out darker than unguided ones
([#244](https://github.com/doubleailes/crust-render/issues/244)). `crust diagnostic`'s
picture check flagged `guiding=true` as `biased` on every ALab crop, and a direct test
confirmed it: a 272×272 crop of `renders/alab/alab_aovs.usda` at frame 1004
(`[176, 88, 448, 360]`), 64 spp, clamp and adaptive sampling off, 40 seeds per side,
read 0.4611 ± 0.0046 unguided against 0.4294 ± 0.0055 guided — 6.9% darker, z −4.4.

`render_guided()` combines its training passes and its final pass with inverse-variance
weights, and it estimates each pass's variance from **that pass's own samples**. A pass
that misses the fireflies (on ALab the default clamp removes 66% of the luminance, so
fireflies carry the image) has a low estimated variance *and* a dark image, so it takes
the weight; a pass that caught them is down-weighted along with their energy. The
weights depend on the values they weight, and the blend is biased low. The logged
shares were erratic: `[2, 69, 6, 8, 14]` gave a 2-spp training pass 69% of the image
and the 64-spp final pass 14%.

The same failure shows on a caustic: PR #197 measured it on a rough glass ball under a
small sphere light, where one 2-spp training pass took 80% of the weight and the
caustic came out at a quarter of its energy. That PR fell 120 commits behind `main`
(render regions, a shared `blend_weights` for the beauty and the AOVs, `blend_luminance`
over a region with the stage's luma); this change ports its idea onto the current code.

## What Changes

- **The pass blend uses weights fixed before rendering.** Each pass is weighted by its
  configured sample budget as a share of the total (`pass_weights`), so no weight
  depends on a rendered value. Training passes still contribute; nothing is discarded.
  The beauty, the AOV films and the blend's variance map take the same shares.
- **The efficiency estimate's shared reference image** (`blend_luminance`) uses the same
  budget weights, so it no longer favours whichever training pass missed the bright
  samples.
- **Guided images change.** ALab's crop is no longer darker (measured in design.md).
  Unguided renders are bit-identical.
- **A caustic sample scene and a regression test**: `samples/caustic_guided.usda` and
  the ignored `guided_caustic_is_not_darkened`, which fails on the old blend and passes
  on the new one.
- **`PassStats::variance` is gone**: the blend was its only reader besides a debug
  log, and the pass's own debug line still logs the mean variance.
- **Not in scope:** the `ΔEff` decision still compares 2–8-spp variance estimates,
  which cannot see rare bright paths. With an unbiased reference it now keeps ALab's
  final pass guided, and that pass is noisier there than an unguided one. A wrong "on"
  costs noise, never bias; it is recorded as a known gap.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `rendering`: "Opt-in path guiding" — passes are blended with weights fixed by their
  sample budgets, not by estimated inverse variance, and the blend is required to stay
  unbiased when a pass's samples are heavy-tailed.

## Impact

- `crates/crust-core/src/tracer/mod.rs`: `pass_weights` replaces `blend_weights`;
  `render_guided` records each pass's configured spp; `blend_passes`,
  `blend_luminance`; `PassStats::variance` removed.
- `crates/crust-core/src/aov.rs`: `AovFilm::blend` takes the shares directly.
- `crates/crust-core/tests/guiding.rs`: the caustic regression test; `tracer/tests.rs`:
  unit tests of the weights.
- `samples/caustic_guided.usda` (new); `crates/crust-render/examples/guided_bias_probe.rs`
  (the issue's probe, kept as a documented example).
- Docs: `openspec/specs/rendering/design.md` (the guiding paragraph, the ALab
  measurement, "Known gaps: path guiding"), the `diagnostics` and `aovs` records,
  `site/` (limitations, render settings, design choices), `README.md`,
  `docs/light_sampling.md`, `crates/crust-core/src/filter.rs`'s module doc.
- Output: guided renders change; unguided renders and the `check_images.sh` goldens do
  not.
- Performance: none. The blend is one pass over the image either way.
