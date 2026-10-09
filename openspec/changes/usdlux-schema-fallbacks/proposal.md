## Why

crust reads every unauthored UsdLux input through a fallback of its own, written next to
the read, because openusd 0.7 reports no schema fallbacks (its compiled-in
`SchemaRegistry` registers nothing). Those hand-written fallbacks are the schema's in
every case but one. `inputs:intensity` falls back to `LightAPI`'s 1 on every light, but
`DistantLight` overrides it to **50000** ("so that we can supply a high default intensity
to approximate the Sun", OpenUSD `pxr/usd/usdLux/schema.usda`, identical to the copy
vendored in openusd's `crates/openusd-schemas/schemas/usdLux/schema.usda`). As a
result, a `DistantLight` that authors no intensity renders **50000 times darker** than USD
specifies and than Hydra draws it. That is effectively black: an un-normalised 0.53° sun
at 1 nit puts 6.7·10⁻⁵ lux on a facing surface.

## What Changes

- **An unauthored `DistantLight` `inputs:intensity` is 50000.** It is the same for a
  blocked value and for a non-finite authored one, which already falls back to the
  schema value with a `light.non_finite_input` warning. That warning now names 50000
  for a distant light. Every other light keeps 1.
- **BREAKING for stages that author a `DistantLight` without an intensity.** Their
  sun becomes 50000 times brighter, as USD specifies. None of the checked-in samples
  is such a stage: all four distant lights author `inputs:intensity`, so no golden
  image changes.
- `crust ls light --json` (and `Scene::list_usd_records`) report the same fallback,
  since they read the inputs through the same function.
- The other fallbacks are audited and pinned. Every other input crust reads already
  falls back to its schema value (design.md, "Audit"). A test now imports each light
  type twice, once with nothing authored and once with every schema fallback written
  out, and requires the two to be the same light. A future drift in any fallback
  then fails a test, not a render.
- Two divergences are deliberate, and the lighting design record and the user
  documentation say so. The first is `inputs:shaping:cone:angle` without
  `ShapingAPI` applied, which is 180° and not 90°; this is already documented, and
  the user page gains it. The second is `DomeLight_1`'s `poleAxis`, which is not
  read: its fallback `"scene"` puts the pole on the stage's up axis, and crust always
  keeps it on the light's +Y. Inputs that are not read at all (`ShadowAPI`, light
  filters, `PortalLight`, mesh and volume lights) have no fallback to compare. The
  `ShadowAPI` fallbacks describe ordinary shadows, which is what crust renders.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `lighting`: a new requirement, "Unauthored UsdLux inputs take the schema's fallback",
  with a scenario that pins the distant light's 50000. "Known gaps" adds
  `DomeLight_1`'s `poleAxis`.

## Impact

- `crates/crust-core/src/scene/usd_import/lights.rs`: `light_inputs` / `lux_params` take
  the intensity fallback from the light's schema type rather than a literal 1.
- `crates/crust-core/src/scene/usd_import/listing.rs`: `read` follows the new bound.
- Tests: `crates/crust-core/tests/usd_inline.rs` (the 50000 fallback, blocked and
  non-finite included, and unauthored ≡ authored-at-fallback for every light type),
  `crates/crust-core/tests/usd_listing.rs` (the listed fallback).
- Documentation: `openspec/specs/lighting/design.md` (UsdLux import, Known gaps),
  `site/content/docs/usd/lights.md` (fallbacks, `poleAxis`, the cone without
  `ShapingAPI`).
- Golden images: none change, because no sample leaves a distant light's intensity
  unauthored. `scripts/check_images.sh` confirms this.
- Performance: none. One constant per light type is read once at import.
- When openusd ships schema data (tracked by `retire-openusd-workarounds`),
  `attr_f32` will return 50000 itself. crust's constant then agrees with it and
  becomes redundant, but it does not change behaviour.
