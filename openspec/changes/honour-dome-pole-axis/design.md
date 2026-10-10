## Context

A dome reaches the renderer through `emit_dome_light` (`scene/usd_import/lights.rs`):
it reads the shared light interface (`lux_params` over `LightAPI`), the texture through
`DomeLight`'s `texture_file_attr` / `texture_format_attr`, and keeps only the rotation of
the prim's world transform as a `Mat3A`, which `CoreDomeLight::new` stores. Everything
downstream (radiance lookup, importance sampling, the pdf, `Light::escaped`) works in the
dome's local frame through that one matrix and `EnvironmentMap::direction_to_uv` /
`uv_to_direction`, so the three cannot disagree about orientation.

The import dispatches lights by typed-schema `get`, which asks the stage's registry
`is_a(type)`. `DomeLight_1` derives from `NonboundableLightBase`, not `DomeLight`, so it
matches no arm: in `mod.rs`'s traversal, nor in `listing.rs`'s `ls light` record or
kind test. The import never reads the stage's `upAxis`: cameras and geometry stay in
stage coordinates, and the camera alone decides what is up in the image.

`openusd-schemas` already generates the `DomeLight_1` view and its `pole_axis()`
accessor (decoding to a `PoleAxis` enum, fallback `scene`), and `openusd` exposes
`Stage::up_axis()` (the root layer's metadata, `None` when unauthored).

## Goals / Non-Goals

**Goals:**

- One import path for both dome schemas, so a `DomeLight_1` cannot drift from a
  `DomeLight` on texture, colour, visibility, linking or LPE tags.
- The pole decided once, at import, with no per-ray cost.
- Bit-identical output for every stage without a `DomeLight_1`.

**Non-Goals:**

- `poleAxis` on a `DomeLight` (its schema has none). No tolerant mode, no switch.
- Reading `upAxis` for anything else (cameras, geometry, distant lights). A `DistantLight`
  has no pole: its direction is its prim's −Z, and the up axis does not enter.
- `texture:format` values other than `latlong` / `automatic`, which keep warning as
  today, now for both schemas.

## Decisions

### The pole is a matrix folded into the dome's rotation

The dome's stored rotation becomes `R_world · R_pole`, with `R_pole` the identity or a
+90° rotation about X (column-vector convention: local +Y → +Z, local +Z → −Y, +X
fixed). That is OpenUSD's `_GetDomeOffset` (`GfRotation(GfVec3d(1, 0, 0), 90.0)`),
applied the way Hydra's `domeOffset` is meant to be applied: before the prim's transform,
in the dome's frame only. It touches only the dome's own frame, so the "not inherited by
namespace children" clause holds by construction: children are traversed with their own
world transforms, which never include `R_pole`.

*Alternatives considered.* Remapping the texture at load (rotating the lat-long image)
would mean resampling, losing the exact texel ↔ direction correspondence the importance
sampler's pdf depends on, and a host round-trip per dome. Baking `R_pole` into the prim's
world transform would leak into children if any read it, which the schema forbids.
Applying it in `EnvironmentMap` would make the map know about USD.

### `DomeLight_1` shares `emit_dome_light`

The traversal gains a `DomeLight_1` arm that calls the same function. `emit_dome_light`
stops taking a `&DomeLight` and takes what it actually needs: the prim, its world
transform, the two texture attributes, and the pole rotation. Both schema views produce
those, from attributes with the same names, types and fallbacks (`inputs:texture:file`,
`inputs:texture:format = "automatic"`). The `DomeLight` arm passes the identity.

*Alternative considered.* Viewing a `DomeLight_1` prim through
`DomeLight::from_prim_unchecked` would compile with no signature change, because the
attribute names coincide. It is rejected because it hides the schema mismatch: the day
the two schemas diverge on a texture attribute, nothing would fail. Passing the
attributes from the right view keeps each schema answering for its own declarations.

### The up axis is read only for a `DomeLight_1`

`poleAxis = scene` needs the stage's `upAxis`. It is read through `Stage::up_axis()` for
each `DomeLight_1`, from the stage being imported. It is one metadata lookup, and a stage
has a handful of domes, so it is not cached. Under masked or streamed
import that is the composed stage, whose root layer is the user's. That matches
OpenUSD's adapter, which reads `prim.GetStage()->GetMetadata(upAxis)`. Unauthored means
Y (UsdGeom's fallback), so no rotation. A value other than `Y` / `Z` cannot be authored
validly. If one is, it is treated as unauthored.

### Strict on the original schema

A `DomeLight` keeps `R_pole = identity` even when it authors `poleAxis`. That attribute
is undeclared on `DomeLight`, and OpenUSD's adapter reads it only through
`UsdLuxDomeLight_1`. Being strict keeps every existing stage bit-identical and matches
what the spec and OpenUSD's own imaging do. JungleRuins (`def DomeLight` with
`poleAxis = "scene"` on a Z-up stage) is the known asset this leaves sideways. That is
the asset's error, and the record says so.

### Diverging from Typhoon, and saying so

On a `DomeLight_1` whose pole resolves to +Z, crust follows the schema and Typhoon does
not: hdEmbree reads only `SampleTransform` and never asks for `domeOffset`. Everywhere
else (both schemas imported, the lat-long convention, a +Y pole) they agree. The
`lighting` design record states the divergence with the evidence (`delegate/light.cpp`
reads the transform alone; `domeLight_1Adapter.cpp` is identical to Pixar's), the same
way it already cites Typhoon for the lat-long mapping.

### Testing in numbers, with a generated map

The scenarios are checked like the existing four-band orientation test: a stage written
inline, a test `AssetLoader` whose `load_environment` returns a generated 4 × 2 (or
wider) lat-long map with red, green, blue and white quarter-width bands, and direct calls
on the resulting light's escaping radiance along the scenario's directions. A brighter
row on top distinguishes the top row from the bands. No image file is checked in and
nothing is rendered. A render of a Z-up `DomeLight_1` stage is a manual sanity check
only.

## Risks / Trade-offs

- [A Z-up asset authored as `DomeLight_1` but tuned by eye in Typhoon or another renderer
  that ignores the pole] → its sky turns 90° in crust. That is the schema's answer, and
  the user documentation says how to keep the old look: author `poleAxis = "Y"`.
- [`Stage::up_axis()` returns the root layer's value, and a wrapper layer that sublayers
  a Z-up asset without repeating `upAxis` reads as Y-up] → the same as OpenUSD (a stage
  takes its metadata from its root layer), and the same trap the ALab wrapper already
  documents for `metersPerUnit`. The user documentation states it.
- [The registry stops answering `is_a("DomeLight_1")` for a stage opened without the
  schema family] → every stage is opened through `usd_import::stage_builder()`, which
  registers the families. The listing and traversal tests cover it.

## Migration Plan

No migration. No checked-in sample authors `DomeLight_1`, so `check_images.sh` must
report no change. The release note says that `DomeLight_1` is now imported (it was
silently dropped) and how a Z-up `DomeLight_1` is oriented.
