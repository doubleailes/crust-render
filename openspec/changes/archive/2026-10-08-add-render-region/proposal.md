## Why

Crust can only render the whole frame. It reads `dataWindowNDC` from
`UsdRenderSettings`, but only to warn that it is ignored
(`usd_import/products.rs`, `warn_unhonoured`). There is no CLI flag for a
crop either.

A render region is useful on its own: look-dev on one object, or re-rendering
a fixed area of a frame. It is also a prerequisite for `add-diagnostic-command`.
That command runs short trial renders on 2–3 crops of the frame. Those trials
are only meaningful if a crop renders exactly the pixels the full frame
would. Two things must stay unchanged:

- the camera, its resolution and the per-pixel sampling keys;
- everything derived from the resolution: ray-cone texture filtering, adaptive
  subdivision's screen rate, and out-of-view culling.

Shrinking the resolution, or moving the camera window, would change all of
these.

## What Changes

- **`dataWindowNDC` is honoured** on the `RenderSettings` prim and on a
  `RenderProduct` (product overrides settings, as for `resolution`):
  - the render traces only the pixels whose centres lie inside the window;
  - it is no longer listed in the "not honoured" warning.
- **`crust render --region X0,Y0,X1,Y1`**: a crop in pixels, top-left
  origin, half-open (`X1`, `Y1` exclusive). It overrides any authored
  `dataWindowNDC`. A region that is empty, or that falls outside the image
  after clipping, is a usage error.
- **Region pixels are bit-identical to the same pixels of a full render**
  whenever the per-pixel sample count does not depend on neighbours
  (`-s 16`, or adaptive sampling with no neighbour tolerance). Rays use
  full-resolution raster coordinates; only the set of tiles traced changes.
- **Output:**
  - the EXR keeps the full resolution as its display window and sets its data
    window to the region, so Nuke and other compositors place the crop
    correctly;
  - the PNG preview holds only the region's pixels;
  - AOV products follow the same rule.
- **`--stats`** reports the region and the share of the frame it covers.
- **Out of scope:**
  - overscan (a window extending past [0, 1]): clipped to the frame, with a
    warning;
  - several regions in one render;
  - per-product regions that differ from the first product's region.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `usd-scene-import`: `dataWindowNDC` is read and resolved like
  `resolution`.
- `cli`: the `--region` flag.
- `rendering`: rendering a sub-rectangle of the frame, and its bit-identity
  with the full frame.
- `image-output`: an EXR's data window and display window, and the PNG of a
  cropped render.

## Impact

- `crust-core`:
  - `RenderSettings` gains a `region` (pixel rectangle, default the full
    frame);
  - `tracer/mod.rs` generates tiles and rows over the region only;
  - the adaptive neighbour hold treats out-of-region neighbours as absent;
  - `Buffer` and `AovFilm` are sized to the region and carry its origin;
  - the importer resolves `dataWindowNDC`.
- `crust-render`: the `--region` flag; the EXR writer sets the data window;
  the PNG writer writes the region.
- Path guiding trains on the region only, so a guided region render differs
  from the same pixels of a guided full render. Guiding is not
  bit-identical across schedules today either. Documented, not fixed.
- Performance: a full-frame render is unchanged. The region is the full frame,
  and the tile generator produces the same tiles.
- Docs:
  - `site/content/docs/reference/command-line.md` (`--region`);
  - `site/content/docs/usd/aovs.md` (`dataWindowNDC` leaves the "not
    honoured" list);
  - the `cli` and `rendering` design records.
