# Design

## Context

The import builds the render camera in one place.
`usd_import/camera.rs::lens_camera` reads `CameraLens` (`focalLength`, `fStop`,
`focusDistance`) through `lens()` and passes `aperture = focal / fStop` to
`Camera::new`, which halves it into `lens_radius`. `Camera::get_ray` samples a
disk only when `lens_radius > 0`, so a pinhole costs nothing. Two other readers
share `lens()`: `crust ls camera --json` (`listing.rs`) and adaptive subdivision
(`screen_projection`, which reads the frame but not the lens). The import never
reads `metersPerUnit`.

`disableMotionBlur` gives the pattern to copy. `products.rs::Base` resolves it per
product (product over settings). `RenderProducts::motion_blur` carries it, and
`mod.rs` puts it on `RenderSettings` (`with_motion_blur`) before the traversal
builds the camera. A later product that differs gets
`product.motion_blur_mismatch`. The MCP live path (`usd_import/live.rs`)
re-reads the settings and products from the edited layer, then calls the same
`build_camera(&stage, &prim, &read.settings)`.

## Goals / Non-Goals

**Goals:**

- One formula for the lens diameter, in one function, used by the render.
- `disableDepthOfField` follows the same path as `disableMotionBlur`, so an MCP
  edit and a full re-import agree without new code in `live.rs`.

**Non-Goals:**

- A CLI flag or an environment switch (the proposal explains why).
- Other lens models: bokeh shape, aperture blades, anamorphic squeeze, tilt-shift,
  or `crust:` lens attributes.
- Honouring `metersPerUnit` anywhere else (light units, displacement bounds,
  edge lengths). This change reads it for the lens only.
- Any change to `crust ls camera --json`. It keeps reporting the authored
  `f_stop` and `focus_distance` and gets no diameter field, so the `cli` spec does
  not change.

## Decisions

**Lens units are millimetres, converted with `metersPerUnit`.** The diameter is
`focal / fStop × 0.001 / metersPerUnit`. The alternative was OpenUSD's own
convention: tenths of a scene unit, `focal × 0.1 / fStop`, which is what Hydra's
`GfCamera` does. It matches usdview on every stage, but on a metre stage it
gives a 50 mm lens a 1.79 m opening. The user asked for a physical lens. The two
agree at `metersPerUnit = 0.01`, which is also USD's fallback, so stages that
author nothing keep matching Hydra.

**The diameter is computed in `lens_camera`, not stored in `CameraLens`.**
`CameraLens` stays "the lens as authored", which is what `ls camera --json`
reports. `lens_camera` already turns it into a `Camera`, and it is the only
consumer of the diameter.

**`metersPerUnit` is read through the stage API, the way `upAxis` is.**
`lights.rs` already calls `stage.up_axis()`. The first task confirms that openusd
exposes `meters_per_unit()` beside it. If it doesn't, the value is read from the
root layer's metadata, which is where USD stores it (`UsdGeomGetStageMetersPerUnit`
reads only the root layer). The masked stages `live.rs` opens keep their root
layer's metadata, so both paths read the same value.

**The refusal check runs only when there is a lens.** An invalid
`metersPerUnit` affects nothing in a pinhole render, and warning about it there
would be noise. Most stages author `fStop = 0` or nothing. It reads as 0.01,
USD's fallback, so the refused stage renders as a stage without the attribute.

**`disableDepthOfField` is applied at import, as a pinhole camera.** It is a
`depth_of_field: bool` on `RenderSettings` beside `motion_blur`, set from
`RenderProducts`, and `lens_camera` reads it to pass aperture 0 to `Camera::new`.
The alternative was a check in the tracer before calling `get_ray`, but that
leaves a lens on `Scene::camera` that the render does not use, and puts a branch
on the camera-ray path. Applied at import, the rays a pinhole gives are the
same rays `fStop = 0` gives today. The focus distance only scales the viewport,
and that scale cancels in the ray direction.

**The mismatch warning copies motion blur's.** A product cannot change the
camera, so products that disagree are written with the first product's
setting, under a new `product.depth_of_field_mismatch` warning (Approximated,
Each). Refusing the product would be stricter than motion blur, for the same
kind of disagreement.

## Risks / Trade-offs

- [Stages tuned for the old blur render much sharper] → It is the intended fix,
  and the proposal marks it BREAKING. The one sample that was tuned
  (`cornellbox_guided.usda`) drops its workaround. The release notes and the
  render-settings page state the factor: 10x on centimetre stages, 1000x on
  metre stages.
- [A metre stage exported in the USD convention (focal length in tenths of a
  metre, e.g. `focalLength = 0.5` for 50 mm) gets a 100x smaller lens] → The
  camera documentation states that crust reads lens values in millimetres. The
  field of view is unaffected, so such a stage looks wrong only in its blur.
- [No longer bit-identical to Hydra on non-centimetre stages] → The camera page
  documents it. Centimetre stages and stages that don't author `metersPerUnit`
  still match.
- [Goldens recorded from a stage with a lens change] → The only sample with a
  non-zero `fStop` is `cornellbox_guided.usda` (`fStop = 1000`, almost a pinhole
  already), and removing it changes that image by noise only. `check_images.sh`
  runs before and after to confirm that no other image moves.

## Migration Plan

There is nothing to deploy. The change lands as one commit with its docs. To
keep an old look, scale `fStop` down by the factor above.
