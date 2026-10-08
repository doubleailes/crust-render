# OpenPBR reference alignment

This document records the alignment work done on `crust-core`'s OpenPBR
übershader (`crates/crust-core/src/material/openpbr/` +
`material/brdf.rs` + `medium.rs`) against the two public references:

- **MaterialX nodegraph** — the normative surface-shader graph shipped with
  the [OpenPBR specification](https://academysoftwarefoundation.github.io/OpenPBR/)
  (`open_pbr_surface` nodedef, v1.1.1). Defines *what* the layers compute,
  but not sampling.
- **Adobe OpenPBR BSDF** — [adobe/openpbr-bsdf](https://github.com/adobe/openpbr-bsdf)
  (Apache 2.0), the production OpenPBR 1.1 eval/sample/pdf implementation
  extracted from Adobe's Eclair renderer. Defines *how* a real path tracer
  evaluates and importance-samples the model, and is the source for every
  formula cited below.

Crust's architecture was already the same shape as Adobe's — a fixed set of
lobes, one-sample MIS with heuristic lobe-selection weights, and a full
mixture value/pdf recombination on every sample (`eval_all` / `pdf_all`
mirror Adobe's aggregate-lobe combine step). The work below closed the
formula-level gaps.

## Phase 1 — MaterialX nodegraph divergences (commit `ea69a4e`)

Five divergences from the reference graph, fixed to match:

| # | Divergence | Fix |
|---|------------|-----|
| 4 | `transmission_color` applied at the interface *and* as Beer-Lambert absorption when `transmission_depth > 0` (double color) | Interface BTDF is untinted when a medium owns the color (`if_transmission_tint`) |
| 5 | Metal Fresnel was plain Schlick on `base_color`; no F82 edge tint, no `base_weight`/`specular_weight`, no thin film on metal | F82-tint model (`fresnel_f82_tint`, `F0 = base_color·base_weight`, edge tint `specular_color`) + per-channel-IOR thin film on the metal path. The lobe is *not* scaled by `specular_weight` — see the note below |
| 6 | `coat_color` tinted the coat *reflection* | Coat reflection is untinted; `coat_color` attenuates light transmitted to the substrate |
| 7 | Emission ignored the coat | View-dependent coated emission via `Material::emitted_directional` (later upgraded in Phase 5 below) |
| 8 | Ad-hoc anisotropy remap (signed ±1) | Spec formula: `αₓ = r²·√(2/(1+(1−a)²))`, `αᵧ = (1−a)·αₓ`, `a ∈ [0,1]` |

The F82-tint formula and the anisotropy remap were later verified to be
*identical* to Adobe's `openpbr_metal_schlick_with_f82_tint` and
`openpbr_compute_anisotropic_alpha`.

## Phase 2 — the ranked Adobe list (all five items complete)

Ranked by effort-to-payoff during the Adobe comparison, then implemented in
order:

### 1. EON diffuse (`1304253`)

The base diffuse slab is the **EON** model (energy-preserving Fujii
Oren-Nayar, Portsmouth et al.) that the spec names — replacing Disney
retro-reflection. Single-scattering Fujii lobe plus the analytic
multiple-scattering compensation lobe
`ρ_ms/π · (1−E(μ_o))(1−E(μ_i))/(1−Ē)`; a white-albedo surface reflects
exactly unit energy at any roughness (pinned by hemisphere-quadrature
test). Runtime uses the quartic directional-albedo fit; the exact closed
form is kept as a test reference. Presence weights fold *into* the EON
albedo (as Adobe does) because the multi-scatter term is nonlinear in ρ.
Sampling stays cosine-hemisphere, matching the reference.

### 2. Cauchy/Abbe physical dispersion (`3ecf0f7`)

`dispersive_ior` evaluates a two-term Cauchy fit `n(λ) = A + B/λ²`,
anchored so `n(λ_d) = n_d` exactly at the Fraunhofer d line (587.6 nm) and
the fit's Abbe number `(n_d−1)/(n_F−n_C)` equals
`transmission_dispersion_abbe_number`, evaluated at the sRGB primary
wavelengths (615/545/465 nm, shared with thin film).
`transmission_dispersion_scale` divides the Abbe number (scale 1 =
physical glass). IORs below 1 disperse via their reciprocal. Replaces the
former linear `(n_d−1)/V` spread. Note green is *not* pinned to `n_d`
(545 nm sits blue of the d line) — physically correct.

### 3. Thin-wall window model (`98b8a1e`)

Thin-walled transmission was a straight-through delta with a flat tint. It
now carries window energy: transmittance `(1−R)/(1+R)` (both interfaces
plus all internal bounces, exact dielectric Fresnel at the view angle),
the matching reflection boost `2R/(1+R)` on the dielectric-specular side
(a clear sheet reflects + transmits exactly unit energy), and
`transmission_color` interpreted as normal-incidence transmittance raised
to the in-sheet path length `1/cos θ_refracted`. The lobe stays delta
(front/back refractions cancel).

### 4. `transmission_scatter` + van de Hulst subsurface (`ae1287b`)

Two previously ignored parameter groups now reach the carried-medium
transport:

- `Medium::from_transmission` takes `transmission_scatter` /
  `transmission_scatter_anisotropy`: `σₜ = −ln(color)/depth`,
  `σₛ = scatter/depth`, absorption shifted by gray to stay non-negative
  (per spec), anisotropy drives the HG phase.
- `Medium::from_subsurface` inverts the observed albedo with the **van de
  Hulst** mapping (`α_ss = (1−s²)/(1−g·s²)`) — the naive `σₛ = σₜ·A`
  under-scattered badly (observed 0.5 needs α_ss ≈ 0.91).
- `OpenPBR::interior_medium` blends both volumes by their dielectric
  fractions `t` and `(1−t)·s` (transmission supersedes subsurface) with
  scattering-weighted phase anisotropy, exactly as Adobe's
  `openpbr_prepare_volume`. Inert interiors attach no medium at all.

Scope note: the subsurface volume is entered *through the refractive
interface*, i.e. on materials that also have `transmission_weight > 0`.
Pure SSS without transmission still renders as tinted diffuse (random-walk
entry is future work — see gaps below).

### 5. Coat passage model (`fd6592b`)

`coat_color` is the *round-trip* absorption at normal incidence: each
passage applies `√coat_color` raised to the refracted in-coat path length
`1/cos θ_t` (Snell at the coat IOR), times that direction's Fresnel
transmission. The base attenuation is `passage(view) · passage(light) ·
darkening` — per-direction and view-dependent, so tinted coats saturate
toward grazing; at normal incidence the round trip recovers exactly
`coat_color·(1−F0)²`. Coated emission reuses one outbound passage
(matching `openpbr_compute_emission`), replacing the earlier MaterialX
`generalized_schlick_edf` approximation.

A note on the two emission fields, since the MaterialX path now writes them.
`emission_luminance` is used as a **raw linear multiplier**, not as photometric
nits: the only thing done with the pair is `emission_color × emission_luminance`
(`OpenPBR::emitted`), so their *product* is the contract and either field alone
is a presentation choice. The MaterialX reduction factors by peak channel, which
keeps `emission_color` a chromaticity; the importer's `UsdPreviewSurface` path
puts the whole value in the colour and 1.0 in the scalar. Both are exact, and
code reading one field without the other is reading a convention that is not
guaranteed.

The darkening term is the spec's `Δ = (1 − K̄)/(1 − K̄·E_base)` with
`K̄ = 1 − (1 − F0)/η²` (the coat underside's hemispherical reflectance, TIR
included — ≈0.57 at η = 1.5), faded as `1 + coat_weight·coat_darkening·(Δ − 1)`.
An earlier form computed `E/(1 − K̄(1 − E))` with `K̄ ≈ F0`, which tends to `E`
for dark bases and so applied the base colour twice (a 0.05 car-paint
substrate was scaled by ~0.055); the corrected ratio is bounded below by
`1 − K̄`.

The passage and the darkening apply to **different** sets of lobes, and the
code keeps them in separate functions (`coat_attenuation`, `coat_darkening`)
to make that hard to re-merge. The `(1 − F)` passage is geometry — every
photon that reaches the substrate pays it whatever lobe it then meets — so it
multiplies everything under the coat. Δ is a substrate-*albedo* term, computed
from `base_color`, so it multiplies only the lobes whose reflectance
`base_color` describes: the diffuse slab, the metal slab (whose F0 literally
*is* `base_color·base_weight`) and emission, which originates inside the
substrate. It must not reach the base **dielectric** lobe, whose reflectance is
~4% and white: running that through a Δ derived from a saturated base both
dimmed and tinted it, so a clearcoated red plastic's white highlight came out a
dim pink one.

Note also that the MaterialX importer sets `coat_darkening = 0` on every coat
it produces. MaterialX's `layer` node is single-scattering (`base·(1 − F) +
top`) and models no bounce series, so imposing Δ on a promoted coat would
darken a substrate the source material never darkened.

## Fuzz: Zeltner's sheen over a directional layer

The fuzz is Adobe's fuzz lobe (`impl/openpbr_fuzz_lobe.h`), transcribed
operation for operation (`brdf::ZeltnerSheen`, `lobes::FuzzLayer`):

- **The lobe** is Zeltner, Burley and Chiang's LTC sheen with Disney's
  "Volume" table: 32 × 32 entries over (`fuzz_roughness`, cos θ_o) of the
  inverse LTC coefficients and the directional albedo `R`, read bilinearly as
  Adobe's array mode reads them. `fuzz_roughness` is the LTC's α as authored,
  with no floor. The value toward `ω_i` is `fuzz_color · w · R · D_ltc(ω_i)`,
  where `D_ltc` already carries the cosine, and the lobe samples `D_ltc`
  exactly, so a fuzz sample weighs `fuzz_color · R`. An LTC sheared below the
  plane loses that share of its samples, as in Adobe.
- **The layering** passes `1 − w · R(ω_o)` to everything beneath (coat,
  specular, metal, diffuse, transmission) and to the emission. This is the view
  side of Adobe's attenuation; Adobe's default
  (`OPENPBR_RECIPROCAL_COAT_AND_FUZZ = 0`) omits the light side too, so the
  factor belongs to the shading point, not to the light.
- **The coat under the fuzz** is roughened as Adobe's `openpbr_prepare_lobes`
  roughens it: `⁴√min(1, r⁴ + avg(fuzz_color) · r_f · 0.005 · r_f⁴)`, faded in by
  `fuzz_weight`.
- **Lobe selection** weighs the fuzz by `w · R(ω_o) · max(fuzz_color)` and
  everything beneath by `1 − w · R(ω_o)`, as Adobe's `openpbr_sheen_probability`
  does.

It is a different look from the Charlie sheen it replaced, not a refinement
of it. Facing the camera, a smooth fuzz all but vanishes and a rough one
reflects about twice as much; the albedo against crust's former Imageworks
fit:

| `fuzz_roughness` | cos θ_o | Zeltner `R` | Imageworks `E` (before) |
|---|---|---|---|
| 0.1 | 0.05 | 0.212 | 1.000 |
| 0.3 | 1.0 | 0.0008 | 0.052 |
| 0.3 | 0.25 | 0.166 | 0.431 |
| 0.5 | 0.5 | 0.154 | 0.259 |
| 1.0 | 1.0 | 0.342 | 0.157 |
| 1.0 | 0.25 | 0.620 | 0.399 |

One departure from Adobe, deliberately: Adobe gives the fuzz no presence when
a closed surface is hit from inside (`back_facing && !thin_walled`). crust
keeps it, as it keeps the coat and the emission there. Fuzz's main use is
cloth, which is usually an open mesh not authored thin-walled, and Adobe's
rule would strip the back of every curtain. The oracle's `interior` deviation
covers it with the rest of that gap.

The Adobe oracle matches every fuzz-only case, from roughness 0 to 1 and from
grazing to head-on, within 1e-4, emission through the fuzz included.

## AOV outputs: the diffuse filter

`lobes::diffuse_filter` (behind the `diffuse_albedo` AOV and the raw light AOVs,
`openspec/specs/aovs/`) reports the diffuse lobe's colour, without lighting:
`ρ · (1 − F̄) · base_atten · Δ`. It is the colour factor of `eval_split`'s
diffuse share, the part of the lobe that does not depend on direction.
**Neither reference has such an output.** MaterialX and Adobe evaluate the BSDF;
neither exports "the diffuse colour" as a separate quantity. The term is
crust's, after V-Ray's `DiffuseFilter`. Each factor maps onto the references as
follows:

| factor | crust | MaterialX nodegraph | Adobe |
|--------|-------|---------------------|-------|
| `ρ` | `mix(base_color, subsurface_color, subsurface_weight) · base_weight · (1 − base_metalness) · (1 − transmission_weight)`, the EON albedo `eval_diffuse` uses | `base_color · base_weight` into the diffuse BSDF's colour, with metalness, transmission and subsurface applied as `mix` weights over it | the diffuse lobe's albedo, under the same presence weights |
| `1 − F̄` | the flat energy split of the base specular (`f0_from_ior(specular_ior)`) | the specular `layer`'s throughput, `1 − E_spec(μ_v)`: **directional** | the directional specular energy complement: **directional** |
| `base_atten` | `1 − fuzz_weight · R(ω_o)`, Adobe's view-side fuzz attenuation (see "Fuzz" above): **view-dependent**, light-independent | the fuzz `layer`'s throughput: **directional** | the sheen layer's throughput, the same factor |
| `Δ` | `coat_darkening`, the spec's `(1 − K̄)/(1 − K̄·E_base)` faded by `coat_weight · coat_darkening` | none: MaterialX's `layer` is single-scattering, and the importer sets `coat_darkening = 0` | the same Δ (see "Coat passage model" above) |
| coat passage | **left out**: it depends on the view and light directions, so it belongs to the light, not the colour | the coat `layer`'s throughput | the coat passage |

Where both references are directional, crust's filter uses crust's own
factor. The filter is therefore exactly what crust's `eval_split` multiplies,
so `raw × filter` reproduces crust's lighting. It is not a reference quantity.
`base_atten` depends on the view but not on the light, so it stays in the
filter: the shading point knows its view (`ShadingPoint`'s `cos_o`). Closing
the `(1 − F_avg)` gap below would make the filter's second factor depend on
the light too; it would then move into the light, like the coat passage. So
would Adobe's reciprocal fuzz mode, which is why crust follows the default.

## Remaining gaps vs. the Adobe reference

Known, deliberate, and recorded here so nobody rediscovers them. Each gap is
also a named deviation in the Adobe oracle (`crust-core/tests/adobe_oracle.rs`),
which replays reference values from Adobe's `openpbr-bsdf` at a pinned commit
(`scripts/adobe_oracle.py` regenerates them). A deviation is the condition on
the inputs under which crust may differ, and a bound on how far: the worst of a
case's albedo error (absolute), emission error (relative to `max(1, |e|)`) and
per-direction value error (relative to `max(0.05, |f·cos|)`). A case outside
every deviation must match within 2e-3; a case under several is excused up to
the sum of their bounds; and a deviation every case passes without fails the
test, so closing a gap means deleting its rule. The bounds below are the ones
measured on 2026-10-08:

- **Microfacet multiple-scattering energy compensation** — Adobe adds
  LUT-driven MMS lobes (dielectric + metal) and scales diffuse by a
  *directional* specular energy complement; crust uses a flat
  `(1 − F_avg)` coupling. Closing this means porting the Apache-2.0 LUT
  data tables. This is the largest remaining quality gap at high roughness.
  It is also an energy *gain*, not just a loss: the flat 0.96 leaves the
  diffuse almost untouched while the specular lobe adds up to ~0.35 at
  grazing, so a coloured surface desaturates toward its silhouette. A
  hand-rolled `1 − F(μ_v)` substitute is not a fix — it breaks reciprocity
  unless symmetrised as `√((1 − E(μ_v))(1 − E(μ_l)))`.
  Oracle: `metal-no-mms` (11.9), `dielectric-specular` (0.0317, which also
  covers Schlick against Adobe's Fresnel), `specular-diffuse-coupling` (2.19),
  and `diffuse-flat-coupling` (0.169: the flat `1 − F_avg` dims the diffuse
  even at `specular_weight = 0`, where Adobe has no interface to take energy,
  by 4% head-on and more toward grazing).
- **Random-walk subsurface entry** — non-transmissive SSS materials never
  refract into their interior; they use the tinted-diffuse (EON)
  approximation. Needs an interface refraction event for the SSS fraction
  and an exit strategy (module header "Phase 5"). Both now exist for the
  MaterialX `subsurface_bsdf` (`crust-core/src/subsurface.rs`: the walk, and
  the exit Lambertian the tracer resumes on); the native shader would need a
  subsurface lobe whose selection returns a `ScatterSample::subsurface`
  entry, and its `from_subsurface` van de Hulst medium replaced by the walk's
  Chiang remap, since the two invert the albedo differently.
  Oracle: `subsurface-as-diffuse` (2.73).
- **`specular_weight` semantics** — `specular_weight` weighs the **dielectric
  base's** specular interface and nothing else: it scales the finished
  dielectric lobe, and the metal lobe takes its coverage from `base_metalness`
  alone. Scaling both by it left the two unable to hold independent coverage,
  which the MaterialX reduction needs when a graph mixes a conductor and a
  dielectric at different weights. crust scales the finished dielectric
  lobe by `specular_weight`; Adobe remaps F0 back to an IOR
  (`ior_from_f0`), which also moves the TIR angle. crust used to scale F0
  itself, which was worse than merely approximate: Schlick is
  `F0 + (1 − F0)(1 − cosθ)⁵`, so an F0 of zero still returns 1.0 at grazing
  and a `specular_weight = 0` surface — every unbound prim — carried a
  full-strength white rim. Scaling the lobe makes it linear in the weight;
  the IOR remap is still not done.
  Related: the coat-aware base-IOR ratio (TIR fix) and coat-induced
  specular roughening are skipped.
  Oracle: `transmission-under-specular` (0.887) for the remap, which moves
  the transmission with the reflection; `transmission` (0.103) for the
  transmission lobe itself; `coat-over-base` (0.709) for the coat's effect on
  the base; `coat` (0.178) for the coat lobe itself, which drifts from
  Adobe's toward grazing (0.17 in albedo at cos θ_o = 0.1, 0.002 at 0.6).
- **Interior hits** — emission is not suppressed when a closed surface is
  hit from inside, and the coat is not reduced to transmission-tint-only
  there.
  Oracle: `interior` (3.06).
- **`geometry_opacity`** — not a BSDF input on either side: Adobe documents
  opacity as the host renderer's job (stochastic cutout), and crust's host
  does it (`Material::opacity`): below 1 the surface is met with that
  probability and otherwise passed through.
- **Geometry inputs** — no normal mapping and no user tangents
  (`geometry_normal/tangent/coat_normal/coat_tangent`); frames are
  auto-generated (Duff et al.), so anisotropy has no authored orientation
  (Adobe additionally offers a (cos, sin) anisotropy-rotation extension).
  Oracle: `anisotropy` (0.603). The frames agree at the oracle's normal, so
  that bound is the anisotropic lobe itself, not its orientation.
- **Thin film + thin wall** — thin film applies to reflection only, not to
  thin-walled transmission (Adobe documents the same limitation).
  Oracle: `thin-film` (0.345), for the interference itself, which in Adobe
  reaches the base even with no specular lobe (`specular_weight = 0`). The
  thin-walled window model, and a thin-walled diffuse, match Adobe in every
  case the fixture holds, so neither has a rule.

Every item above is test-pinned where implemented; the shader's regression
suite lives in `openpbr/` (`cargo test -p crust-core`).
