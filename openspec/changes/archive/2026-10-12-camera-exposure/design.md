## Context

See `proposal.md` for why. What shapes the approach:

- **The render camera is geometric only.** `usd_import/camera.rs` reads the lens
  (focal length, apertures, f-stop, focus distance) into `crust_core::Camera`, which
  generates rays and nothing else. No part of the render carries an image-wide scale
  today.
- **Three places produce a pixel's value**, all from the same per-pixel accumulator
  (`PixelState::estimate`):
  - the final resolve of a pass, into the beauty `Buffer` and the AOV film;
  - the snapshots a controlled render publishes as it goes (`crust mcp`'s
    progressive renders);
  - the blend of a guided render's passes, which combines resolved pass buffers.
- **Several hosts consume the engine's result**: `crust render` (EXR products and the
  PNG), the `crust mcp` session (snapshots and final images) and `crust diagnostic`
  (probe renders). Each would have to repeat a scale applied outside the engine.
- **The film's `ChannelKind::Color` is not "radiance".** Albedo and the diffuse
  filter are colour-managed colour channels but reflectances, which an exposure must
  not touch.
- **Typhoon (hdEmbree, OpenUSD `typhoon/main` `70c45e8`) does the same thing.** It takes
  `HdCamera::GetLinearExposureScale()`, multiplies the Color AOV's RGB (not alpha) as
  each sample is written (`renderer/aov/aovOutput.cpp`), and feeds its adaptive
  sampling the unexposed colour (`renderer/renderer.cpp`), so the samples do not
  depend on the exposure. It has no light path expression or variance AOVs. It gates
  the whole thing behind the Hydra render setting `enableExposureCompensation`,
  default on.
- **USD's formula is single precision.** C++ computes
  `(time × iso × powf(2, exposure) × responsivity) / (100 × fStop × fStop)` in
  `float`.

## Goals / Non-Goals

**Goals:**

- One scale, computed once at import, applied in one place the engine owns, that
  every host inherits without a change.
- Scale 1, which is every scene without an authored exposure, costs nothing and
  changes no bit.

**Non-Goals:**

- A `--exposure` flag or a `crust:*` override. The stage authors the exposure; a
  host that wants another one authors it in an override layer, as `crust mcp` edits
  do. A flag can come later without changing this design.
- The photometric camera beyond brightness. `exposure:time` does not become the
  shutter interval, and ISO adds no noise. They enter the scale only, as USD
  specifies.
- `crust ls camera --json` reporting the exposure attributes.
- Display-side exposure in the PNG tone map. The PNG is derived from the already
  exposed beauty, as it is from any beauty.

## Decisions

**D1. Scale the resolved film, not the paths.** The scale multiplies each pixel's
resolved estimate. Alternatives considered:

- *Weight each camera ray's throughput.* The clamp threshold would then be in exposed
  units, so the same scene clamps differently at another exposure. Adaptive
  sampling would be unchanged, since its threshold is relative.
- *Scale in `crust-render`'s writers.* The MCP snapshots, `crust diagnostic` and any
  future host would each need the same code, and the snapshot path lives in the
  engine anyway.

Applying it after the resolve keeps the integrator, the clamp, the stopping rule and
the LPE ↔ beauty bit-identity pair exactly as they are. Whatever an output is divided by the beauty
must move with it: the clamp counter's removed luminance takes the scale, so
`crust diagnostic`'s clamp share does not depend on the exposure. Exposure becomes a pure
post-multiplication.

**D2. Where in the engine.** The scale is applied in two places, the only two from
which an image leaves the engine:

- once to the finished result at the end of `render_impl`: the beauty buffer and
  every film plane. This covers the guided blend, which works on unscaled passes,
  as its weights expect;
- in the snapshot publish closure, as each estimate is written to the display.

**D3. Each source states its exposure power.** `AovSource::exposure_power() -> u8`:

| power | sources |
|---|---|
| 1 | `Color`, `Lpe` (with the raw light sources and light groups, which are LPE vars) |
| 2 | `Variance` (the variance of the luminance mean) |
| 0 | every other source |

An exhaustive `match` with no wildcard, so a new source has to choose. `ChannelKind`
stays a channel-naming concern.

**D4. Scale 1 is skipped, not multiplied.** Multiplying by `1.0` is exact, NaN
included, but skipping it makes the bit-identity guarantee structural and the
default free. The resolve takes `if scale != 1.0`.

**D5. Computed at import, carried on `RenderSettings`.** `camera.rs` computes the
scale with C++'s operations, in `f32` and in C++'s order, from the five attributes at
`eval_time()`. The import sets `RenderSettings::exposure_scale`, so the stamp,
`crust check` and the engine read it where they already read settings. The
procedural fallback scene keeps the default of 1. Each camera the traversal builds carries its
scale unchecked. Once the render camera is resolved, and only when its exposure
applies (D6), a non-finite or non-positive scale raises `camera.invalid_exposure`
(`Refused`) naming that camera and stores 1. A fallback candidate that is not rendered
through, or an exposure turned off, warns about nothing.

**D6. `enableExposureCompensation`, as Hydra reads it.** The import reads the render
setting off the `RenderSettings` prim. `crust:enableExposureCompensation` wins over
the bare Hydra name, exactly as `domeLightCameraVisibility` is read. Default `true`;
`false` stores 1. Typhoon multiplies per sample before accumulating and crust after
resolving. Accumulation is linear, so the two give the same image, and crust's
resolve-time placement also covers the LPE and variance planes Typhoon does not have.

**D7. Recording and comparing.**

- `SamplingStamp` gains `exposure_scale`, written as `crust:exposureScale` (float)
  after `crust:indirectClamp`.
- `crust check` lists `exposure_scale` with flag and attribute `null`. It comes from
  the camera's exposure attributes, and no `crust:*` attribute or flag sets it.
- `compare::comparability` adds a `warn` note when both stamps carry the key and the
  values differ. A missing key reads 1, so files written before this change compare
  as before.

## Risks / Trade-offs

- **Scenes re-lit by hand to compensate double up.** A user who raised their lights'
  `inputs:exposure` to cover for the ignored camera exposure renders brighter now. →
  The stamp records the scale, `crust check` shows it, and the user docs say so where
  they describe the camera.
- **Large exposures move a lot of brightness.** Sponza's 4.5 stops is ×22.6. → This
  is the authored intent. Nothing in the engine's sampling depends on it (D1), so
  noise and clamping are unchanged.
- **Goldens.** No sample authors an exposure, so `check_images.sh` must report every
  sample unchanged. That is the gate for D4.
