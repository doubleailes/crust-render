# Proposal

## Why

`subsurface_bsdf` was the one MaterialX closure crust shaded as something else:
a diffuse in its colour, with no radius and no anisotropy, reported per
material. Every `open_pbr_surface` with a `subsurface_weight` and every
`standard_surface` with a `subsurface` rendered as plastic — no translucency, no
colour bleeding through thin parts, no softened terminator. It is the first
entry of the MaterialX known gaps.

NVIDIA's Typhoon (`typhoon/main` of NVIDIA-Omniverse/OpenUSD, hdEmbree) already
renders the same closure tree crust follows, and its subsurface is a
self-contained random walk (`renderer/integrator/sss.cpp`, itself a port of
Blender Cycles' `subsurface_random_walk.h`, Apache 2.0). Porting it keeps
crust's MaterialX path the reference's.

## What Changes

- **`subsurface_bsdf` becomes a random walk.** The leaf has no value toward any
  direction (no NEE at the entry, as in Typhoon); selecting it is a delta event
  whose direction refracts through the dielectric layered over it (Typhoon's
  `SampleSubsurfaceEntry`; IOR 1.5 and roughness 0.5 with none).
- **A new `crust-core` module, `subsurface.rs`**, ports `ty::RandomWalkSSS`:
  Chiang 2016 albedo inversion, channel MIS, forward/backward Dwivedi guiding
  with opposite-interface detection, the similarity relation, 256 bounces. It
  traces the entered geometry alone.
- **The integrator resumes at the walk's exit** on a white Lambertian with no
  emission (Typhoon's and Cycles' synthetic exit). The walk spends no path
  depth. `--stats` reports walks, their exit share, length and rays.
- **The load warning for `subsurface_bsdf` is removed**: it is no longer
  approximated. A zero radius still shades as the diffuse.
- **A fixture**, `samples/materialx_subsurface.usda` + `.mtlx`: one material per
  way to reach the leaf (OpenPBR, Standard Surface, a bare `subsurface_bsdf`).

## Capabilities

### Modified Capabilities

- `materials`: `subsurface_bsdf` is rendered as a random walk rather than
  reported and approximated.

## Impact

- Renders of MaterialX materials with a live subsurface leaf change (they
  become translucent, and darken slightly: see design). Every other scene is
  bit-identical; cornellbox costs +0.49% instructions.
- Depends on `add-mtlx-surface-shaders` being archived first: its reporting
  requirement is the one this change modifies.
