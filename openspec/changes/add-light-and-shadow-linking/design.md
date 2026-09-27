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

- Membership is tested on the geometry's prim path and its ancestors, using
  `UsdCollectionAPI` rules: the nearest path in `includes` / `excludes` decides,
  with `includeRoot` as the fallback. `expandPrims` is the default rule,
  `explicitOnly` matches only the named paths, and `expandPrimsAndProperties`
  behaves like `expandPrims` because crust has no property-level geometry.
  Included collections are resolved recursively, and a cycle is refused with a
  `WARN`.
- A collection with `includeRoot = true` (UsdLux's fallback) and no
  `includes` / `excludes` is *default*: it builds nothing.
- **Trap:** the streaming import does not keep prim paths after emission, so the
  class has to be assigned while each geometry is emitted, which means lights must
  be resolved first. Either resolve light collections in a light-first pass over
  the stage (lights are few), or record `(geom_id, path)` and assign classes after
  traversal. The second option costs memory proportional to the prim count on
  Moana-scale scenes, so prefer the first.
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
ray-mask bits 3–31, which gives up to 29 classes. Each geometry that casts shadows
(`MASK_SHADOW` set) carries its class's bit in place of bits 3–31. Class 0, the
class included in every restricted light's set, keeps the `MASK_SHADOW` bit
itself. A restricted light's shadow ray carries the union of its included classes'
bits, and an unrestricted light's ray carries `MASK_SHADOW | bits 3–31`, which is
exactly what it carries today. `shadow_transmittance` passes the same mask on each
of its steps through media, so an excluded volume does not attenuate either.
`crust-rt` needs no change.

- **Trap:** an authored `crust:rayMask` with bits ≥ 3 set would alias shadow
  classes. Those bits become reserved, and an authored value that uses them is
  masked with a `WARN`.
- **Trap:** nested instance masks compose, so the prototype's geometry must keep
  every class bit and let the instance's own mask decide. Otherwise a class bit
  is lost inside instances.
- **More than 29 classes:** the lights whose sets need more are refused with a
  `WARN`, and their shadows fall back to every occluder. A per-ray occluder filter
  in `crust_rt::Scene::occluded` (a `geom_id` bitset, keeping the kernel free of
  crust types) is the follow-up if a real scene hits this limit.

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
- **The class limit (D3)** of 29 shadow classes is plenty for character and
  set rigs. It is a hard refusal, not a silent approximation, when exceeded.
- **Wasted picks (D2)** grow with the number of unlinked lights. Per-class
  renormalisation is the fix and is scoped as its own task.
- **Throughput.** One class lookup per NEE sample, and per emitter hit, when
  linking exists. Verify with `scripts/bench_ab.sh` on `usdlux.usda` (unlinked:
  expect noise) and the new sample.

## Migration Plan

1. Membership resolution and classes (D1), with unit tests on `includes` /
   `excludes` / nesting / `includeRoot` / `explicitOnly`.
2. Light linking (D2) on all three sites, plus learned-selection training (D4).
3. Shadow linking occlusion (D3), then its bounce-side rule.
4. The sample, spec sync, and the design-record and README gap updates.

Each step is independently shippable, and each ends with
`scripts/check_images.sh check` showing unlinked samples bit-identical.

## Open Questions

- Does `openusd-schemas` 0.7 expose `UsdCollectionAPI` membership publicly? If it
  does, D1 uses it instead of `collections.rs`.
- Should the NEE-only rule (D3) be replaced by the Cycles-style extra ray in the
  same change, or only once the sample's relMSE shows it matters?
