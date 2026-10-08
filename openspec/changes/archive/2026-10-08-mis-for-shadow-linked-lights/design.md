## Context

The motivation and the ALab numbers are in proposal.md § Why.

### Today's encoding

The importer encodes a restricted `collection:shadowLink` as a shadow-ray mask
(`LightLinks::shadow_masks`: `MASK_SHADOW` plus the light's class bits) and flags the
light `nee_only`. Every strategy-aware site reads that flag:

| site | today, restricted light at a continuous vertex |
|---|---|
| surface NEE (`path.rs`, the `light_weight` call) | weight 1 |
| phase NEE (volume vertex) | weight 1 |
| `bounce_emission_weight` (bounce ray hits the light's geometry) | 0 if the strategy samples lights, else unopposed |
| `escaped_emission` / `escaped_split` (bounce ray leaves the scene) | skipped if the strategy samples lights |

Under `--strategy bsdf` the bounce therefore collects a restricted light through the
*physical* occluders. That is why BSDF-only and light-only disagree on ALab: 0.030 against
0.208 mean luminance.

### What the bounce ray has to cross

The bounce ray already crosses hidden emitters (`pass_cutouts` / `pass_walls`, recorded
in `ThinWalls::crossed`), cutouts and thin walls stochastically. The shadow side crosses
them by transmittance (`shadow_transmittance`). That is a matched pair for *unlinked*
lights only.

### How the motivating numbers were measured

Two renders that differ only in seed: a wrapper layer sublayers the scene with
`(offset = 1)` and renders `-f 1005`, so the scene is evaluated at 1004 while the
sampler seed is 1005. The camera debug line is identical in both. Per pixel, the noise
variance of the two-seed mean is `(a − b)² / 4`.

Light groups come from an overlay that sets `crust:light:lpeTag` on every light, plus
`C<RG><L.'group'>` / `C<RD><L.'group'>` vars. Fireflies are the beauty's top 0.1% of
pixels by that variance. The scratch tool and overlays are not committed. Task 1
records the method and the numbers in `docs/alab_profile.md`.

## Goals / Non-Goals

**Goals:**

- Make NEE and BSDF sampling estimate the *same* integrand for a restricted light
  (emission × the light's own shadow visibility × BSDF), so MIS can combine them.
- Cost nothing in unlinked scenes, bit for bit, and little in linked ones: +5% on ALab
  at equal spp.
- Keep every pair `CLAUDE.md` names intact:
  - NEE weight ↔ bounce-side twin;
  - `escaped_emission` ↔ `escaped_split`;
  - `C.*[LO]` ≡ beauty.

**Non-Goals:**

- Delta vertices. A mirror keeps showing the physical shadow (the documented gap).
- A bounce-side twin for restricted domes (D5).
- The ALab rect sun's residual variance (`lgt_sun_area_01`). With the blockers
  deactivated, so that MIS is whole, it falls only 9% (2 983 → 2 711). It is not a
  firefly problem:
  - **Where it is:** 89% of it sits in about 20 pixels at the right frame edge
    (x 638–639, y 116–126).
  - **What it is:** a real highlight, at luminance 30–180 in both seeds against
    ~0.05 around it, with about 20% relative noise.
  - **Why it doesn't move:** the share is the same under every selection, light-sample
    count and strategy, so it is sub-pixel coverage noise of the glint, not
    light-sampling noise.
  - **Why it doesn't matter:** a display-referred image clips it to white.
- Light linking (`lightLink`). It filters receivers and needs no visibility twin.

## Decisions

### D1. The twin is a fresh shadow ray from the vertex, not a continuation of the bounce ray

At a continuous vertex with bounce direction ω (already sampled, its pdf already
known), each restricted light L whose emission ω can reach is resolved with
`shadow_transmittance` on a ray from the vertex along ω, carrying L's shadow mask, up to
L's point (D2). The contribution is the bounce throughput × L's emission there ×
transmittance × the bounce-side MIS weight.

By construction, that is the same visibility function NEE evaluates, crossings and
volumes included.

**Alternative rejected: continue the bounce ray past the occluders L ignores.** That
is cheaper, since it needs a ray only when the bounce first stops on an excluded
surface. But the bounce ray crosses cutouts and thin walls stochastically and carries
`P/q`, while NEE uses transmittance. A cutout L ignores, crossed before the first stop,
would weigh `(1 − α)` on one side and 1 on the other: a biased MIS combination. Getting
it right needs a second crossing walk per light with L's mask, which is a shadow ray
anyway.

**Alternative rejected: keep NEE-only and raise `--light-samples`.** Measured: direct
glossy goes 5 340 → 4 688 from 4 to 16 samples, while render time rises 17%.

### D2. Which lights ω can reach: an analytic support test, not a BVH query

- **Area lights:** a ray–shape intersection on `AreaShape`, giving the first point
  along ω and its distance.
  - The shapes: sphere, rect, and the affine unit disk and cylinder.
  - Its domain is exactly the shape the light samples (`sample_point` /
    `solid_angle_sampler`): one-sided where the light is one-sided, and the cylinder's
    wall only.
- **Distant light:** `Light::escaped(from, ω)` already answers whether ω is inside the
  cone, and with what pdf. The distance is infinite.

Each restricted non-dome light gets one test per vertex. A shadow ray is cast only on a
hit: about 3% of directions for ALab's 20° sun, and rarer for a 10 cm card.

**Alternative rejected: a closest-hit query on the light's geometry.** Hidden lights
carry no shadow-mask bit and solid ones do, so finding just L through the kernel would
need a per-light mask bit. Those bits are already spent on shadow classes.

**The pair:** the analytic hit must agree with the kernel's hit on the light's own
geometry. A unit test fires rays at each shape both ways.

### D3. One owner of the bounce-side estimate

For a restricted non-dome light at a continuous vertex, the ordinary bounce collects
nothing, **under every strategy**. That covers the geometry hit
(`bounce_emission_weight` → 0), a hidden-emitter crossing, and an escape
(`escaped_emission` / `escaped_split` skip it). The twin is the only bounce-side
estimate.

The twin's weight follows the strategy:

| strategy | twin weight |
|---|---|
| `power`, `balance` | `bounce_weight(bounce_pdf, light_pdf)` |
| `bsdf` | `unopposed_weight()` |
| `light` | not run |

`light_pdf` is `lights.density(light.pdf_at_point(from, point), pmf, nee_count)`:
exactly what NEE divides by. `bounce_pdf` is the vertex's own sampling pdf, the
guide/BSDF mixture when guiding is on, as `PrevVertex` records it.

The twin must use the same emission answer as everything else:
`Emissive::radiance_toward` for area lights and `escaped` for the distant light.

**Alternative rejected: let `bsdf` keep the physical occluders.** Then `--strategy
bsdf` would remain a different image from `light` on linked scenes, and the
strategies stop being visualisations of one integrand. That is the current
`light_linking` test's premise, and it only holds for a link that changes nothing.

### D4. NEE gets its ordinary MIS weight

The surface and phase NEE replace `if lights.nee_only(index) { 1.0 }` with the ordinary
`strategy.light_weight(light_pdf, bounce_pdf)` for restricted non-dome lights. This is
the other half of D3. The two halves change in one commit, or emission is double-counted
or lost.

### D5. A restricted dome stays NEE-only

A dome's support is every direction, so a twin would cost a shadow ray on every
continuous bounce. On ALab that is about +84% shadow rays: 556 M bounces against
664 M shadow rays at 1024 spp. The restricted dome there contributes 0.0001 luminance
and no measurable noise (`dd_env_dome` / `gd_env_dome` both ≈ 0).

The `LightLinks` flag splits in two:
- `restricted`: the shadow mask is set;
- `nee_only`: now true only for a restricted dome.

**Revisit when** a scene's restricted dome shows up in a noise attribution. The cheap
version then would be to cast the twin only when the bounce ray's first stop is geometry
the dome ignores, or when it escapes. For an infinite light, "reached" means "escaped",
so the crossing mismatch of D1 applies only between the vertex and that first stop.

### D6. Where the twin runs

The twin runs at the vertex, right after the bounce direction and its pdf are drawn,
before the bounce ray is traced:
- at a surface vertex, beside the per-lobe `scatter_resolved` / `scatter_split`;
- at a phase vertex, beside the phase sample.

The vertex has the lobe there, for routing (D7), and the throughput the bounce will
carry. The contribution is that throughput × emission × transmittance × weight, added
to the path's radiance exactly as NEE's is.

### D7. Light path expressions

The twin's contribution is routed as the vertex's sampled lobe event followed by `L`,
with the light's tag. That is the event sequence a bounce hit on the light would have
produced (`C<RG>L`, `C<RD>L`, …). It goes through the same lobe-split routing the
bounce-hit emission uses. The value routed is the value added, computed once, so
`C.*[LO]` stays bit-identical to the beauty (pinned by the existing LPE tests).

### D8. Sampler domains

The twin's shadow ray draws (volume transmittance, cutouts) from a new keyed
sub-domain, `K_LINK_TWIN`, beside `K_NEE_SHADOW` in `tracer/path.rs`, one per light
index. A scene without restricted lights never draws it, so the existing streams, and
images, are untouched.

## Risks / Trade-offs

- **[Risk] The twin's visibility differs from NEE's, silently biasing MIS.**
  → D1 calls the same `shadow_transmittance` with the same mask and the same
  `[t_min, distance − ε]` convention as NEE. The "three strategies agree" scenario
  (spec) is the test: light vs bsdf vs power on a link that matters, glossy floor,
  1024 spp, 4 seeds, the means agreeing within the 1/√N noise.
- **[Risk] The analytic support test disagrees with the kernel's geometry** (a rect's
  facing, a cylinder's caps), so the twin finds a light where the bounce would not, or
  misses one. → The D2 pair test, per shape.
- **[Trade-off] Bounce-side noise moves into the twin.** Where BSDF sampling was the
  better strategy, the twin's contribution now carries most of the weight. That is
  intended, and it is the 97% drop. Diffuse receivers barely change: NEE already
  dominates them.
- **[Risk] Cost scales with the number of restricted lights.** One support test each per
  continuous vertex. ALab has 4, 3 of them non-dome. → If a scene with dozens appears, a
  small BVH over restricted area lights. Not needed now.
- **[Risk] Learned selection or guiding densities differ between the two sides.** → Both
  sides read the same `lights.density(…, pmf, nee_count)` and the vertex's recorded
  `pdf`, the existing invariant `bounce_emission_weight` already relies on.
- **[Trade-off] `power_mis_matches_nee_only_for_a_shadow_linked_light` stops holding.**
  It pins today's estimator bitwise. It is replaced by the agreement-in-expectation test
  and the variance test from the spec.

## Migration Plan

There is no data or API migration. The change is a behaviour change in linked scenes
only: same mean, lower variance. Unlinked goldens (`scripts/check_images.sh`) stay
bit-identical.

Rollback is reverting the commit. The `restricted` / `nee_only` split makes a
"twin off" switch trivial if an A/B is wanted. `CLAUDE.md` asks that such a switch's off
side be the old behaviour. One switch is added for the measurement only:
`CRUST_LINK_TWIN=0`, documented as for any `CRUST_*`.

## Open Questions

- **Should `CRUST_LINK_TWIN` outlive the measurement?** The decision can wait for the
  ALab numbers. Either way it is one `Config` field and its documentation rows.
