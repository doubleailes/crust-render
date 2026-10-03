## Why

`UsdLuxLightAPI` gives every light two collections: `collection:lightLink` (which
geometry the light illuminates) and `collection:shadowLink` (which geometry casts
shadows from it). Both are everyday lighting tools in production scenes, used for
example to put a rim light on a character without spilling onto the set, or to stop
a hair groom from shadowing a key light. Crust reads neither today. The `lighting` and
`usd-scene-import` specs list "light and shadow linking" as a known gap, so a scene
that relies on them renders every light onto every surface, shadowed by every
occluder. The result looks plausible but is wrong.

This change puts both on the roadmap as one proposal because they share the same
machinery: resolving `UsdCollectionAPI` membership at import, and a per-geometry
class that the integrator can test cheaply. It lays out an order in which the work
can be implemented: light linking first, then shadow linking.

## What Changes

- **Collection membership at import.** Resolve each light's `collection:lightLink`
  and `collection:shadowLink` using `UsdCollectionAPI` semantics: `includeRoot`
  (UsdLux's fallback is `true`, meaning every prim), `includes` / `excludes`
  relationships where the nearest ancestor path decides, `expansionRule`, and
  collections that include other collections. A pattern-based
  `membershipExpression` is refused with a `WARN` and does not silently mean "all".
- **Light linking.** A light illuminates only the receivers in its `lightLink`.
  This covers the light's NEE contribution at surface and volume vertices, and its
  emission collected by a BSDF, phase or escaped ray. The receiver is the prim at
  the vertex that samples the light (for a bounce, the previous vertex). NEE and
  the bounce side stay one MIS pair: both apply the same filter, so an unlinked
  light contributes zero on both sides and a linked light keeps its current weights.
- **Shadow linking.** A shadow ray towards a light is blocked only by occluders in
  that light's `shadowLink`, including volume transmittance along the ray. The
  bounce-side twin is handled as described in `design.md`: a light with a
  restricted shadow set is NEE-only at non-delta vertices, so both MIS sides agree
  on the light's visibility.
- **Learned light selection** (`--light-selection learned`) trains through the same
  two filters, so its per-cell tables do not favour lights a receiver cannot use.
- **Scenes without links are unchanged.** When no light authors a non-default
  collection, the import builds no classes and every mask stays as it is. Output is
  bit-identical, which `scripts/check_images.sh` can verify.
- A sample, `samples/light_linking.usda`, and the matching design-record updates:
  the `lighting` and `usd-scene-import` "Known gaps" entries are retired.

## Capabilities

### New Capabilities

None. Both features extend existing capabilities.

### Modified Capabilities

- `lighting`: adds the "Light linking" and "Shadow linking" requirements, plus the
  requirement that unlinked scenes are bit-identical. Removes light and shadow
  linking from the "Known gaps" requirement.
- `usd-scene-import`: adds a "Light collection membership" requirement for how
  `UsdCollectionAPI` is resolved. "Light schema mapping" stops listing light linking
  as unread.

## Impact

- `crates/crust-core/src/scene/usd_import/`: a new `collections.rs` for membership
  resolution (unless `openusd-schemas` 0.7 already exposes a `UsdCollectionAPI`
  query; `materials.rs:87` suggests it resolves collection bindings internally).
  `lights.rs` and `mod.rs` assign classes to geometry and light entries.
- `crates/crust-core/src/rt_world.rs`: a per-`geom_id` link-class table next to
  `materials` / `faces`.
- `crates/crust-core/src/light/list.rs`: a per-light "illuminates class" test, and
  the `LightList::density` / `pmf` contract if selection is renormalised per class
  (a later optimisation; see `design.md`).
- `crates/crust-core/src/tracer/path.rs`: surface NEE, `volume_nee`,
  `bounce_emission_weight`, `escaped_emission` and the shadow-ray mask.
  `crates/crust-core/src/tracer/light_cache.rs` needs the same filters.
- `crates/crust-rt`: no API change for the first version of shadow linking (it
  reuses the geometry ray mask's free bits 3–31). A per-ray occluder filter is a
  possible follow-up.
- Performance: import-time work only, plus one table lookup and a bit test per NEE
  sample. There is no cost when nothing is linked.
