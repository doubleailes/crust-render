## Why

crust reads a `DomeLight`'s lat-long texture rotated 180° about +Y from the
orientation UsdLux specifies. The `DomeLight` schema adopts the OpenEXR lat-long
convention: longitude 0 points along +Z, the image's left column is longitude
+π and its right column −π, and longitude +π/2 points along +X. So the image
centre (u = ½) faces **+Z**, and +X sits at u = ¼. crust puts **−Z** at the
centre and +X at u = ¾ (`environment.rs`, `u = 0.5 + atan2(x, −z) / 2π`). Its
module comment reasons that "−Z, the direction a USD camera looks down" should be
at the centre. That reasoning is not in the spec.

The error shows on any interior lit by an HDRI through a window. On the OpenPBR
Shader Playground, the dome's sun lands behind the room instead of shining
through its window, so the sunlit patch and the window-frame shadows on the floor
are missing. A 180° rotation about the dome's local Y, authored in a scratch
layer, restores them. NVIDIA's Typhoon (hdEmbree on OpenUSD `typhoon/main`, 70c45e8) implements the
spec's mapping exactly: `s = 0.5 − atan2(x, z) / 2π` in
`renderer/lights/domeLight.cpp`, with the inverse
`d = (sin θ sin φ, cos θ, sin θ cos φ)`, `φ = 2π(½ − s)`.

## What Changes

- **The lat-long mapping follows UsdLux.** `EnvironmentMap::direction_to_uv`
  becomes `u = ½ − atan2(x, z) / 2π`, and `uv_to_direction` its inverse,
  `φ = 2π(½ − u)`, `d = (sin θ sin φ, cos θ, sin θ cos φ)`. Latitude is
  unchanged: row 0 is +Y. Radiance lookup, importance sampling and the pdf all
  go through these two functions, so they stay one consistent density.
- **BREAKING for textured domes.** Every textured dome is turned 180° about its
  own +Y axis relative to today. A uniform-colour dome is unchanged. Of the
  checked-in samples, 17 carry a textured dome and their golden images change.
  No other image changes.
- The module documentation, the unit test that pins the old convention, and the
  `lighting` design record state the spec's convention and cite it.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `lighting`: "Infinite lights" states the lat-long orientation and gains a
  scenario that pins it.

## Impact

- `crates/crust-core/src/environment.rs`: `direction_to_uv`, `uv_to_direction`,
  the module header, and `conventions_are_as_documented`.
- `crates/crust-core/tests/environment.rs`:
  `minus_z_is_the_image_centre_and_plus_z_the_seam` is replaced by a test of the
  spec's convention.
- Golden images for the 17 textured-dome samples (`samples/animation.usda`,
  `cornellbox`, `curves`, `displacement`, `domelight`, `hair`, `instancing`,
  `materialx_cutout`, `materialx_subsurface`, `materialx_surfaces`, `motionblur`,
  `nested_instancing`, `openpbr_showcase`, `pxr_displace`, `subdivision`,
  `subdivision_adaptive`, `usdlux`) must be re-recorded. Any sample that authored
  a dome rotation to compensate for the old convention is reviewed.
- Performance: none. The same arithmetic, with one sign and one argument swapped.
