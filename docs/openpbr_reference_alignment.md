# OpenPBR reference alignment

This document records the alignment work done on `crust-core`'s OpenPBR
übershader (`crates/crust-core/src/material/openpbr.rs` +
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
| 5 | Metal Fresnel was plain Schlick on `base_color`; no F82 edge tint, no `base_weight`/`specular_weight`, no thin film on metal | F82-tint model (`fresnel_f82_tint`, `F0 = base_color·base_weight`, edge tint `specular_color`, lobe scaled by `specular_weight`) + per-channel-IOR thin film on the metal path |
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

## Remaining gaps vs. the Adobe reference

Known, deliberate, and recorded here so nobody rediscovers them:

- **Microfacet multiple-scattering energy compensation** — Adobe adds
  LUT-driven MMS lobes (dielectric + metal) and scales diffuse by a
  *directional* specular energy complement; crust uses a flat
  `(1 − F_avg)` coupling. Closing this means porting the Apache-2.0 LUT
  data tables. This is the largest remaining quality gap at high roughness.
- **Random-walk subsurface entry** — non-transmissive SSS materials never
  refract into their interior; they use the tinted-diffuse (EON)
  approximation. Needs an interface refraction event for the SSS fraction
  and an exit strategy (module header "Phase 5").
- **`specular_weight` semantics** — crust scales F0 directly; Adobe remaps
  F0 back to an IOR (`ior_from_f0`), which also moves the TIR angle.
  Related: the coat-aware base-IOR ratio (TIR fix) and coat-induced
  specular roughening are skipped.
- **Fuzz** — Charlie sheen D × Imageworks visibility with a scalar
  `(1 − fuzz_weight)` layer approximation, vs. Adobe's Zeltner LTC sheen
  with fuzz↔coat roughness cross-coupling.
- **Interior hits** — emission is not suppressed when a closed surface is
  hit from inside, and the coat is not reduced to transmission-tint-only
  there.
- **`geometry_opacity`** — unconsumed, by design on both sides: Adobe
  documents opacity as the host renderer's job (stochastic cutout); the
  tracer doesn't implement cutout yet.
- **Geometry inputs** — no normal mapping and no user tangents
  (`geometry_normal/tangent/coat_normal/coat_tangent`); frames are
  auto-generated (Duff et al.), so anisotropy has no authored orientation
  (Adobe additionally offers a (cos, sin) anisotropy-rotation extension).
- **Thin film + thin wall** — thin film applies to reflection only, not to
  thin-walled transmission (Adobe documents the same limitation).

Every item above is test-pinned where implemented; the shader's regression
suite lives in `openpbr.rs` (`cargo test -p crust-core`).
