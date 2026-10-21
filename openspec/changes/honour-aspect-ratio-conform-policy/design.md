# Design

## Context

See proposal.md (Why). The current framing lives in one place:
`CameraFrame::read` (`usd_import/camera.rs`) reads `focalLength` and both apertures.
It defaults an unauthored `verticalAperture` to `horizontal · H / W`. Three readers
use it:

```
                 CameraFrame::read(cam, stage, prim, &RenderSettings)
                 |            |                       |
         lens_camera   screen_projection         camera_lens
         (render rays) (adaptive subdiv,         (`crust ls camera --json`)
                        implicit shapes)
```

`lens_camera` derives `vfov` from the vertical aperture alone and passes the image's
`W / H` as `Camera::new`'s aspect. So the horizontal aperture is never read for
framing, which is why every policy collapses to `adjustApertureWidth`.

The settings side is `products.rs`. `Base` holds the attributes a product inherits
from the settings and may override (camera, resolution, data window, blur and
depth-of-field flags). `resolve_products` compares each later product's `Base`
against the first's and warns per attribute (`product.region_mismatch` and the
others). `warn_unhonoured` warns for `pixelAspectRatio != 1`.

The pinned `openusd-schemas` already provides what we need:
`render::conform_aperture(policy, resolution, pixel_aspect, aperture)`, a port of C++
`_ApplyAspectRatioPolicy` that guards degenerate inputs, plus
`AspectRatioConformPolicy` with `TryFrom<sdf::Value>` and the accessor
`SettingsBaseSchema::aspect_ratio_conform_policy_attr()`.

## Goals / Non-Goals

**Goals:**
- The conformed aperture is computed once per import and is the only framing every
  reader sees: render rays, the adaptive and implicit-shape projection, `ls`, and MCP
  live edits.
- The change is bit-identical for every stage whose conform is a no-op. That covers
  every repo sample, since none authors a `verticalAperture` with another aspect
  than its resolution. Goldens stay unchanged.

**Non-Goals:**
- Non-square pixels: honouring `pixelAspectRatio` in rays and in the EXR header.
  This remains a known gap (see Decision 4).
- `horizontalApertureOffset` / `verticalApertureOffset` (lens shift). These are
  still unread. They need an off-centre window in `Camera` and get their own change.
- Overscan through a conformed window larger than the frame. `dataWindowNDC` keeps
  clipping as today.

## Decisions

### 1. Conform inside `CameraFrame::read`, not after it

`CameraFrame` gains the conformed `[h, v]` apertures. Its `vert_aperture` and
`tan_half_vfov` read the conformed vertical, and a new `tan_half_hfov` reads the
conformed horizontal. The authored pair is kept as well, for `ls`. The default for an
unauthored vertical is applied first, then the conform.

Doing it here means `lens_camera`, `screen_projection` and `camera_lens` cannot
disagree, which is the property `CameraFrame` exists for. MCP live edits come for
free: `refresh_settings` re-reads through `read_index`, and the projection key
(`ScreenProjection::key`: `f_px` and the frustum normals) changes whenever the
conformed vertical or horizontal changes. So the existing "adaptive subdivision would
read another camera projection" check sends a reframing edit to re-import, and
anything else applies in place.

*Alternative:* conform in `lens_camera` only. That was rejected because adaptive
subdivision would tessellate for one frame while the render shows another, the class
of half-pair bug CLAUDE.md lists.

### 2. Carry the framing beside `RenderSettings`, not inside it

`CameraFrame::read` takes `&RenderSettings` today, but `RenderSettings` is the
tracer's struct and holds no USD vocabulary. A small `Framing { policy:
AspectRatioConformPolicy }` resolved by `products.rs` (with the camera and the
resolution, on `Base`) travels with the import's read (`read_index` → the import
context → the three callers, plus `live.rs`). The resolution itself stays where it
is, on `RenderSettings`.

*Alternative:* add a field to `RenderSettings`. That's less plumbing, but it puts an
import-only enum into the tracer and into every `RenderSettings::default()` caller.

### 3. Rays use the conformed apertures, with the image aspect as a check

With square pixels and a policy other than `adjustPixelAspectRatio`, the conformed
aperture's aspect equals `W / H`. `lens_camera` keeps calling `Camera::new(vfov,
aspect = W / H)` with `vfov` from the conformed vertical, so `Camera` is unchanged.
A `debug_assert` (relative 1e-5) pins conformed `h / v == W / H` and catches a
wrong plumbing order. When the conform is a no-op, the conformed vertical is the
authored or defaulted one bit for bit, because `conform_aperture` only rescales the
adjusted axis. That is why the golden images stay unchanged.

### 4. `adjustPixelAspectRatio` conforms as `expandAperture`, with a warning

This was the user's decision: conform and warn. The policy keeps the aperture and
would set `pixel_aspect = aperture_aspect / (W / H)`. Crust renders square pixels,
so keeping the aperture would break Decision 3's invariant. When the computed pixel
aspect differs from 1 (relative 1e-6), the import raises
`product.unhonoured_attribute`: "aspectRatioConformPolicy = adjustPixelAspectRatio
needs pixelAspectRatio = 0.75; rendering square pixels, conformed as expandAperture".
It then conforms as `expandAperture`, which keeps the whole authored aperture in
frame, the closest square-pixel image.

The conform is always run with pixel aspect 1. An authored `pixelAspectRatio != 1`
keeps its existing `warn_unhonoured` warning and is not fed to the conform:
conforming to a non-square image aspect and then rendering square would distort.

Note for the follow-up: honouring non-square pixels turns out to be small. The rays
would take independent horizontal and vertical tangents (aspect = conformed `h / v`
instead of `W / H`), and the `exr` crate's `ImageAttributes::pixel_aspect` carries
the header. It is recorded as a known gap, not done here.

### 5. An unknown token is refused once, as `expandAperture`

`AspectRatioConformPolicy::try_from` fails on any token outside the five, or on a
non-token value. The import raises the new `product.invalid_conform_policy`
(Refused, `Each`) at the prim that authors it, and resolution then falls back as if
the value were unauthored. That gives the settings' value for a product, and
`expandAperture` for the settings. This mirrors `product.invalid_data_window`, which
also covers both prims.

### 6. A later product with another policy: warn, first wins

The policy joins the per-attribute comparisons in `resolve_products` next to
`product.region_mismatch`, under a new `product.framing_mismatch` (Approximated,
`Each`). It is not added to `Base::same_render`, which skips the product. A differing
policy changes framing, not what can be rendered, and is treated like a differing
window. The name `framing` leaves room for aperture offsets later.

### 7. `ls camera --json` reports both

`CameraRecord` gains `conformed_aperture_mm` next to the authored `aperture_mm`. The
`crust-ls/1` schema stays at version 1, because adding a field is compatible. Every
camera listed is conformed with the render's resolved policy and resolution, which is
how it would render if chosen with `--camera`.

## Risks / Trade-offs

- [Stages whose aperture is wider than the image change framing silently.] →
  Mitigation: a "Stages written for older versions" note in `render-settings.md`
  that names the fix (author `aspectRatioConformPolicy = "adjustApertureWidth"` to
  keep the old crop). There is no environment switch: this is a scene setting, not
  an optimisation to A/B (the precedent is `dataWindowNDC`).
- [A Houdini / Solaris export authors the policy at its fallback on every settings
  prim.] → `value_at` already returns `None` for a fallback-equal value that is not
  authored, and an authored `expandAperture` is the default anyway. No warning
  results.
- [The `debug_assert` in Decision 3 could fire on extreme apertures through float
  rounding.] → It is relative 1e-5 and debug-only. The tests cover 36×24 against
  1:1, 2:1 and 1:2.
- [The `openusd-schemas` conform could diverge from C++ in a branch.] → Its own tests
  cover the branches. Ours pin the five policies against hand-computed apertures from
  the USD documentation's table rather than against the function itself.

## Migration Plan

No data migration. The framing change for wide apertures is documented, and the
user-side fix is one authored attribute. Rollback is a revert: no file format or
schema version changes.
