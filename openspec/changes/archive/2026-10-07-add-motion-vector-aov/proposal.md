## Why

Compositors add motion blur in Nuke with VectorBlur, which needs a sharp
beauty plus a 2D motion-vector pass in pixels. Crust can produce neither
today. It has no motion-vector AOV. And any prim that authors
`crust:motion:translate` is always blurred in the beauty: the importer warns
that `disableMotionBlur` is "not honoured" and blurs anyway. Blurring a
beauty in Nuke that crust has already blurred doubles the blur. So the
motion data that exists cannot be used for comp blur.

Crust's only motion is a pure world-space translation over the shutter. That
makes the vector exact, with no time samples and no interpolation between
frames, which is the scope wanted here.

## What Changes

- **`motionvector` raw AOV source** (canonical name matching Arnold's):
  - the forward 2D screen-space displacement, in pixels, of the
    camera ray's first hit, from shutter open to shutter close;
  - Nuke's conventions: `+u` right, `+v` up, components named `u` / `v`, so
    a RenderVar named `forward` lands on Nuke's built-in `forward` layer;
  - per-shutter units: the authored `crust:motion:translate` is the
    displacement over the shutter, and the camera's `shutter:open/close`
    are still not read;
  - closest accumulation by default, clear value 0 (escapes, domes, volumes
    and static geometry);
  - the same value whether or not the beauty is motion blurred. The hit is
    rebased to its shutter-open position before projection.
- **`disableMotionBlur` is honoured** on the `RenderSettings` prim, and
  `instantaneousShutter` is read as its synonym:
  - every camera ray is traced at shutter open, so the beauty is sharp;
  - the motion data stays in the scene, so `motionvector` is still written;
  - the "not honoured" warning is no longer emitted for these two
    attributes. It stays for `disableDepthOfField`, `pixelAspectRatio` and
    `dataWindowNDC`.
- Motion-vector channels use lowercase `u` / `v` components. The UV AOV
  keeps `U` / `V`.
- The other renderers' names for 2D vectors (`velocity`, `Vector`,
  `motionFore`, …) are not aliases of `motionvector` in this change.
- **Out of scope:**
  - a 3D (world- or camera-space) velocity source;
  - a backward vector;
  - per-frame or per-second units;
  - reading `shutter:open/close`;
  - camera motion;
  - rotation or deformation motion;
  - Arnold's normalised (non-raw) motion-vector encoding;
  - a CLI switch to disable blur.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `aovs`:
  - a new `motionvector` raw source and its definition;
  - closest is added to the default modes for motion vectors.
- `image-output`: motion-vector channels use `u` / `v` components.
- `usd-scene-import`: `disableMotionBlur` / `instantaneousShutter` on the
  render settings disable the beauty's motion blur, where today they are
  warned about and ignored.

## Impact

- **Code**:
  - `crust-core/src/aov.rs`: the source, its channel kind, its value;
  - the tracer's first hit (`tracer/path.rs`, `FirstHit`): the hit's
    geometry motion and shutter time, AOV instantiation only;
  - `rt_world.rs`: a per-`geom_id` motion record, derived from each
    attached instance's end transform;
  - `tracer/mod.rs`: the shutter-time draw is gated by the new setting;
  - `usd_import/products.rs` and `settings.rs`: read the two flags;
  - `crust-render/src/products.rs`: the `u` / `v` channel names.
- **Performance**: the zero-AOV render is unchanged. The `trace_path`
  `AOV = false` instantiation stays instruction-for-instruction identical,
  pinned as today. With blur disabled, a scene with motion skips the
  shutter draw, as a static scene already does.
- **Dependencies**: none.
- **Docs**:
  - `site/content/docs/usd/aovs.md`: the new source, and Nuke usage;
  - `site/content/docs/usd/geometry.md`: blur can now be disabled;
  - the render-settings page and `architecture/limitations.md`;
  - the `aovs` and `usd-scene-import` design records.
