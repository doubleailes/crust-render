## Context

See `proposal.md` (Why). The requirements are in `specs/rendering/spec.md`.

The code today:

- **Shadow side.** `shadow_transmittance` asks `surface_visibility` (scalar):
  1 when the any-hit query finds nothing, and otherwise, only when the world has
  cutouts, `cutout_through`, the product of `1 − opacity` over cutouts, or 0 at
  the first non-cutout hit. `surface_visibility` is also what the learned light
  cache trains on.
- **Bounce side.** `pass_cutouts` meets a cutout with probability `opacity`,
  drawn from the `K_CUTOUT` domain, and otherwise restarts the segment past it.
  `t` stays measured from the segment origin, no depth is spent, no vertex is
  recorded, and the previous vertex's MIS record carries over. Passes are
  counted as `Ts` events for the LPE routing (`arrival_ts`).
- **Thin-walled transmission is a delta lobe** in both shading paths. Native
  OpenPBR (`openpbr/transmission.rs`, `sample_transmission_thin`) uses the
  Adobe window model: transmission `(1 − R)/(1 + R)` with the window
  reflectance `2R/(1 + R)` (`lobes.rs`). The MaterialX closure evaluator has a
  thin-walled dielectric T leaf that transmits one interface's `(1 − F)`
  (`openspec/specs/materials/design.md`, "crust:openpbr and MaterialX
  open_pbr_surface disagree"). A sampled delta transmission keeps the
  direction, and the next emission it reaches gets the unopposed weight.

## Goals / Non-Goals

**Goals**

- NEE and MIS work through thin walls, on both shading paths, without
  changing any expectation.
- Keep the pass-through pair (bounce `pass_cutouts` ↔ shadow `cutout_through`)
  one rule, applied to cutouts and thin walls together.
- Worlds without thin-walled transmission are bit-identical, at the same
  instruction count.

**Non-Goals**

- Thick dielectrics (refracting interfaces with an interior). A straight shadow
  through them is biased. It could be an opt-in later (Typhoon's
  `ty:enableCaustics = false` behaviour), behind an explicit setting, never by
  default.
- Rough thin-walled transmission as a lobe. crust has none today: thin-walled
  transmission is delta in both paths, so nothing is approximated.

## Decisions

### D1. Thin walls are presence plus a coloured pass

At a hit, a material splits as `f = f_rest + T(ω) · δ(straight)`, where
`T(ω) ∈ [0,1]³` is the weight its BSDF gives the straight transmission for this
crossing direction. Combined with opacity `α`, the fraction of the ray that
continues unscattered along the same line is

```text
P(ω) = (1 − α) + α · T(ω)        (RGB)
```

This is Typhoon's `_CombinePresenceAndTransmissionVisibility`. The shadow side
multiplies `P` over every crossing. The bounce side passes with probability
`q = clamp(max_c P_c, 0, 1)`. That is the minimum-variance single-sample choice
that never needs `q > 1`. On a pass, throughput becomes `throughput · P / q`.
On a meet (probability `1 − q`), the path scatters through `α · f_rest`, with
throughput divided by `1 − q`. For a grey `P` this reduces exactly to today's
cutout rule. Since `T` is evaluated for the same direction on both sides (the
shadow ray and the bounce continuation are the same line), the two strategies
see the same visibility, and MIS weights stay those of the previous vertex.

### D2. The material reports `T`; the integrator never inspects lobes

`Material::straight_transmittance(ray, rec) -> Vec3A` returns `α`-free `T(ω)`.
`Material::has_straight_transmission()` is the cheap per-material flag the
world's flag is built from. Scattering on a meet must exclude the straight lobe.
Native OpenPBR drops the thin-walled delta transmission from its lobe set. The
closure evaluator drops the thin-walled T leaves and keeps every other leaf's
weight, so layering above a sheet (coat, specular) still attenuates the other
lobes as before. `Material::resolve` returns the same `T` and the same reduced
lobe set as per-query shading, and `tests/resolve.rs` pins it for every material,
as `CLAUDE.md` requires.

### D3. Events and AOVs

A pass through a thin wall is a specular transmission, so the LPE routing
records a `TS` event for it, exactly the event the delta-transmission vertex
produced before, so `C<TS>…L` expressions keep their meaning. A cutout pass
keeps its existing classification. Since the beauty's recurrence and the LPE
split share `pass_cutouts`, `C.*[LO]` stays pinned to the beauty.

### D4. The learned light cache trains on luminance

The cache's training shadow rays need a scalar visibility. They use the
luminance of `P` under the working space's weights, as every other
luminance-weighted heuristic does (`docs/color_management.md`). This only steers
selection, never the estimate, so a luminance scalar is unbiased.

### D5. No environment switch

This is not an optimization A/B: the old behaviour blocked light. The
expectation-equality claim is verified directly (task 3.2), which is the
evidence the switch would otherwise provide.

## Risks / Trade-offs

- **Shadow rays get longer** in scenes full of sheets (foliage cards that are
  thin-walled transmissive, curtains). The walk is bounded by
  `MAX_CUTOUT_CROSSINGS`, shared with cutouts.
- **`surface_visibility` becomes RGB**, which touches every NEE call site. The
  `has_straight_transmission` gate keeps the scalar fast path, and task 3.1 pins
  the instruction count.
- **Native OpenPBR versus MaterialX `T` differ** (window model versus one
  interface), as their thin walls already differ. Each is self-consistent with
  its own BSDF, which is what the MIS pair needs.
