## Why

crust does not import `UsdLuxDomeLight_1` at all. `DomeLight::get` asks `is_a("DomeLight")`,
and `DomeLight_1` derives from `NonboundableLightBase`, not from `DomeLight`, so a stage
whose sky is a `DomeLight_1` renders black with no warning: `crust check` reports
`0 light(s)` and `crust ls light` says the stage has no light. Importing it is only half
the job. `DomeLight_1` adds `poleAxis`, and on a Z-up stage its fallback (`scene`) puts the
dome's top pole on +Z. A `DomeLight_1` read as a `DomeLight` would show its sky on its
side, with the HDRI's sun near the horizon or below it.

The schema is unambiguous (`usdLux/schema.usda`, `DomeLight_1`): with `poleAxis = "Z"`,
or `"scene"` on a stage whose `upAxis` is `Z`, latitude ±π/2 is ±Z and latitude 0,
longitude 0 points along −Y. OpenUSD's own imaging adapter
(`usdImaging/domeLight_1Adapter.cpp`, `_GetDomeOffset`) implements it as a +90° rotation
about X, applied to the dome itself and not inherited by its namespace children.
NVIDIA's Typhoon (hdEmbree, OpenUSD `typhoon/main` 70c45e8) imports `DomeLight_1` but
never reads that offset: none of hdEmbree's 222 source files mention `domeOffset`,
`poleAxis` or `upAxis`, and its light reads only `SampleTransform`. On exactly the
Z-pole case crust follows the schema, not Typhoon.

## What Changes

- **`DomeLight_1` is imported** as a dome light with everything `DomeLight` reads
  (`inputs:texture:file` / `format`, intensity, exposure, colour, colour temperature,
  camera visibility, light and shadow linking, LPE tag), and it is listed by
  `crust ls light` and counted by `crust check` exactly as a `DomeLight` is.
- **`poleAxis` orients a `DomeLight_1`.** When `poleAxis` is `Z`, or `scene` (the
  fallback) on a stage whose root layer authors `upAxis = "Z"`, the dome's own frame is
  turned +90° about X before its prim transform, so its top pole is +Z and longitude 0
  points along −Y. `Y`, or `scene` on a Y-up stage or one with no `upAxis`, leaves the
  frame as it is. Radiance, importance sampling and the pdf all see the one rotated
  frame.
- **`DomeLight` (the original schema) is unchanged and strict.** Its pole is +Y whatever
  the stage's up axis, even when the prim authors a `poleAxis` attribute, which that
  schema does not declare. OpenUSD's adapter reads `poleAxis` only through
  `UsdLuxDomeLight_1`, and crust does the same. JungleRuins (`def DomeLight` with
  `poleAxis = "scene"` on a Z-up stage) and the three Z-up MaterialX samples render
  bit-identically.
- **A deliberate difference from Typhoon** on `DomeLight_1` with a +Z pole, recorded in
  the `lighting` design record beside the lat-long convention that does agree with it.

No image of a checked-in sample changes: none authors `DomeLight_1`.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `lighting`: the "Infinite lights" requirement covers `UsdLuxDomeLight_1` as well as
  `UsdLuxDomeLight`, and states how `poleAxis` and the stage's `upAxis` orient a
  `DomeLight_1` while a `DomeLight` keeps its +Y pole.

## Impact

- `crates/crust-core/src/scene/usd_import/mod.rs`: the light dispatch also matches
  `DomeLight_1`.
- `crates/crust-core/src/scene/usd_import/lights.rs`: `emit_dome_light` takes the dome
  through the light interface both schemas share, plus the pole offset, which is the
  identity for a `DomeLight`.
- `crates/crust-core/src/scene/usd_import/listing.rs`: `ls light` and the listing's
  kind test recognise `DomeLight_1`.
- The stage's `upAxis` is read once per import (`Stage::up_axis`). Nothing else in the
  import reads it yet.
- A new test fixture: a minimal Z-up stage with a `DomeLight_1` and a small generated
  lat-long map with one bright patch at a known latitude and longitude, checked in
  numbers, plus the same map under a `DomeLight` to pin the strict +Y case.
- Documentation: `site/content/docs/usd/lights.md` (the light table and the orientation
  paragraph), the `lighting` design record.
- No render-time cost: the rotation is folded into the dome's existing `Mat3A` at
  import.
