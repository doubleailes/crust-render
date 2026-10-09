## Context

openusd 0.7 resolves no schema fallbacks: `SchemaRegistryBuilder::compiled_in()` registers
nothing ("the registry machinery ships ahead of the OpenUSD schema data"), so an
unauthored or blocked attribute reads back `None`. Every UsdLux read in crust-core is
therefore `attr_f32(..).unwrap_or(<literal>)` or the equivalent, and the literal is the
fallback. The light listing (`crust ls light`, `Scene::list_usd_records`) shares
`light_inputs` with the import, so the two cannot disagree. See proposal.md for the bug
this let through.

### Audit

The reference is OpenUSD `pxr/usd/usdLux/schema.usda` (`release`). The copy vendored in
openusd main, `crates/openusd-schemas/schemas/usdLux/schema.usda`, is byte-identical.
"Per type" means a concrete schema overrides a `LightAPI` fallback
(`apiSchemaOverride = true`). Only `DistantLight`'s `inputs:intensity` does so among
the inputs crust reads.

| input | read in | crust's fallback | schema's fallback | verdict |
|-------|---------|------------------|-------------------|---------|
| `inputs:intensity` | `lights.rs` `light_inputs` | 1, every light | 1 (`LightAPI`); **50000 on `DistantLight`** | **mismatch on `DistantLight`**: fixed |
| `inputs:exposure` | `light_inputs` | 0 | 0 | match |
| `inputs:color` | `light_inputs` | (1, 1, 1) | (1, 1, 1) | match |
| `inputs:normalize` | `light_inputs` | false | false | match |
| `inputs:enableColorTemperature` | `lux_params` | false | false | match |
| `inputs:colorTemperature` | `lux_params` | 6500 | 6500 | match |
| `inputs:diffuse`, `inputs:specular` | `lux_params` (warn when ≠ 1) | 1 (no warning) | 1 | match (an authored ≠ 1 is ignored: a documented gap) |
| `SphereLight` `inputs:radius` | `emit_sphere_light` | 0.5 | 0.5 | match |
| `DiskLight` `inputs:radius` | `emit_disk_light` | 0.5 | 0.5 | match |
| `CylinderLight` `inputs:radius`, `inputs:length` | `emit_cylinder_light` | 0.5, 1 | 0.5, 1 | match |
| `RectLight` `inputs:width`, `inputs:height` | `emit_rect_light` | 1, 1 | 1, 1 | match |
| `RectLight` `inputs:texture:file` | `rect_light_texture` | none (uniform) | none | match |
| `DistantLight` `inputs:angle` | `emit_distant_light` | 0.53 | 0.53 | match |
| `DomeLight` `inputs:texture:file` | `emit_dome_light` | none (uniform) | none | match |
| `DomeLight` `inputs:texture:format` | `emit_dome_light` | unauthored ≡ `latlong` | `automatic` (crust reads it as `latlong`) | match |
| `inputs:shaping:focus` | `lux_shaping` | 0 | 0 | match |
| `inputs:shaping:focusTint` | `lux_shaping` | (0, 0, 0) | (0, 0, 0) | match |
| `inputs:shaping:cone:angle` | `lux_shaping` | 90 with `ShapingAPI` applied, 180 without | 90 (exists only with the API) | match where the attribute exists. The 180° without the API is **deliberate** (already documented) |
| `inputs:shaping:cone:softness` | `lux_shaping` | 0 | 0 | match |
| `inputs:shaping:ies:file` | `lux_shaping` | none | none | match |
| `inputs:shaping:ies:angleScale` | `lux_shaping` | 0 | 0 | match |
| `inputs:shaping:ies:normalize` | `lux_shaping` | false | false | match |
| `collection:lightLink:includeRoot`, `collection:shadowLink:includeRoot` | `light_links.rs` `link_query` | true | true (`LightAPI`'s override of `CollectionAPI`'s false) | match |

These inputs are not read, so they have no fallback in crust to compare:

| input | schema's fallback | what crust renders | verdict |
|-------|-------------------|--------------------|---------|
| `DomeLight_1` `poleAxis` | `"scene"` (the pole on the stage's up axis) | the pole on the light's +Y, whatever the up axis | **deliberate gap**, documented |
| `ShadowAPI` `inputs:shadow:enable` / `:color` / `:distance` / `:falloff` / `:falloffGamma` | true / black / −1 (unbounded) / −1 (none) / 1 | ordinary, unbounded black shadows | unauthored ≡ the schema. Authored values are a documented gap |
| `SphereLight` `treatAsPoint`, `CylinderLight` `treatAsLine` | false | the area light | match (hints the schema lets an area-light renderer ignore) |
| `DomeLight` `guideRadius` | 1e5 | (viewport guide only) | no rendering effect |
| `light:shaderId`, `light:materialSyncMode`, `light:filters`, `PortalLight`, `GeometryLight`, `MeshLightAPI`, `VolumeLightAPI` | | not read | documented gaps |

## Goals / Non-Goals

**Goals:**

- Each input crust reads falls back to the schema's value for the light's own type,
  through one place per light type that the import and the listing share.
- A test pins every fallback, not only the one fixed here.

**Non-Goals:**

- Reading `DomeLight_1`'s `poleAxis` or the stage's `upAxis`. That needs a dome-only
  rotation that does not reach the dome's namespace children. It is a feature, not a
  fallback, and it stays a known gap.
- Registering UsdLux schema data in openusd's `SchemaRegistry` (see D2).
- Validating `inputs:colorTemperature`, which is not checked for being finite today.
  That is unrelated to fallbacks.

## Decisions

### D1. The intensity fallback comes from the light's schema type

`light_inputs` and `lux_params` take `&impl LightSchema`. `LightSchema` is a crust-side
trait over openusd-schemas' `Light`, implemented once per concrete UsdLux type crust
imports. It carries `const INTENSITY: f32`. The default is `LightAPI`'s 1, and
`DistantLight` overrides it to 50000, exactly as the schema does.

*Why a trait rather than a parameter at each call site:* the import and the listing
both call `light_inputs`. With a parameter, the fallback would be a value that two
call sites must keep in step, the kind of pair CLAUDE.md lists as the usual source of
bugs. With the trait, the fallback belongs to the type: a caller cannot pass a
`DistantLight` with the wrong fallback, and a newly imported light type has to
implement the trait before it can reach `light_inputs`.

*Why not match on the prim's type name inside `light_inputs`:* the type is already
known statically at every call site, and a string match would silently fall through
to 1 for a type it forgot.

### D2. Not through openusd's `SchemaRegistry`

openusd 0.7 can resolve fallbacks against a registry built with
`SchemaRegistry::builder()`. That needs the flattened schematics *and* a manifest of
`def` prims carrying `schemaKind` / `bases`, which openusd has not shipped yet. The
same work is in flight upstream (openusd main reads a collection's fallbacks through
its registry; `retire-openusd-workarounds` tracks it). Doing it here would duplicate
upstream's data for one value. Once openusd registers UsdLux, `attr_f32` returns
50000 itself. crust's constant then agrees with it, and retiring it changes no image.

### D3. Blocked and non-finite take the same fallback

A blocked attribute reads `None` in openusd, as an unauthored one does, which is USD's
rule: a block reads back the schema fallback. A non-finite value already falls back
with a `light.non_finite_input` warning. The warning prints the fallback it used, so
for a distant light it now prints 50000.

### D4. The pinning test compares whole lights

For each light type, the test writes a stage with two prims: one that authors nothing
and one that authors every input crust reads at its schema value. Both import, and
the test compares `sample_li` (direction, distance, radiance, pdf) over a grid of
`(u, v)` from a few points, requiring the two to be bitwise equal. A shaped pair
applies `ShapingAPI` on both. This covers every fallback in the audit's first table
that affects the image, through the code a render runs, and it does not depend on how
each fallback is spelled in the importer. Two inputs are exceptions: `ies:angleScale`
and `ies:normalize` act only on a profile that a host loads, and the test runs
without one.

## Risks / Trade-offs

- [A stage relying on the old, dark fallback] → no checked-in sample does. Such a
  stage would not render as USD intends anywhere else, so the old look was a bug. The
  proposal marks the change as BREAKING.
- [A later openusd returning fallbacks itself] → it would return the same values. The
  pinning test keeps passing, and it would catch an openusd fallback that disagreed
  with the schema.
- [The schema changes a fallback] → the audit names the schema file and branch. The
  pinning test writes the schema values out explicitly, so it would need updating
  together with the constant.
