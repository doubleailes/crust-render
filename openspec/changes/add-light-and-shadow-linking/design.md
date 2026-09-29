## Context

The relevant current code (from investigation; not restated in full):

- **Lights.** `usd_import/mod.rs:308-327` dispatches each UsdLux type to an
  `emit_*_light` function in `usd_import/lights.rs`. Area lights attach their
  emitter with `world.attach_masked(geometry, material, light_ray_mask(prim))` and
  are added with `lights.add(AreaLight::new(shape, material, geom_id))`. Distant
  and dome lights have no geometry. `LightList` (`light/list.rs`) keeps
  `by_geom: geom_id → index`. Nothing maps a light entry, or a `geom_id`, back to
  its prim path.
- **Collections.** Nothing in crust reads `collection:*`. `usd_import/materials.rs:87`
  says `openusd-schemas`' `compute_bound_material` resolves collection bindings,
  so the crate has membership logic internally. Whether that logic is public was
  not checked, because the crate sources were not on the machine.
- **Hit identity.** `WorldHit { rec, mat, geom_id, prim_id }`. Inside an instance,
  `geom_id` is the top-level instance. Per-geometry tables are indexed by `geom_id`
  (`rt_world.rs:389-396`).
- **Ray masks.** The kernel stores one `u32` per geometry, copied into each
  primitive and tested as `ray.mask & geom.mask != 0`. Only bits 0–2 are used
  (`MASK_CAMERA`, `MASK_SHADOW`, `MASK_INDIRECT`; `crust-rt/src/ray.rs:8-11`).
  `crust:rayMask` sets them from USD, the default is `MASK_ALL`, and nested
  instance masks compose.
- **NEE and its twins** (`tracer/path.rs`): surface NEE (`pick_at` → `sample_li`
  → shadow ray with `MASK_SHADOW` → `shadow_transmittance`), `volume_nee` (same
  shape), `bounce_emission_weight` (through `find_by_geom_at`), `escaped_emission`
  (loops over `iter_at`). `light_cache::train` fires its own `MASK_SHADOW`
  occlusion rays.

## Goals / Non-Goals

**Goals**

- UsdLux-correct membership for `collection:lightLink` and `collection:shadowLink`
  on every light crust reads.
- An unbiased estimator in which the NEE and bounce sides of each MIS pair agree
  on both filters.
- Zero cost and bit-identical output when no link is authored.
- Order the work so light linking can ship on its own.

**Non-Goals**

- Linking for emitters that are not light-list entries: emissive materials,
  and the not-yet-read mesh lights. They have no `LightAPI` and so no collections.
- `membershipExpression` (pattern-based collections). It is refused with a
  `WARN`, the same way `latlong`-only dome formats are.
- `ShadowAPI` (`inputs:shadow:enable`, shadow colour, falloff). That is a separate
  gap, although `shadow:enable = 0` could later reuse the same
  "NEE ignores every occluder" path this change builds.
- Filter lights and light filters.

## Decisions

### D1. Membership resolves to *link classes*, computed once at import

For every geometry the importer computes, for each light that authors a
non-default collection, whether the geometry is a member. Geometries whose answers
agree for every light share a **class**. The class id is a small integer stored in
a per-`geom_id` table in the world, next to `materials`. Each light stores a
bitset over classes: `illuminates(class)` for light linking and
`shadowed_by(class)` for shadow linking. Classes are deduplicated, so a scene
with ten linked lights usually has a handful of classes, not one per prim.

- Membership is openusd's own: `usd::Collection::compute_membership_query`
  gives a `MembershipQuery` whose `is_path_included` applies the
  `UsdCollectionAPI` rules (nearest opinion in `includes` / `excludes` decides,
  `includeRoot` as the fallback, `expandPrims` by default, `explicitOnly`
  matching only the named paths, included collections merged with cycles
  broken). crust adds no `collections.rs`. An expression-mode collection
  (`membershipExpression`) is refused with a `WARN` and read as the default.
- A collection with `includeRoot = true` (UsdLux's fallback) and no
  `includes` / `excludes` is *default*: it builds nothing.
- **Classes are assigned after traversal.** A light can be traversed after the
  geometry it links (and, streaming, in another chunk whose stage is gone), so
  nothing is decided at emission. Instead, as the traversal dispatches each prim
  that emits geometry, it records one entry: the first `geom_id` that prim
  assigned, and its stage path interned. `geom_id`s are handed out in traversal
  order, so one entry covers a mesh, a native instance, a whole PointInstancer
  and an area light's own emitter. Each linked light's `MembershipQuery` is
  computed when the light is met, while its chunk's stage is live. After the
  last chunk, each distinct path is evaluated against every linked light, the
  answer vectors are deduplicated into classes, and the per-`geom_id` class
  table is filled from the runs. This extends the `light_links` module of
  `light-camera-visibility-and-link-exclusion`: that change's "covers no
  receiver" test becomes "the light's illuminated-class set is empty".
  *Rejected:* a light-first pass over the index stage. Both production scenes
  would allow it (ALab's and Moana's rigs come in through sublayers and
  references, not payloads), but it composes most of the index stage a second
  time (composition is ALab's largest traversal cost) and cannot see a linked
  light behind a payload.
- **Instances.** The receiver is the top-level instance's `geom_id`, so
  membership is that of the instance prim. A collection target inside a native
  instance's prototype, or one PointInstancer instance, cannot be told apart
  from its siblings. This is a documented gap, warned once per collection that
  names such a path.

### D2. Light linking: the same filter on both MIS sides, no renormalisation (first version)

A light that does not illuminate the receiver's class contributes **zero**:

- at surface NEE and `volume_nee`, after `pick_at` and before `sample_li`;
- at a bounce-arrival hit on an area light's geometry, where the receiver is the
  *previous* vertex (its record owns the emission, per the CLAUDE.md pair rule);
- in `escaped_emission`, for each infinite light in `iter_at`.

Pick probabilities are left unchanged, so `LightList::density` needs no change and
the MIS pair stays consistent. Both sides use the same pmf, and a linked light's
weights are exactly today's. The cost is wasted NEE picks on unlinked lights.
**Renormalising the pick per class** (a CDF per class, a later task) removes that
waste, but it changes `density` / `pmf`, which is a pair change: it must land on
both sides together.

A vertex in a medium that belongs to no prim counts as a member of every
collection.

### D3. Shadow linking: reuse the kernel mask's free bits, and make restricted lights NEE-only

**Occlusion.** Shadow classes (D1, computed over `shadowLink` only) are encoded in
the kernel ray mask's free bits. Today no ray ever carries bits 3–31: camera,
indirect and shadow rays carry only `MASK_CAMERA`, `MASK_INDIRECT` and
`MASK_SHADOW`. So a geometry's bits 3–31 have no observable effect, and the
encoding can take them over without changing what an authored mask means.

- **The encoding is active only when some light authors a restricted
  `shadowLink`.** Otherwise every geometry mask, including an authored
  `crust:rayMask`, is passed through unchanged, and every shadow ray carries
  `MASK_SHADOW` as it does today. That is what makes the "unlinked scenes are
  unchanged" requirement hold bit for bit.
- **When it is active,** each geometry that casts shadows (`MASK_SHADOW` set)
  keeps `MASK_SHADOW` only if it is in class 0, the class every restricted light
  includes. Otherwise it clears `MASK_SHADOW` and carries exactly one *shadow
  bit*. Bits 3–30 are allocated to the 28 most-populated remaining classes, and
  bit 31 is the shared **overflow bit** for every class left over. The encoding
  rewrites bits 3–31 of an authored `crust:rayMask`. Because no ray ever
  carried those bits, this changes no visibility; it logs one `WARN` per scene
  that authored any of them.
- **Ray masks.**
  - An unrestricted light's shadow ray carries `MASK_SHADOW | bits 3–31`, which
    matches every shadow-casting geometry: each one carries `MASK_SHADOW` or
    exactly one bit in 3–31. The result is equivalent to today's `MASK_SHADOW`
    ray.
  - A restricted light's ray carries `MASK_SHADOW` (class 0), the bits of the
    allocated classes it includes, and the overflow bit only if it includes
    **every** overflow class.
  - A restricted light that includes some overflow classes but not others cannot
    be encoded. It is refused with a `WARN` and falls back to the unrestricted
    mask, which, by the invariant above, is blocked by every occluder, overflow
    geometry included.
- `shadow_transmittance` passes the same mask on each of its steps through
  media, so an excluded volume does not attenuate either. `crust-rt` needs no
  change.
- **Trap:** nested instance masks compose, so the prototype's geometry must keep
  every class bit and let the instance's own mask decide. Otherwise a class bit
  is lost inside instances.
- **Trap:** the invariant "every shadow-casting geometry carries `MASK_SHADOW` or
  exactly one bit in 3–31" is what makes both the unrestricted and the fallback
  mask correct. Pin it with a test that includes a geometry in an overflow class.
- **Beyond the encoding:** a per-ray occluder filter in `crust_rt::Scene::occluded`
  (a `geom_id` bitset, keeping the kernel free of crust types) is the follow-up if
  a real scene refuses lights for want of bits.

**The bounce-side twin.** A BSDF ray that reaches a light through an occluder
outside the light's shadow set is stopped by that occluder, while NEE sees the light.
If the two sides disagreed on visibility, MIS would weight the light as if the bounce
could have found it, which is biased. For a light with a restricted shadow set, the
first version therefore makes **NEE the only strategy at non-delta vertices**: the NEE
weight is 1 and bounce-collected emission from that light is 0. At delta
(specular) vertices, where NEE cannot contribute, the bounce keeps full weight and
the light is seen through the real occluders. A mirror therefore still shows
shadows the light has no link for. That is documented as a gap. Cycles' approach,
a dedicated extra ray that finds shadow-linked emitters behind excluded blockers
and MISes them, is the follow-up that restores MIS for these lights.

This is bias-free, but noisier on glossy receivers of restricted lights, and it
affects only lights that author a restricted `shadowLink`.

### D4. Learned selection and guiding

`light_cache::train` must apply D2's filter to its candidate lights and D3's
mask to its occlusion rays. Otherwise the trained per-cell pmfs are fitted to the
wrong radiance. Guiding does not read `LightList`. It learns from radiance that
is already filtered, so it needs no change.

### D5. No new switch

Linking is authored scene data, not an optimisation, so it gets no `CRUST_*`
switch. The "off" state is the unlinked scene itself. A `DEBUG` line per linked
light gives its member counts. An `INFO` line would violate the bounded-INFO rule.

## Risks / Trade-offs

- **NEE-only restricted lights (D3)** give up MIS on those lights. That is visible
  as extra noise on glossy materials. It is accepted for the first version and
  measured on the sample with `exr_diff`'s `relmse:` against a 1024 spp reference.
- **The class limit (D3):** 28 individually encoded shadow classes plus one
  shared overflow bit is plenty for character and set rigs. A light that cannot
  be encoded is refused with a warning and falls back to full shadowing; it is
  never silently approximated.
- **Wasted picks (D2)** grow with the number of unlinked lights. Per-class
  renormalisation is the fix and is scoped as its own task.
- **Throughput.** One class lookup per NEE sample, and per emitter hit, when
  linking exists. Verify with `scripts/bench_ab.sh` on `usdlux.usda` (unlinked:
  expect noise) and the new sample.

## Follow-ups (scoped separately)

- **Restore MIS for shadow-linked lights** with a Cycles-style extra ray that finds
  the light behind excluded blockers and MISes it. Motivated by the measurement in
  `openspec/specs/lighting/design.md`: NEE-only costs nothing on the diffuse
  sample, but relMSE rises 2.1× (median) to 3.1× (mean) on the same scene in rough
  metal. It matters for ALab, whose key light (`lgt_sun_area_*`) is shadow-linked
  past the louvered windows.
- **Per-class renormalisation of the light pick.** Removes the NEE picks wasted on
  lights a receiver is not linked to. A `density` / `pmf` pair change: it must
  land on the NEE and bounce sides together, with `light_cache` in step.
- **A per-ray occluder filter in `crust-rt`** (a `geom_id` bitset on the occlusion
  query), if a real scene refuses shadow-linked lights for want of mask bits. ALab
  needs 3 occluder classes of the 28 available.

## Migration Plan

1. Membership resolution and classes (D1), with tests on `includes` /
   `excludes` / nesting / `includeRoot` / `explicitOnly` through openusd's query.
2. Light linking (D2) on all three sites, plus learned-selection training (D4).
3. Shadow linking occlusion (D3), then its bounce-side rule.
4. The sample, spec sync, and the design-record and README gap updates.

Each step is independently shippable, and each ends with
`scripts/check_images.sh check` showing unlinked samples bit-identical.

## Open Questions

- Should the NEE-only rule (D3) be replaced by the Cycles-style extra ray in the
  same change, or only once the sample's relMSE shows it matters?
