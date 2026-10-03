## Context

`render_guided()` (`crates/crust-core/src/tracer/mod.rs`) renders training passes at
2, 2, 4, 8, … spp, then a final pass at the scene's budget. It returns
`blend_passes(passes)`: one scalar weight per pass, `1 / PassStats::variance`, where
`variance` is the mean over pixels of each pixel's sample variance in that same pass.
`blend_luminance` builds the shared reference image for the `ΔEff` efficiency
estimate from the same weights.

An inverse-variance blend of unbiased passes is unbiased only if the weights are
independent of the pass values. Here they are not. Under heavy-tailed transport, most
low-spp passes contain no bright sample at all, so a pass's estimated variance and its
image brightness fall together. In the measured case (see proposal.md - Why) one 2-spp
pass took 80% of the weight. Its image had no caustic in it.

Constraints:

- Blending must stay deterministic and bit-identical between tiles and rows. This is
  pinned by `guided_tiles_and_rows_are_bit_identical`.
- The final pass is adaptive, so how many samples a pixel took depends on its values.
- `ΔEff` still needs per-pass variance maps, so the training schedule keeps its 2-spp
  floor.

## Goals / Non-Goals

**Goals:**

- The blended image is an unbiased combination of the passes for any transport,
  heavy-tailed included.
- Training samples keep contributing to the image.
- A regression test that fails on the current blend.

**Non-Goals:**

- Making `ΔEff` robust to heavy tails. It decides whether the final pass is guided.
  Once the blend is fixed it never biases the image, but a wrong "on" costs noise,
  not just speed (see Risks). It is recorded as a known gap.
- Guiding narrow lobes less. That is the fix for the noise below, and it is a
  separate change.
- Changing the training schedule, the guide probability, or which vertices are guided.
  The exploration measured removing the training cap and guiding primary vertices;
  neither gave a consistent gain.
- An environment switch for the old blend. Switches exist to A/B an optimization
  against what it replaced. The old blend is a bias, not a speed setting, and a
  scratch build is enough to compare against it.

## Decisions

### Weight each pass by its sample budget

`w_k = spp_k / Σ spp`, using each pass's configured `PassConfig::spp`. The weights are
known before any pass renders, so the blend is a fixed convex combination of unbiased
images, and it is unbiased by construction.

Variance cost against the ideal: with per-sample variances `σ_k²`, the blend has
variance `Σ spp_k σ_k² / (Σ spp)²`. Keeping the training passes beats discarding them
whenever their per-sample variance is below `2 + n_t/n_f` times the final pass's
(`n_t`, `n_f`: training and final spp). On `cornellbox_guided.usda` guiding cuts error
by about 20%, so the untrained first pass is at most about 1.5× as noisy per sample.
That is well inside the bound.

Alternatives considered:

- **Inverse estimated variance (current).** Biased under heavy tails, as measured.
- **Final pass only.** Unbiased, but it throws training away. On
  `cornellbox_guided.usda` (8 iterations at 64 final spp) that is 256 of 320 spp, 80%
  of the samples.
- **Cross-estimated inverse variance.** Each half of the pixels (a checkerboard, or
  split sample sets) is weighted by the variance estimated on the other half. This is
  unbiased, because weights and values are independent given the field. But it keeps
  the failure that matters: the 2-spp pass that missed the caustic everywhere still
  gets 80% of the weight, and now its rare firefly is amplified 4×. Unbiased, but
  very noisy.
- **Robust variance estimates** (median, trimmed). A heavy tail is underestimated by
  exactly these estimators, which favours low-spp passes again.

### Budget, not samples taken

An adaptive final pass stops some pixels early. Weighting a pixel by the samples it
actually took would tie the weight to that pixel's values, which is the bug this
change removes. The configured budget is fixed in advance. The cost is that an
early-stopped pixel keeps the final pass's full share, which is harmless: it stopped
because it was already converged.

### One weight vector for the image and for the `ΔEff` reference

`blend_luminance` takes the same budget weights, through one shared function
(`pass_weights(&[u32]) -> Vec<f64>`, pure, unit-tested). The design record already
warns that the efficiency reference must not correlate with a pass's own noise. Fixed
weights are that rule applied to the reference itself.

### Regression test on a caustic scene

The current failure needs heavy-tailed transport, which `cornellbox_guided.usda` does
not have. Add `samples/caustic_guided.usda`: the glass ball (`specularRoughness` 0.05)
on a white floor under a small sphere light, the exploration scene. Add an
`#[ignore]`d end-to-end test in `crates/crust-core/tests/guiding.rs` comparing the
guided and unguided means over the shadow region at `--indirect-clamp 0`. The seed is
fixed, so the test is deterministic. It must be shown to fail on the old blend before
the fix lands, at a resolution and spp that keep it to seconds in release.

## Risks / Trade-offs

- **[Risk] A scene where guiding is dramatically better than BSDF sampling gives its
  early, poorly-trained passes too much weight.** → Bounded by the `2 + n_t/n_f` rule
  above, and training is 16 spp under the default schedule. Re-measure
  `cornellbox_guided.usda` relMSE against a high-spp reference, old blend against new,
  over 4 seeds. If the new blend is clearly worse there, revisit before landing.
- **[Risk] The "about 20% less error" figure in the user docs was measured with the
  biased blend.** → Re-measured (task 3.2). The claim doesn't hold: at equal time an
  unguided render has about half the error. Corrected in `site/`, `README.md` and the
  design record.
- **[Measured] `caustic_guided.usda` gets unbiased but noisier.** With the biased
  reference gone, `ΔEff` reads 1.26–1.30 instead of 0.00–0.32 and keeps the final
  pass guided. Guiding at the glass's narrow lobe is about 6× noisier there (shadow
  relMSE 2.56 against 0.49 before and 0.42 unguided). This is not a bug in the
  sampler: at the glass vertices 36% of guide samples contribute nothing, and BSDF
  samples carry twice their weight where the guide density is near zero. Switching
  guiding off at the glass alone gives 0.41, and `crust:guidingProb` 0.1 gives 0.41.
  → Accepted for this change: a noisy unbiased image can be fixed by turning
  guiding off, a silently dark caustic can't. Documented as a limitation. A
  follow-up change stops guiding narrow lobes.
- **[Trade-off] Guided images change.** Expected. Unguided renders and the
  `check_images.sh` goldens are untouched, since the blend only runs with
  `crust:pathGuiding`.

## Migration Plan

None. No setting, file format or API changes. Rollback is a revert.
