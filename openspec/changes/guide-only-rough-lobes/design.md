## Context

Guided bounces are one-sample MIS between the guide and the material's whole BSDF
mixture (`sample_bounce_direction`, `tracer/path.rs`). The guide probability `α` is
the clamped `crust:guidingProb`, the same at every vertex. The same `α` appears in
four places that must agree:

- the coin;
- the mixture pdf `α·p_guide + (1−α)·p_bsdf`, on both branches;
- the `1/(1−α)` compensation on delta samples, which only the BSDF branch can
  reach;
- the NEE MIS weight's bounce pdf (`bounce_pdf`, around `path.rs:1189`).

Bounce-side MIS reads the stored mixture pdf, so it follows automatically.

Materials already compute what this needs, but don't expose it:

- **OpenPBR:** `LobePmf::from_params` (`openpbr/lobes.rs`) gives the selection
  weight of Diffuse / Specular / Coat / Fuzz / Transmission. It depends on
  parameters only, not on the view. `roughness_to_alpha_aniso` gives the alphas
  (transmission uses `specular_roughness.max(0.01)`).
- **MaterialX closures:** each `Prepared` leaf has a selection weight (`select`,
  normalised by `select_total`) and its lobe. Specular leaves store GGX alphas
  directly.
- `PreviewSurface` resolves to OpenPBR. `Emissive` and `ExitLambertian` are the only
  other `Material`s.

## Goals / Non-Goals

**Goals:**

- No guide samples are spent on lobes the guide cannot represent. On
  `caustic_guided.usda`, guided noise returns to the unguided level (see the spec
  scenario).
- Scenes without narrow lobes are untouched, bit for bit.
- A setting that restores today's behaviour exactly, for the A/B.

**Non-Goals:**

- Product sampling of guide × BSDF, for example Cycles' RIS mode. It is better on
  glossy lobes, but a larger change with its own MIS bookkeeping.
- Learning `α` per region (*Practical Path Guiding in Production*). It is
  complementary, and stays in the known-gaps list.
- Making `ΔEff` see rare bright paths. That gap stays. This change only stops it
  mattering for narrow lobes.
- Guiding primary vertices or changing the training cap. The exploration measured
  both, with no consistent gain.

## Decisions

### Scale `α` by the guidable share, instead of gating the vertex

`α' = α × f`, with `f = Σ guidable selection weight / Σ all selection weight`. The
mixture stays `α'·p_guide + (1−α')·p_bsdf` over the **full** BSDF mixture, so every
lobe keeps its exact pdf in the denominator. The estimator is unbiased for any `α'`
that depends only on the vertex, never on a random draw.

Alternatives considered:

- **Binary gate (guide iff the dominant lobe is rough).** Layered materials break
  it. A diffuse base under a smooth clear coat (plastic, car paint) either loses all
  guiding on the base or keeps wasting samples on the coat.
- **Replace only the rough lobes' sampling with the guide** (a mixture of guide and
  narrow lobes). Better in principle, but the pdf has to be split per lobe for every
  material type, and the NEE side must mirror it. Scaling `α` gets most of the gain
  with one scalar.

### Delta and subsurface lobes count in the denominator, never the numerator

A thin-walled window is mostly a delta transmission lobe. If the denominator held
only continuous lobes, a rough reflection would make `f = 1`. Delta samples would
then run half the time at twice the weight: the same waste this change removes. So
they count as weight the guide cannot help. Subsurface entry is a delta sample in
the integrator (`subsurface: Some`) and is treated the same.

### "Roughness" is the alphas' geometric mean, back in perceptual units

A lobe is guidable when `(ax·ay)^(1/4) ≥ threshold`. For an isotropic OpenPBR lobe
this is exactly `specular_roughness` (or `coat_roughness`), since `α = r²`. Closure
specular leaves store alphas, so the same formula applies to both paths and the
setting reads in the units artists author. Diffuse, sheen/fuzz and translucent lobes
are always guidable.

The geometric mean lets a strongly anisotropic lobe, narrow along one axis, count as
guidable. See Risks.

### Snap `f` at 0.01 and 0.99, and make threshold 0 mean "off"

- **Below 0.01: unguided.** OpenPBR floors absent lobes (diffuse at 1e-4, coat and
  fuzz at 1e-6), so smooth glass has `f ≈ 1e-4`, never 0. Snapping makes it a fully
  unguided vertex: no `field.sample`, no `field.pdf`, no training sample.
- **Above 0.99: full `α`.** This keeps diffuse-dominated scenes bit-identical. A
  white OpenPBR wall otherwise loses about 1e-6 of share to its floored, smooth coat.

Snapping is a deterministic function of the vertex, so it introduces no bias.
Threshold 0 skips the computation and uses `α` unscaled, including at delta and
subsurface lobes. That is exactly today's behaviour, which makes it the honest A/B
CLAUDE.md asks of a switch. A USD setting covers it, so no environment switch is
added.

### One computation per vertex, threaded to every use

`trace_path` computes `f` once per surface vertex where guiding is active (training
or trained), right after the `ShadingPoint` exists. It passes `α'` to
`sample_bounce_direction` and to the NEE `bounce_pdf`. Delta compensation becomes
`1/(1−α')`.

Training records a sample only where `f > 0` after snapping, so the first, untrained
pass already skips glass. The training gate uses `f`, not `α'`, because `α'` is also
0 wherever the field is still untrained.

### Where the query lives

`ShadingPoint::guidable_fraction(threshold) -> f32` dispatches on the private
`Resolved` enum:

- **OpenPBR (plain or resolved):** computed from `LobePmf::from_params` and the
  lobe alphas, as `ResolvedOpenPBR::guidable_fraction` in `openpbr/`.
- **Closure:** computed from `leaves()`.
- **`Resolved::Material(dyn)`:** a new `Material::guidable_fraction` defaulting to
  1.0, which is right for `ExitLambertian`, and `Emissive` never scatters.

crust-mtlx stays free of guiding vocabulary: the closure code lives in crust-core's
`material/closure/`.

## Risks / Trade-offs

- **[Risk] The default threshold is a guess.** 0.1 sits between the measured bad
  case (0.05) and the roughly neutral one (0.2). → A threshold sweep (0.05, 0.1, 0.2,
  0.3) on `caustic_guided.usda` at roughness 0.05 and 0.2 and on a glossy guided
  scene fixes the default before landing. If the measurement disagrees, the spec
  default changes with it.
- **[Risk] Anisotropic narrow lobes slip through the geometric mean.** → Accepted,
  and documented. Brushed metal under guiding is rare in the sample set. The cost is
  noise, never bias.
- **[Risk] NEE and bounce disagree on `α'`.** That would be a silent MIS bias. →
  `α'` is computed once and passed to both. A test renders a scene with a partly
  guidable material through `--strategy` variants and checks the guided and
  unguided means agree, in the style of `guided_render_is_unbiased`.
- **[Trade-off] An extra pmf evaluation per guided vertex.** It is a few dozen
  flops, already paid twice per vertex (scatter and eval), and only with guiding on.
  Unguided renders don't run it.

## Migration Plan

None. The new setting defaults to on. Scenes that relied on guiding narrow lobes
can author `crust:guidingRoughnessThreshold = 0`.
