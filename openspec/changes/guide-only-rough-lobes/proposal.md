## Why

Path guiding steers every continuous lobe, including narrow ones it cannot help. The
guide learns where light *arrives* at a point, not where that light would leave
through the material's lobe. At a narrow lobe, guide samples mostly land outside it.
Meanwhile BSDF samples run only `1 − crust:guidingProb` of the time, so wherever the
guide density is near zero they carry `1/(1−α)` = 2× their weight.

`unbiased-guided-pass-blend` measured this on `caustic_guided.usda`, a glass ball
with `specularRoughness` 0.05 under a small light (256², 1024 spp, 4 seeds,
`--indirect-clamp 0`):

- At the glass vertices, 36% of guide samples contribute nothing, and BSDF-branch
  weights reach 2× what BSDF sampling alone gives.
- With guiding on, shadow relMSE is **2.56**, against **0.42** unguided.
- Switching guiding off at the glass alone gives **0.41**, with the energy unchanged.

Since the blend fix, that scene's efficiency check keeps the guided final pass, so
this noise now ships. Production guiders avoid it by guiding only lobes above a
roughness threshold. Cycles with Open PGL is one example.

## What Changes

- **Guiding scales with how much of the material is rough.** At each guided vertex
  the guide probability becomes `α' = crust:guidingProb × f`. `f` is the share of the
  material's lobe-selection weight held by lobes the guide can help:
  - diffuse, sheen/fuzz and translucent lobes always count;
  - specular, coat and rough-transmission lobes count when their roughness is at
    least the threshold;
  - delta lobes (thin-walled transmission) and subsurface entry never count.

  A vertex with `f` below a small cutoff is not guided at all: no field lookups,
  no training samples. The estimator stays unbiased, because `α'` depends only on
  the vertex.
- **One `α'` per vertex, everywhere it is used.** The guide/BSDF coin, the mixture
  pdf, the delta-lobe `1/(1−α')` compensation and the NEE MIS weight's bounce pdf all
  use the same value. This keeps the NEE ↔ bounce pair consistent.
- **New setting `float crust:guidingRoughnessThreshold`**, in OpenPBR perceptual
  roughness. Default **0.1**, provisional: the tasks confirm it by measurement before
  it lands. `0` restores today's behaviour (every continuous lobe guided), which is
  also the A/B.
- **Training skips unguided vertices**, so a glass surface no longer feeds incident
  radiance into spatial cells it shares with nearby diffuse surfaces.
- **Docs:** the narrow-lobe limitation from `unbiased-guided-pass-blend` is retired or
  updated, and the `crust:guidingProb` page states its existing 0.1–0.9 clamp.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `rendering`: a new requirement that guiding scales with the material's rough
  fraction and leaves narrow and delta lobes to BSDF sampling. It is ADDED alongside
  "Opt-in path guiding", which `unbiased-guided-pass-blend` is already modifying.
- `usd-scene-import`: "Render settings from USD with defaults" gains
  `crust:guidingRoughnessThreshold`.

## Impact

- `crates/crust-core/src/material/`:
  - a per-hit "guidable fraction" query on `ShadingPoint`, implemented for OpenPBR
    (from `LobePmf` and the lobe alphas) and for MaterialX closures (from each leaf's
    selection weight and alphas);
  - a default for other `Material`s.
- `crates/crust-core/src/tracer/path.rs`: `sample_bounce_direction`, the NEE bounce
  pdf, and the training record take the vertex's `α'`.
- `crates/crust-core/src/tracer/settings.rs`, `scene/usd_import/settings.rs`: the new
  setting.
- Tests:
  - per-material fraction tests;
  - a bitwise check that threshold 0 reproduces the old behaviour;
  - an end-to-end noise test on `caustic_guided.usda`.
- Docs: `site/content/docs/usd/render-settings.md`,
  `site/content/docs/architecture/limitations.md`,
  `openspec/specs/rendering/design.md`.
- Performance: one pmf-weighted sum per guided vertex. The pmf is already computed
  per query today. Unguided renders are unaffected and stay bit-identical.
- Depends on `unbiased-guided-pass-blend`, for `samples/caustic_guided.usda` and its
  measurements. Implement on top of that branch.
