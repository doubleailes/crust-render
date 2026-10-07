# Design

## Context

See `proposal.md` for the motivation and `specs/particle-fields/spec.md` and
`specs/intersection-kernel/spec.md` for the behaviour. This section covers what
already exists, and the constraints the splat data imposes.

**The data.** OpenUSD 26.03's `ParticleField3DGaussianSplat` (checked against
`usd-core` 26.8's `usdVol/schema.usda` and a production renderer's tutorial converter)
authors:

```usda
def ParticleField3DGaussianSplat "scan"
{
    point3f[] positions                         # count = len(positions)
    float3[]  scales                            # linear (converter: exp(log scale))
    quatf[]   orientations                      # (w, x, y, z), normalised
    float[]   opacities                         # linear [0,1] (converter: sigmoid(logit))
    uniform int radiance:sphericalHarmonicsDegree = 3
    float3[]  radiance:sphericalHarmonicsCoefficients (elementSize = (deg+1)², interpolation = "vertex")
    # + half twins: positionsh, scalesh, orientationsh, opacitiesh, …Coefficientsh
    # + uniform token projectionModeHint ∈ {perspective, tangential}
    # + uniform token sortingModeHint ∈ {zDepth, cameraDistance, rayHitDistance}
}
```

The kernel is defined by `ParticleFieldKernelGaussianEllipsoidAPI`: a unit-σ Gaussian
whose opacity is multiplied by the per-particle opacity, with 99.7 % of its support
inside radius 3. The converter copies the PLY's raw `f_dc` / `f_rest` coefficients,
so the 3DGS `+0.5` DC offset and the display encoding of the result are conventions
the schema does not state (see Decision 6 and Decision 7).

**What Crust already has.**
- *Cutouts* (`tracer/path.rs`, `pass_cutouts` / `cutout_through`): stochastic
  presence on the bounce side, restarting the ray past each passed hit
  (`hit_past`, `resume_before`) up to `MAX_CUTOUT_CROSSINGS = 256`, with a
  deterministic `∏(1 − α)` on the shadow side. Passing a cutout spends no depth
  and records no vertex.
- *The re-hit rule* ("A pass-through crosses each surface once"): the same
  primitive, from the same side, within `1e-3·t` of a crossing, is not crossed
  again.
- *Emission outside the light list*: a material that emits through `emitted_at`
  is collected at bounce arrival with the BSDF-only weight. The LPE `O` event
  already means "emission from anything not in the light list".
- *Ray masks* (`RayMask`, `MASK_ALL`) per geometry and per ray category.
- *Colour-space resolution* for USD colour attributes (`attrs.rs`,
  `attr_color_space` / `in_working`), with `srgb_texture` as a named space.
- *`crust-rt`* primitives as `PrimNode` variants (triangles in `Tri4` packets;
  spheres, disks, cylinders, curves scalar), with cubic curve spans stored inline
  in arrays of their own kind.

**Constraints.** `crust-rt` stays free of crust types; crust-core decodes no files
(USD attributes are not "assets", so the importer reads them directly as it does
meshes). No RNG outside openqmc. The USD import is single-threaded. Sizing target:
the reference capture, 1,256,332 particles at SH degree 3.

## Goals / Non-Goals

**Goals:**
- One integrator: splats go through the same path walk, cutout machinery and
  emission bookkeeping as every other surface. No raster pre-pass and no separate
  splat compositor.
- Every number about a splat can be verified exactly: the kernel's `t*` and `d²`,
  the SH evaluation, and the colour conversion each have a fixture with expected
  values produced by the reference toolchain.
- Zero cost when absent. Splat cost scales with splat crossings, and `--stats`
  measures those crossings.

**Non-Goals:**
- Matching a rasteriser bit for bit. 3DGS's screen-space 0.3 px dilation,
  per-splat (rather than per-ray) SH direction, and α-compositing in
  display-encoded space are rasteriser artefacts. They are documented as known
  differences and measured with relmse, not reproduced.
- Relighting, shadow casting, surflets, a k-buffer traversal, and SIMD particle
  packets. Each is a follow-up that this design leaves room for (see the
  `particle-fields` known gaps).

## Decisions

### 1. Splats are emitting cutouts hit at their peak (3DGRT-style), not a volume

A particle becomes a virtual hit at `t*`, the maximum of its Gaussian along the
ray, with presence `α = o·exp(−½d²)`. A met splat ends the path with its radiance.
A passed one is a cutout pass.

*Alternatives:*
- **Emissive heterogeneous volume** (`σ(x) = Σ σᵢGᵢ(x)`, delta tracking). This is
  the physically honest reading, but captures are trained against α-compositing,
  not the volume integral. The α→σ mapping is not exact, so the same data renders
  as a different, softer image. It also needs a spatial majorant grid that
  `volume.rs` lacks ("one global majorant per region" is a known gap). Worth
  revisiting for EVER-style captures trained volumetrically.
- **Convert to surfels or meshes**: lossy, and it discards view-dependent SH.
- **Deterministic k-buffer compositing** (sort k nearest `t*` and composite front
  to back). Lower variance per ray, but it needs a new kernel query and buys
  little on the bounce side, where stochastic presence usually stops within a
  few hits. Deferred until `--stats` crossings justify it.

### 2. Kernel: a particle geometry with its own inline arrays

`crust-rt` gets a particle geometry: per particle, the centre `μ` and the 3×3
world→normalised map `A = S⁻¹R⁻¹M⁻¹`, where `M` is the linear part of the prim's
composed transform. That is 12 `f32` = 48 B, stored SoA and inline like cubic
spans, with no per-particle pointer. The hit maths is parametrisation-invariant,
which matters because Crust's ray directions are not unit length:

```
o' = A(o − μ),  d' = A d
t* = −(o'·d') / (d'·d')          d² = |o' + t* d'|²
hit  iff  d² ≤ 9  and  t_min < t* < t_max;   report t*, prim_id = k, u = d²
```

- The bound is the exact AABB of the 3σ ellipsoid: half-extent along world axis
  `i` = `3·‖row i of A⁻¹‖`.
- Because the point at `t*` lies inside that bound, `t* ≥` the box entry, so
  ordinary nearest-hit pruning orders particles by `t*` correctly. This is the
  "ordering by peak, not by entry" scenario.
- The prim's transform is baked into each particle at import, so particle fields
  live in the top-level scene. Instancing a field is a known gap.
- The BVH builder uses object splits only for particle ranges. Clipping an
  ellipsoid for spatial splits buys little for millions of small, overlapping
  primitives, and the builder stays deterministic.
- `occluded()` (any-hit) treats a particle like any other primitive. Because of
  Decision 4 it is never asked about splats in practice.

*Alternative:* a `PrimNode::Particle` variant in the generic leaf. Rejected: it
adds an arm to the match every triangle leaf passes through, while a geometry kind
of its own leaves the triangle path's instruction stream untouched (Decision 9).

### 3. Opacity and radiance live in crust-core, indexed by `prim_id`

The kernel holds no opacity or colour, as the `intersection-kernel` spec
requires. crust-core keeps one side table per field:
- `opacity: Vec<f32>`;
- SH coefficients `Vec<[f32; 3]>` striped by `(deg+1)²`;
- the field's world→local rotation for evaluating the SH direction;
- the colour conversion.

At SH degree 3 this is 192 B per particle. The reference capture is about 240 MB of
SH plus 60 MB of kernel data, before the BVH. Coefficients are kept in `f32`
because the schema says to prefer `float`. Storing `half` (−96 B/particle) is a
measured follow-up, not a default.

### 4. Splats are off shadow rays by mask, so no shadow-side machinery is touched

Particle geometry is attached with a mask that excludes the shadow-ray category.
`occluded()` never reports a splat, so NEE and `surface_visibility` cost nothing
more and stay exact. The cutout shadow product is not used for splats at all.

Shadow casting later becomes a mask change plus the existing `∏(1 − α)` product.
This is the place where a k-nearest query would pay off.

### 5. Integration: a splat-field material through the existing cutout and emission paths

Each field is one geometry with one material, whose behaviour at a hit is:
- **opacity at the hit** = `opacity[prim_id] · exp(−½u)`, using the kernel's `u = d²`;
- **`emitted_at`** = the SH radiance toward the ray, converted to the working space;
- **no BSDF**: a met splat is an emitter that absorbs.

`pass_cutouts` then gives stochastic presence, the crossing bound and the re-hit
rule for free. Emission is collected exactly as for an emissive material outside
the light list, with the BSDF-only MIS weight. The lighting pair "a material that
emits only through `emitted_at` must never become a light-list entry" holds by
construction, because the light-list builder must not see splat fields (pinned by
a test).

Two traps to pin with tests:
- **Re-hit at `t*`.** A ray restarted just before `t*` recomputes the same
  particle's `t*` at about the same distance. The re-hit rule must treat a
  particle as one-sided (always "the same side") so the particle is not crossed
  twice. This is the spec scenario "A splat is not met twice".
- **First-hit AOVs.** A met splat's shading normal is `−ω̂` (facing the ray) and
  its depth is `t*`. Its albedo for denoising is what an emissive material outside
  the light list reports today. No new albedo rule is introduced.

*Alternative:* a dedicated splat branch in the path walk. Rejected: it duplicates
the cutout recurrence and its LPE and AOV twins, which is the "pairs that must
change together" failure mode.

### 6. SH evaluation is the reference implementation's, toward the ray

The radiance is `max(0, 0.5 + Σ cₖYₖ(ω̂))`, using the 3DGS `eval_sh` constants and
sign convention (`C0 = 0.28209479…`, `C1 = 0.48860251…`, `C2[5]`, `C3[7]`) and its
coefficient order, which is what OpenUSD's PLY conversion writes.
- `ω̂` is the normalised ray direction (viewer → splat, the same sense as 3DGS's
  `pos − campos`), rotated into the field's local frame.
- 3DGS uses one direction per splat, from the camera centre to the splat centre.
  Crust uses the ray's own direction, which is the only one defined for indirect
  rays. The difference is sub-pixel for camera rays and is listed among the known
  differences.
- A fixture generator (Decision 8) emits expected values from a NumPy port of
  `eval_sh`, the same way `osl_oracle.py` pins MaterialX.

### 7. Radiance colour space: resolved like any USD colour, `srgb_texture` by default

Crust's rule is "a value that names no space is in the working space". Splats get
an explicit exception, of the same kind as `UsdUVTexture`'s `auto` and Ptex's
`g22_rec709`, because their radiance is display-referred: training fits it to
sRGB-encoded photographs.
- The coefficients attribute's resolved `colorSpace` is honoured if authored.
- Otherwise `srgb_texture` is used.
- The transfer curve is applied to the **evaluated** radiance, after the SH sum
  and the clamp, never to the coefficients, because the curve is not linear.
- Compositing then happens in linear light, where a rasteriser composites encoded
  values. This is a known difference at semi-transparent edges.
- The default is recorded in `docs/color_management.md`'s input inventory.

*Alternative:* the plain rule (treat the radiance as linear). Rejected as the
default: on a scene-linear `lin_rec709` render it brightens the midtones of every
capture (0.214 becomes 0.5). An artist who wants it authors
`colorSpace = "lin_rec709"`.

### 8. Fixtures come from the reference toolchain, outside the renderer

`scripts/splat_fixtures.py` (run with `usd-core` ≥ 26.03 in a venv, documented
like `scripts/osl_oracle.py`) writes small `.usda` fixtures into
`crates/crust-core/tests/fixtures/splats/`:
- one particle;
- an anisotropic, rotated particle;
- degrees 0–3 with one coefficient per band;
- `half`-only;
- mis-sized arrays;
- degenerate particles.

It also writes a JSON file of expected `t*`, `d²`, `α` and radiance per probe ray.
Nothing in the renderer depends on Python. The reference capture is a measurement
asset, never committed. It is rendered through a local wrapper `.usda` that adds
the 180° flip and a camera.

### 9. Zero cost when absent, verified in instructions

The particle geometry is a kind of its own (Decision 2). The tracer's existing
`has_pass_throughs()` gate already keeps worlds without cutouts on the fast path,
and a world without splats keeps that gate false. The check:
- the Cornell box at `-s 16` is bit-identical;
- its single-threaded callgrind count stays within 0.1 %, which is the
  "Scenes without particle fields are unchanged" requirement.

### 10. Import: dispatch on type name, read raw attributes

`usd_import/particle_field.rs` (a `pub(super)` sibling with explicit imports) is
dispatched on the prim type name before the mesh/sphere dispatch:
- attributes are evaluated at the import time code (`EvalTimeScope`);
- `float` is preferred and `half` widened to `f32`;
- the schema's length rules and fallbacks are applied;
- degenerate particles are dropped with one WARN per prim, and the per-prim
  summary is logged at DEBUG.

openusd 0.7 predates the 26.03 schema, so no typed API is used, mirroring
`crust:volume:*`. When `openusd-schemas` ships the type, switching to it is
tracked like the other openusd workarounds.

## Risks / Trade-offs

- **Crossings per ray are high on fluffy captures**, so each crossing costs a full
  traversal and renders slow down.
  → Measure first (`--stats` crossings, mean and max) on the reference capture. The
  k-nearest query is the designed follow-up. The 256 crossing bound caps the
  worst case, at the cost of meeting the 257th splat.
- **The look differs from the 3DGS viewer and from production renderers** (no 0.3 px
  dilation, per-ray SH direction, linear-light compositing).
  → Report relmse against a reference render (a production renderer or OpenUSD's
  `hdParticleField`), and list the differences in the user docs.
- **The default colour space may not match production renderers.**
  → The default is one line in the import, and the attribute override exists from
  day one. Confirm against a production-renderer reference before archiving (task 7.3).
- **openusd 0.7 cannot decode `half3[]` / `quath[]` in `.usdc`, or chokes on the
  new prim type.**
  → This is the first task (a probe). If `half` decoding is missing, `half`-only
  prims are skipped with a WARN and recorded as a gap. Float data, which is what
  the tutorial converter writes, is unaffected.
- **f32 precision for very thin splats** (scale ~1e-4 makes `A` ~1e4).
  → The `t*` formula is ratio-based. A fixture with a 1e-4-thin particle pins
  `t*` and `d²` against an f64 reference.
- **Stochastic presence noise on semi-transparent fluff.**
  → The noise is unbiased. Adaptive sampling targets it, and the k-buffer remains
  the variance-reduction follow-up.
- **Memory at 10 M+ particles is about 3 GB.**
  → Reported by `--stats`. `half` SH storage is the measured follow-up.
- **The indirect firefly clamp (10) also clamps splat emission seen by bounces.**
  This is consistent with every other emitter. A/B work uses `--indirect-clamp 0`.

## Migration Plan

Purely additive. No existing scene changes behaviour, and that is pinned bitwise.
No environment switch is added: there is no old behaviour to A/B against. Rollback
is reverting the change.

## Open Questions

- Do production renderers decode splat radiance as sRGB? Decision 7 makes either answer
  a one-line default change, and task 7.3 compares against a reference EXR if one
  can be obtained.
- Is `half` SH storage worth its precision loss on the reference capture (relmse against
  `f32`)? Measure after the change lands.
