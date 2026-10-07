# Proposal

## Why

3D Gaussian splats are becoming a production scene representation: OpenUSD 26.03
standardised them as `UsdVolParticleField3DGaussianSplat`, Arnold 7.5.2 renders
them, and another major production renderer (in beta) renders them straight
from that USD prim — its vendor tutorial renders a 1,256,332-splat outdoor
capture on CPU, with the capture's baked lighting and no relighting.
Crust loads scenes only from USD and is CPU-only, so the standard prim is exactly
its way in: a captured set rendered behind, around, and reflected in CG.

## What Changes

- **Import `ParticleField3DGaussianSplat` prims** from `.usda` / `.usdc` / `.usdz`,
  following the schema's own rules: linear scales and opacities, real-first
  quaternions, float attributes preferred over their `half` twins, per-attribute
  length rules (too long: truncated; too short: ignored, falling back to unit
  scale, no rotation, opacity 1, and SH DC 0.5 grey), degenerate particles
  dropped with one warning per prim. No PLY reader is added: PLY → USD conversion
  stays outside the renderer, as it does in that production renderer.
- **Render splats as emission-only stochastic presence.** Each particle is a
  Gaussian ellipsoid of 3σ support; a ray meets it at the point of peak response
  along the ray, `t*`, with presence `α = opacity · exp(−½ d²(t*))`. A met splat
  ends the path with its spherical-harmonic radiance `max(0, 0.5 + Σ c·Y(ω))`
  evaluated toward the ray; a passed splat is crossed like a cutout. Camera and
  indirect rays see splats, so a capture is seen directly, in reflections and
  through refraction, and lights CG through BSDF-sampled bounces.
- **Splats do not cast shadows** (the capture's shadows are baked in) and are
  never light-list entries (they emit only through the material path, as the
  lighting invariant requires).
- **A new primitive in `crust-rt`**: a Gaussian-ellipsoid particle that reports
  its hit at `t*`, stays free of crust types, and costs a scene without
  particles nothing.
- **`--stats`** reports particle-field count, particle count and memory, and
  splat crossings per ray — the number that decides whether a k-nearest
  traversal is needed later.
- **User documentation**: a `usd/particle-fields.md` page and the new
  limitations.

Not in this change (each recorded as a known gap): relighting (a diffuse lobe on
splats, as Arnold's `gaussian_splat_shader` offers), shadow casting, surflet
kernels (`ParticleFieldKernelGaussianSurfletAPI` /
`…ConstantSurfletAPI`), motion blur of particles, instancing a particle field,
SIMD packets for particles, and a k-buffer traversal.

## Capabilities

### New Capabilities
- `particle-fields`: how USD particle fields (today, the 3D Gaussian splat prim)
  are imported, interpreted (kernel, presence, spherical-harmonic radiance,
  colour space) and transported (which rays see them, what meeting or passing
  one contributes), plus their statistics and known gaps.

### Modified Capabilities
- `intersection-kernel`: the kernel accepts a new primitive kind, Gaussian-ellipsoid
  particles, reporting their hit at the peak of the kernel's response along the
  ray; its known gaps name particles among the primitives without SIMD packets.

## Impact

- `crates/crust-rt`: new particle primitive (bounds, hit at `t*`, any-hit) and
  its storage; deterministic build unchanged for scenes without particles.
- `crates/crust-core/src/scene/usd_import/`: a `particle_field.rs` sibling and
  dispatch on the prim type name; reads raw attributes (openusd 0.7 predates the
  26.03 schema), including `half` arrays and `quatf` / `quath`.
- `crates/crust-core`: a splat "material" whose presence and `emitted_at` come
  from the particle data; reuse of the cutout pass-through machinery in
  `tracer/path.rs`; ray masks keep splats off shadow rays; `--stats` counters.
- `scripts/`: a fixture generator (Python, `usd-core` ≥ 26.03) for small
  synthetic splat files the tests load; nothing in the renderer depends on it.
- Memory: ≈ 250–330 B per particle at SH degree 3 in `f32` (≈ 0.4 GB for the
  1.26 M-splat reference capture including its BVH) — small next to Moana / ALab.
- Performance: zero cost for scenes without particle fields (pinned by
  instruction count); splat scenes are bounded by crossings per ray, measured
  before any traversal change.
- Docs: `site/content/docs/usd/particle-fields.md`, `docs/architecture.md`,
  and a design record `openspec/specs/particle-fields/design.md` on archive.
