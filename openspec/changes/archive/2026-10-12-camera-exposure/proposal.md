## Why

The import ignores `UsdGeomCamera`'s exposure, so a scene whose camera authors one
renders at the wrong brightness unless its lights are re-tuned by hand (#266). USD
defines the camera's exposure as a scale on the image, and DCC exports author it:
Intel's New Sponza sets 2 to 4.5 stops on every one of its six cameras.

## What Changes

- The import reads the render camera's exposure attributes (`exposure`,
  `exposure:time`, `exposure:iso`, `exposure:fStop`, `exposure:responsivity`) at
  its evaluation time and computes USD's linear exposure scale (C++
  `UsdGeomCamera::ComputeLinearExposureScale`). The unauthored fallbacks give
  exactly 1.
- The render multiplies its outputs by that scale once the film is resolved:
  radiance outputs (the beauty and the light path expression AOVs) by the scale,
  the variance AOV by its square, the rest not at all. The integrator, the
  firefly clamp and adaptive sampling still work in scene radiance, so an
  exposure changes brightness and nothing else.
- A scale that is not finite or not positive is refused with a coded warning and
  reads 1.
- A stage can turn the exposure off with the Hydra render setting
  `enableExposureCompensation = false` on its `RenderSettings` prim (or
  `crust:enableExposureCompensation`), as Typhoon (hdEmbree) and other Hydra
  renderers honour it.
- EXRs record the scale (`crust:exposureScale`), `crust check` reports it among
  the effective settings, and `crust diff` warns when two files differ in it.
- A camera with no exposure authored renders bit-identical to today.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `usd-scene-import`: the render camera's exposure is imported as a linear
  scale on the image, with the refused cases warned.
- `aovs`: which outputs the camera's exposure scales, and by what power.
- `image-output`: EXRs record the exposure scale the render was written with.
- `image-comparison`: `crust diff` warns when two files were written at different
  exposure scales.

## Impact

- `crust-core`: `usd_import/camera.rs` (reading the scale), `RenderSettings`
  (carrying it), `tracer` (applying it to the resolved buffer, the film and
  progressive snapshots), `aov.rs` (each source's exposure power), `stamp.rs`,
  `warnings.rs` (a new `camera.invalid_exposure` code), `compare.rs` (the
  comparability note).
- `crust-render`: `crust check`'s effective settings.
- Docs: the cameras section of the user docs, the warnings reference, and the
  `usd-scene-import` and `aovs` design records.
- No new dependency. Existing renders without an authored exposure are
  unchanged, bit for bit.
