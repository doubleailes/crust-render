## Context

See `proposal.md` for the measurement. The requirements are in
`specs/lighting/spec.md` ("Hidden light sources do not occlude", "Rays cross
hidden light sources", "Visible and masked light sources stay solid").

The code today:

- **Masks.** `light_ray_mask` (`usd_import/attrs.rs`) returns
  `MASK_SHADOW | MASK_INDIRECT`, plus `MASK_CAMERA` when the light opts in. An
  authored `crust:rayMask` replaces it. `World::attach_masked` stores it on the
  geometry. Shadow rays (`surface_visibility`) carry `MASK_SHADOW`, or a light's
  class bits under shadow linking (`LightList` shadow masks, `light_links.rs`).
- **Bounce side.** A bounce ray takes the closest hit with `MASK_INDIRECT`. When
  the hit emits (`ShadingPoint::emitted`), the previous vertex's `VertexRec`
  gets `next_emit = atten · emitted` and `next_emit_weight =
  bounce_emission_weight(...)`, one pair per record (`tracer/path.rs`, around
  line 1400). The emitter's `Emissive` material does not scatter, so the path
  ends there.
- **Cutouts already pass through.** `pass_cutouts` restarts a segment past
  surfaces it does not meet, keeping `t` measured from the segment's origin, so
  the carried medium, volume regions and ray cone are unaffected. It spends no
  depth and leaves the previous vertex's MIS record to whatever the segment
  reaches. Its shadow twin is `cutout_through`. Both are bounded by
  `MAX_CUTOUT_CROSSINGS`.

## Goals / Non-Goals

**Goals**

- Hidden lights neither shadow nor block one another, on both MIS strategies,
  so NEE and bounce estimate the same integral.
- Bit-identical images, at identical cost, for scenes with no camera-invisible
  area light (`World::has_transparent_emitters()` false).

**Non-Goals**

- Camera-visible light sources keep occluding. That is what a visible lamp bulb
  should do, and what Typhoon does for visible light geometry.
- Emissive *materials* (`Material::emitted_at` with no light-list entry) are
  surfaces, not lights. They keep occluding and scattering as authored.
- Shadow linking semantics do not change. A light source simply stops being a
  caster.

## Decisions

### D1. Both sides, or neither

Dropping `MASK_SHADOW` alone would make NEE treat lights as transparent while
bounce rays still stop at the first light they meet. For directions where light
B hides light A, NEE would count A, MIS-weighted, but the bounce side would
return B alone and never A, so the MIS combination would lose A's bounce share.
That is a bias, and exactly the NEE ↔ bounce mismatch the invariants in
`CLAUDE.md` exist to prevent. The change therefore makes the bounce side pass
through hidden sources too. Typhoon only half does this: its bounce side picks
the nearest finite light analytically, so a nearer light still hides a farther
one. crust does it on both sides.

### D2. A segment can collect several emitters

A bounce segment that crosses k hidden sources collects k emissions, each with
its own `bounce_emission_weight`, which depends on that light's pmf and pdf. The
current `VertexRec` holds one `(next_emit, next_emit_weight)` pair. Options:

1. Accumulate pre-weighted emission:
   `next_emit_weighted += atten · w_i · e_i`.
2. Keep a small inline list of `(emit, weight, emitter)` per record.

The LPE AOVs replay the beauty's recurrence and `C.*[LO]` is pinned bitwise to
the beauty, so the routing needs each crossed emitter's own `L` event — but the
beauty itself does not. **As built, the two are split:**

- The beauty takes **option 1**. `VertexRec` gains `crossed`, the crossings'
  weighted emission summed in crossing order, and `crossed_raw`, the same sum
  unweighted for guiding training. The gather adds `crossed` beside
  `next_emit · next_emit_weight`; with no crossing it adds zero, so a segment
  that crosses no hidden source is bit-identical.
- The per-crossing list of option 2 lives in the LPE route only:
  `Route::cross` records each crossing's `L` symbol, weighted share and the
  pass-through events before it. Where every crossing's `L` is accepted, the
  gather uses the record's own `crossed` sum, so `C.*[LO]` stays the beauty bit
  for bit.

The inline overflow first planned for `VertexRec` was not needed. Task 3.1
measured the cost on the zero-AOV render: +0.12% instructions on cornellbox.

### D3. Telling a transparent emitter at a hit

A hit has to be classified cheaply. The plan was to read the answer off the
geometry's mask (a light source without `MASK_SHADOW` and without `MASK_CAMERA`).
**As built, the mask cannot answer it:** an authored `crust:rayMask = 4` also
lacks both bits, yet an authored mask must keep the source solid. So the
importer marks a transparent emitter explicitly. `light_ray_mask` returns the
mask together with that answer: true only with no authored mask and no camera
visibility. `WorldBuilder::set_transparent_emitter` stores it as a sparse
per-`geom_id` table, and `World::is_transparent_emitter(geom_id)` reads it at a
hit. `World::has_transparent_emitters()` and `has_bounce_pass_throughs()` keep a
world with no hidden light out of the new branch. Demoting a light that
illuminates nothing clears its mark.

### D4. Order with cutouts

A segment's walk is: closest hit, then `pass_cutouts`, then the transparent
emitter check, which repeats the walk from just past the emitter. Both walks
share one crossing budget. Past the budget, the next hit counts as present, as
cutouts do today.

### D5. Shadow linking

`light_links.rs` assigns caster classes by clearing `MASK_SHADOW` on casters and
setting class bits. A hidden light source must stay out of every class.
Otherwise a restricted light's shadow rays, which carry class bits, would see
it again. **As built, no new skip was needed:** `encode_shadows` already gives
no class bit to a geometry without `MASK_SHADOW`, and a hidden source has none.
A comment there records why. A camera-visible or authored-mask source keeps its
shadow bit and is classed like any occluder, as before.

## Risks / Trade-offs

- **The AOV/LPE pair is the delicate part** (D2). Mitigation: the one-slot path
  keeps today's bits, and new tests pin `C.*[LO]` equal to the beauty on a scene
  where a bounce crosses two hidden lights.
- **Images change** wherever hidden lights see one another. That is the intent,
  and veach_mis is the reference case, but users who relied on the old solid
  behaviour need `crust:rayMask` (documented).
- **Light cache (`learned` selection) training** uses `surface_visibility`, so
  it sees the new masks automatically and stays consistent.
