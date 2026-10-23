# Proposal

## Why

When the camera's aperture aspect differs from the image's, crust always keeps the
vertical aperture and derives the horizontal field of view from the image aspect —
in effect `adjustApertureWidth`, whatever the stage asks for. `aspectRatioConformPolicy`
(a `UsdRenderSettingsBase` attribute, on `RenderSettings` and `RenderProduct`, not on
`UsdGeomCamera` as issue #339 states) is never read. Two consequences:

- A stage that asks for `adjustApertureHeight` (horizontal fit, the Maya / Nuke
  convention) renders at a different field of view than intended — 0.75× the
  intended size for a 3:2 aperture on a 2:1 image (#339).
- The schema default `expandAperture` is wrong too, whenever the aperture is *wider*
  than the image: a 36×24 aperture on a square image renders cropped to 24×24
  instead of expanded to 36×36. No policy needs to be authored for this one.

## What Changes

- Read `aspectRatioConformPolicy` from the first `RenderProduct`, else the
  `RenderSettings` prim, as `resolution` and `dataWindowNDC` are resolved; unauthored
  is `expandAperture`.
- Conform the render camera's apertures to the image's aspect before the field of
  view is derived, for the five policies (`expandAperture`, `cropAperture`,
  `adjustApertureWidth`, `adjustApertureHeight`, `adjustPixelAspectRatio`), as
  OpenUSD's `CameraUtilConformWindow` does. The same conformed apertures frame the
  render, size adaptive subdivision and on-screen implicit shapes, and are reported
  by `crust ls camera --json`.
- `adjustPixelAspectRatio`, which keeps the aperture and changes the pixel aspect,
  is conformed as `expandAperture` with a `product.unhonoured_attribute` warning:
  crust renders square pixels.
- An authored policy token that is none of the five is refused with a warning and
  read as `expandAperture`.
- A later product that asks for another policy than the first is written with the
  first's framing and warned about, as a differing `dataWindowNDC` is.
- `crust ls camera --json` adds `conformed_aperture_mm` beside the authored
  `aperture_mm`.
- **BREAKING** (framing): a camera whose aperture is wider than the image, with no
  policy authored, now shows its whole authored width (expanded vertically) instead
  of being cropped. Cameras whose aperture is narrower than or equal to the image's
  aspect, and cameras with no authored `verticalAperture`, frame exactly as before.

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `usd-scene-import`: a new requirement for the render camera's framing under the
  aspect-ratio conform policy, and its warnings for an unknown policy, an unhonoured
  `adjustPixelAspectRatio` and a product that differs from the first.
- `cli`: `crust ls camera --json` camera records gain `conformed_aperture_mm`.

## Impact

- `crates/crust-core/src/scene/usd_import/`: `products.rs` (resolve the policy with the
  product base, the mismatch warning), `camera.rs` (`CameraFrame::read` conforms),
  `listing.rs` (the new field), `mod.rs` / `live.rs` (passing the framing to the
  camera readers). `warnings.rs` gains codes for an invalid policy and a policy
  mismatch.
- Dependency: uses `openusd_schemas::render::conform_aperture` and
  `AspectRatioConformPolicy`, already in the pinned `openusd-schemas`; no new crate.
- No render-time cost: the conform runs once per import on four floats.
- MCP live edits: a policy edit on the settings re-frames the camera through the
  existing camera-projection check (in place, or a re-import when the tessellation
  read the projection).
- User docs: `site/content/docs/usd/render-settings.md`, `reference/warnings.md`,
  `architecture/limitations.md` (non-square pixels), `reference/command-line.md` if it
  lists `ls --json` fields. Design record: `openspec/specs/usd-scene-import/design.md`.
- Issue #339: its repro authors the policy on the camera; the fix is tested with the
  policy on the render settings.
