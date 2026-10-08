## Context

`render_guided()` (`crates/crust-core/src/tracer/mod.rs`) renders training passes at
2, 2, 4, 8, … spp, then a final pass at the scene's budget, and returns their blend.
On `main` the blend weighted each pass by `1 / PassStats::variance`, the mean over the
pass's pixels of each pixel's sample variance *in that same pass*, through one shared
`blend_weights` that the beauty (`blend_passes`), the AOV films (`AovFilm::blend`) and
the blend's own variance map all applied. `blend_luminance` built the reference image
of the `ΔEff` efficiency estimate from its own copy of the same inverse-variance
weights.

An inverse-variance blend of unbiased passes is unbiased only if the weights are
independent of the pass values. Here they are not. Where rare bright paths carry the
image — fireflies on ALab, where the default clamp removes 66% of the luminance; a
caustic through glass from a small light — most low-spp passes contain few or none of
them, so a pass's estimated variance and its brightness fall together. The weight goes
to whichever pass missed the most energy.

Constraints:

- Blending must stay deterministic and bit-identical between tiles and rows
  (`guided_tiles_and_rows_are_bit_identical`).
- The final pass is adaptive, so how many samples a pixel took depends on its values.
- `ΔEff` still needs per-pass variance maps, so the training schedule keeps its 2-spp
  floor and `PassStats::var_map` stays.
- Render regions (`raster_region`, `Buffer::for_raster_rect`): every per-pixel plane
  covers the region only.

## Goals / Non-Goals

**Goals:**

- The blended image is an unbiased combination of the passes for any transport,
  heavy-tailed included; ALab's guided crop no longer reads darker than unguided.
- Training samples keep contributing to the image.
- A regression test that fails on the old blend.

**Non-Goals:**

- Making `ΔEff` robust to heavy tails. It decides whether the final pass is guided;
  once the blend is fixed it never biases the image, but a wrong "on" costs noise
  (Risks). Recorded as a known gap.
- Guiding narrow lobes less, or learning the guide/BSDF split.
- Changing the training schedule, the guide probability, or which vertices are guided.
- An environment switch for the old blend. Switches exist to A/B an optimization
  against what it replaced; the old blend is a bias, not a speed setting, and a
  scratch build of the parent commit is enough to compare against it.

## Decisions

### Weight each pass by its sample budget

`sₖ = sppₖ / Σ spp` (`pass_weights(&[u32]) -> Vec<f64>`, pure, unit-tested), from each
pass's configured spp. The weights are known before any pass renders, so the blend is
a fixed convex combination of unbiased images: unbiased by construction.

Variance against keeping the final pass alone: with per-sample variances `σₖ²`, the
blend's is `Σ sppₖ σₖ² / (Σ spp)²`. Keeping the training passes (`n_t` spp in all, at
per-sample variance `σ_t²`) beats discarding them whenever
`σ_t² < (2 + n_t/n_f) σ_f²`, with `n_f` the final pass's spp. On ALab's crop the
training passes are no noisier per sample than the guided final pass (across 80 seeds
the final pass is the noisiest), well inside the bound; under the default schedule
`n_t` is 16 spp.

Alternatives considered:

- **Inverse estimated variance (the old blend).** Biased under heavy tails, as
  measured.
- **Final pass only.** Unbiased, but it throws training away (on
  `cornellbox_guided.usda`, 8 iterations at 64 final spp, 256 of 320 spp).
- **Cross-estimated inverse variance** (weights from one half of the samples, values
  from the other). Unbiased, but it keeps the failure that matters: a 2-spp pass that
  missed the fireflies everywhere still takes the weight, and the rare firefly it does
  hold is amplified with it.
- **Robust variance estimates** (median, trimmed). They underestimate a heavy tail,
  which favours the low-spp passes again.

### The configured budget, not the samples taken

An adaptive final pass stops some pixels early. Weighting a pixel by the samples it
actually took would tie the weight to that pixel's values, the bug this change
removes. The configured budget is fixed in advance; an early-stopped pixel keeps the
final pass's full share, which is harmless, since it stopped because it had converged.

### One weight vector for the image, the AOVs, the variance and the reference

`render_guided` computes the shares once and hands them to `blend_passes`,
`AovFilm::blend` (which now takes shares, not weights and their total) and the
variance map (`Σ sₖ² varₖ`, now exact: the weights are constants). `blend_luminance`
takes the same `pass_weights`. The design record already warned that the efficiency
reference must not correlate with a pass's own noise; fixed weights are that rule
applied to the reference itself. A 1-spp final pass, which the old blend dropped
(its variance could not be estimated), now weighs its share, and the blend's variance
map is then infinite: unknown, which is the truth.

### A caustic regression test

The ALab failure needs a 160-s import; the test needs a scene that renders in seconds.
`samples/caustic_guided.usda` (PR #197's exploration scene: a rough glass ball,
`specularRoughness` 0.05, on a white floor under a small sphere light) is that case in
miniature: almost all the light in the ball's shadow is a caustic. The ignored
`guided_caustic_is_not_darkened` compares the guided and unguided shadow means over 8
seeds at `--indirect-clamp 0`; it is calibrated to fail on the old blend and pass on
the new (tasks 2.4).

## Measurements

All with `--indirect-clamp 0` and adaptive sampling off on both sides; the numbers
are recorded in `rendering`'s design record ("Path guiding", "Known gaps: path
guiding"), the evidence being the spread across seeds, never one render.

- **ALab, the issue's probe** (`guided_bias_probe`, crop `[176, 88, 448, 360]` of
  frame 1004, 64 spp, 40 seeds, seed step 0x9E3779B9): unguided 0.4611 ± 0.0046;
  guided 0.4311 ± 0.0056 with the old blend (−6.5%, z −4.1 unpaired / −4.7 paired,
  darker on 36/40 seeds) and 0.4461 ± 0.0049 with budget weights (−3.3%, z −2.2 /
  −2.7, darker on 30/40). Not yet within noise by the |z| < 2 bar, so the second
  suspect was tested.
- **Each pass against an unguided render with its seed and spp** (a scratch build
  logging every pass's own mean; 80 seeds, the first 40 the same): pass 0
  bit-identical on every seed, passes 1–3 and the guided final pass −3.3 ± 3.3%,
  −0.6 ± 2.1%, −0.3 ± 1.8%, −0.9 ± 0.9%, the blend −0.5 ± 0.8% (z −0.6). The final
  pass's shift reads z −2.6 on seeds 0–39 and +1.5 on seeds 40–79: the −3.3% was the
  seed set. No pass, the guided ones included, is biased at this resolution — the
  guide mixture ↔ NEE pair is not a second cause.
- **An independent replication at 256 spp** (40 fresh seeds, step 1000003): guided
  0.4560 ± 0.0034 against 0.4547 ± 0.0016 unguided, +0.3%, z +0.3 / +0.4, darker on
  23/40. A bias of the size the 64-spp seeds suggested would have read z ≈ −4.8.
- **The caustic** (`guided_caustic_is_not_darkened`, 64², 256 spp, 32 seeds): shadow
  ratio guided/unguided 0.510 with the old blend (the test fails), 0.97 with budget
  weights (it passes); per block of 8 seeds 0.50–0.52 against 0.80–1.07.
- **`ΔEff` and noise.** On ALab's 40 seeds `ΔEff` went from 0.02–4.20 (15 final
  passes guided) to 1.02–1.20 (all 40); on `caustic_guided.usda` from 0.01–0.87 (none)
  to 3.36–3.84 (all 4). The standard error across ALab's seeds is 0.0049 guided
  against 0.0056 before and 0.0046 unguided at 64 spp; at 256 spp the guided one is
  2.1× the unguided one.
- **Unguided renders** are bit-identical: `check_images.sh check` against goldens of
  the parent commit (tasks 5.2).

## Risks / Trade-offs

- **[Measured] Guided renders keep their final pass guided more often, and that can
  be noisier.** The unbiased reference moves `ΔEff`: on ALab's crop from 0.02–4.20
  (15 of 40 final passes guided) to 1.02–1.20 (40 of 40). The guided final pass has
  about twice an unguided pass's variance across seeds there, so ALab's guided
  render is noisier than an unguided one with fewer samples, though no noisier than
  `main`'s guided render was. → Accepted: a noisy unbiased image is fixed by turning
  guiding off, a silently dark one is not. `ΔEff`'s blindness to rare paths is a
  known gap.
- **[Trade-off] Guided images change.** Expected. Unguided renders and the
  `check_images.sh` goldens are untouched but for `cornellbox_guided`, the one sample
  that authors guiding.

## Migration Plan

None. No setting, file format or API changes. Rollback is a revert.
