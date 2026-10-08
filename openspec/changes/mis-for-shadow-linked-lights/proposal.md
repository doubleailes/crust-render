## Why

A light whose `collection:shadowLink` excludes some occluders is sampled by NEE alone at
every continuous vertex today (`LightList::nee_only`). The reason is sound: a bounce ray
is stopped by occluders the light's shadow rays ignore, so the two strategies disagree
on its visibility and cannot be MIS-combined. But it removes the half of MIS that keeps
glossy reflections of a light quiet. The design record already lists it as a gap
("noisier on glossy receivers").

Production rigs use exactly this setup, and on ALab it is the image's dominant noise
source. Measured 2026-10-08, frame 1004, 256 spp, `--light-samples 4
--light-selection learned`, two seeds:

- All four exterior lights (`lgt_sun_distant`, `lgt_sun_area_01/02`, `lgt_env_dome`)
  are shadow-linked: the louvered windows and the sky-dome sphere are excluded.
  **They are NEE-only.**
- The image's variance is 97% fireflies (0.1% of the pixels), all of it in
  `C<RG>[LO]` (direct glossy). Of that, **`lgt_sun_distant` is 66%**.
- A `--strategy bsdf` render collects 0 from either sun: bounce rays toward them end on
  the excluded louvers and sky dome. `--strategy light` and the default MIS have
  identical sun variances (3 545 and 2 983), so MIS is doing nothing for these lights.
- **With the three excluded prims deactivated, so that MIS is whole again**, the distant
  sun's direct-glossy variance falls from 3 545 to **103 (−97%)**, and the worst
  pixel's noise from 39 to 25.
- **Raising `--light-samples` does not reach it.** From 4 to 16, direct glossy moves
  5 340 → 4 688 while diffuse direct halves twice.

The design has the method: the light-group overlay, the one-frame layer offset that
changes only the seed, and the variance estimate. The first task records the
measurement in `docs/alab_profile.md`.

## What Changes

- **A restricted light gets a bounce-side twin that sees what its shadow rays see.**
  At a continuous surface or volume vertex, after the bounce direction is drawn, every
  restricted light whose emission that direction can reach is resolved along it with its
  own shadow ray, as NEE would test it. That shadow ray uses the light's shadow mask and
  the same transmittance. The light's emission found there is added with the bounce-side
  MIS weight.
- **NEE toward a restricted light gets its ordinary MIS weight** instead of 1. The two
  sides now estimate the same integrand: emission × the light's own visibility × BSDF.
- **The ordinary bounce ray no longer collects a restricted light at a continuous
  vertex**, under any strategy. The twin owns that contribution. Today `--strategy
  bsdf` collects it there through the physical occluders; after the change, BSDF-only
  honours shadow links too. Light-only, BSDF-only and MIS then agree in expectation on
  a scene whose link matters, which they do not today.
- **Delta vertices are unchanged.** After a mirror or glass bounce, the light is still
  found at full weight through the real occluders, so the documented "a mirror shows
  the physical shadow" gap stays.
- **A restricted dome stays NEE-only**, as now. Its twin would cost a shadow ray on every
  bounce, and on ALab the dome carries no measurable noise. This is a decision with its
  measurement in the design, not a gap left by accident.
- **Unlinked scenes, and scenes whose links never restrict a shadow set, are
  bit-identical.**
- **Images change where a restricted light lights a continuous surface:** same mean,
  lower variance. `power_mis_matches_nee_only_for_a_shadow_linked_light`, which pins
  today's equality bitwise, is replaced by an agreement-in-expectation test.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `lighting`:
  - the "Shadow linking" requirement: a restricted light is MIS-combined through a
    shadow-linked bounce twin at continuous vertices, with domes excepted;
  - the documentation requirement: the NEE-only statement narrows to restricted domes.

## Impact

- **Code:**
  - `crust-core/src/tracer/path.rs`: the bounce-side twin at the surface and phase
    vertex; `bounce_emission_weight`, `escaped_emission` and `escaped_split` (a pair, so
    all three); the NEE weights in the surface and volume NEE.
  - `crust-core/src/light/`: a ray–shape intersection for area-light shapes, and a
    direction-support test for distant lights.
  - `crust-core/src/scene/usd_import/light_links.rs`: `nee_only` becomes "restricted",
    plus "twin or not".
- **Light path expressions:** the twin's contribution routes as the vertex's sampled
  lobe followed by `L` with the light's tag, through the same split the bounce-hit
  emission uses. `C.*[LO]` must stay the beauty bit for bit.
- **Performance:** at each continuous vertex, one cheap support test per restricted
  non-dome light, and a shadow ray only when the bounce direction actually reaches
  that light. On ALab that is a ~3% chance of hitting the 20° sun cone, or a 10 cm
  rect. Unlinked scenes pay nothing: the existing `links().is_none()` early-out. The
  budget is +5% render time on ALab at equal spp, measured with `scripts/bench_ab.sh`.
- **Documentation:**
  - the lighting design record § Shadow linking and § Known gaps;
  - `site/content/docs/architecture/limitations.md`, whose light-linking gaps line about
    shadow-linked lights being noisier on glossy surfaces narrows to domes;
  - `docs/alab_profile.md`, with the before/after measurement.
