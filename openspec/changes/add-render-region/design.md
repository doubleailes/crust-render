## Context

See `proposal.md` for the motivation. This section describes the code the
design builds on.

**The frame is the unit of work today.**

- `RenderSettings` holds `width` × `height`.
- `tracer/mod.rs` covers the frame in one of two ways:
  - `generate_tiles(width, height, TILE)`: 16×16 tiles, row-major;
  - `generate_rows`: `width`×1 rows, with `--scanline`.
- Both replay through one gather into a `Buffer` (and an `AovFilm` when AOVs
  are requested) of the full frame.
- `render_pixel(x, y)` keys every sampling domain on the pixel coordinates.
  Camera rays come from `Camera::get_ray(s, t, …)`, with `s`, `t` derived
  from `(x, y)` and the full resolution.
- A pixel's samples therefore do not depend on which work unit traces it.
  This is the existing "tiles ↔ scanlines" bit-identity, pinned bitwise.

**Things that read the resolution, and must keep reading the full frame:**

- `Camera::pixel_span`, used for ray-cone texture filtering;
- adaptive subdivision's screen rate and frustum culling (`usd_import/adaptive.rs`);
- the pixel filter: filter importance sampling, so a sample lands in its
  own pixel and nothing splats across a work-unit border.

**The only cross-pixel coupling is the adaptive neighbour hold.**

- `with_adaptive_neighbour_tolerance`: a pixel keeps sampling while a cross
  neighbour that is still sampling sits above its convergence index.
- At a region border, the neighbour outside the region is never sampled.

**`dataWindowNDC` is parsed only to be refused.**

- `warn_unhonoured` (`usd_import/products.rs`) lists it when authored with a
  value other than `(0, 0, 1, 1)`.

## Goals / Non-Goals

**Goals:**

- Trace only the pixels of a rectangle. Each pixel's value is bit-identical
  to the full render's whenever its sample count does not depend on
  neighbours.
- Honour USD's `dataWindowNDC`, and give the CLI a pixel-space `--region`.
- Write crops that compositors place correctly (EXR data window).

**Non-Goals:**

- Overscan or data windows larger than the display window.
- Multiple regions, or per-product regions.
- Making guided renders region-invariant.

## Decisions

### D1. The region is a pixel rectangle in `RenderSettings`

- `region: PixelRect { x0, y0, x1, y1 }`, half-open, top-left origin, the
  same space as `render_pixel`'s `(x, y)`.
- Default: the full frame. A builder `with_region` clips to the frame and
  refuses an empty result (`Err`, not a panic).
- The engine never sees NDC; the importer and the CLI convert into pixels.

*Alternative considered:* store NDC in the settings, as USD does. Rejected.
Every consumer (tiles, film, writer) wants pixels, and the CLI's region
would round-trip through floats.

### D2. NDC → pixels: pixel centres inside the window

USD's `dataWindowNDC` is `(xmin, ymin, xmax, ymax)`, with `(0, 0)` at the
**bottom-left**. Crust's pixel `y` grows **downwards**. A pixel `(x, y)`
is in the region when its centre is inside the window:

- `xmin ≤ (x + 0.5) / W < xmax`;
- `ymin ≤ 1 − (y + 0.5) / H < ymax`.

Rules:

- windows extending past [0, 1] are clipped, with a warning (overscan is out
  of scope);
- a window that selects no pixel is refused, with a warning, and the full
  frame renders;
- resolution follows `UsdRenderComputeSpec`: the first product's authored
  `dataWindowNDC`, else the settings prim's, else the full frame.

### D3. `--region X0,Y0,X1,Y1` in pixels overrides the stage

- Pixels, because the user reads coordinates off an image viewer, and the
  diagnostic report gives crops in pixels.
- Precedence: `--region` > product > settings > full frame, as for
  `--camera`.
- Parsing:
  - four non-negative integers;
  - `X1 > X0` and `Y1 > Y0`;
  - after clipping to the resolution, non-empty.

  Anything else is a clap usage error, raised before the stage is loaded
  where possible. Clipping needs the resolution, so an empty result is
  refused after import, with an error naming the resolution.

### D4. Work units cover the region only; sampling stays keyed on frame pixels

- `generate_tiles` / `generate_rows` take the region instead of
  `(width, height)`.
- Tiles keep the frame-aligned 16-pixel grid: `x` and `y` are multiples of
  16 clipped to the region, not region-relative. Aligning to the frame
  grid keeps a pixel's work unit, and its scanline-replay order, the same
  as in a full render. That costs nothing, and keeps any future
  per-tile state (cache warmth, progress) comparable.
- `render_pixel` and the camera are untouched: they already use frame
  coordinates.

### D5. Out-of-region neighbours are absent, not converged

- The adaptive neighbour hold skips a neighbour outside the region, exactly
  as it skips one outside the frame today.
- Consequence: a border pixel may stop earlier than it would in a full render
  if its outside neighbour would have held it. That breaks bit-identity
  only when the neighbour tolerance is active. The requirement scopes
  bit-identity to that condition.
- The diagnostic runs its trials with adaptive sampling off, so it is
  unaffected.

### D6. The film is region-sized; the EXR records both windows

- `Buffer` and `AovFilm` are allocated at the region's size and carry its
  origin, so a small crop of a 4K frame costs crop-sized memory.
- EXR:
  - display window: the full `W×H`;
  - data window: the region, with the image's y axis as crust writes it
    (top-down rows);
  - the `exr` crate exposes both, as the layer position and the image's
    display window. Verify in task 4.1 before relying on it.
- The PNG is written at the region's size. A PNG has no data window, and
  padding a 4K canvas with black for a 64-pixel crop helps nobody.
- Products: every product of a render shares the region (one camera, one
  resolution, as today).

### D7. Guiding trains on what it renders

- The guided schedule (training passes, then the final pass) runs over the
  region. The field therefore learns from the region's paths only, and a
  guided region differs from the same pixels of a guided full render.
- This is stated in the `rendering` design record. It is not prevented: a
  guided render is not bit-identical across schedules anyway, and a
  region-trained field is arguably the better field for that region.

## Risks / Trade-offs

- **[A hidden frame-sized assumption in a consumer]** Something may index
  the film by `y * width + x`: the variance map, `PassStats::var_map`,
  `mean_relative_error`, or the LPE film.
  → Mitigation: the film's indexing goes through one `region.index(x, y)`.
  The bitwise test (task 3.3) covers beauty, AOVs and `sampleCount` on a
  region that does not touch the frame's origin, so an offset bug cannot
  hide.
- **[Off-by-one at the NDC conversion]** → unit tests for windows on exact
  pixel boundaries, and for a 1-pixel window.
- **[Stats skew]** Per-pixel stats (`spp_min`, `adaptive: samples / pixel`)
  are over the region, and rays/s are unchanged. The report states the
  region, so a reader does not compare a crop's totals against a full
  frame's.

## Migration Plan

None. With no region authored and no `--region`, the region is the frame,
and every path through the code produces the same tiles as today. The
existing golden images (`scripts/check_images.sh check`) must pass
unchanged.

## Open Questions

- Should `--region` also accept NDC (`--region 0.25,0.25,0.5,0.5`)?
  Deferred; pixels cover the diagnostic's and the user's needs.
