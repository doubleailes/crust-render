# Proposal

## Why

Depth of field cannot be turned off without editing the camera, and the blur it
gives is not a real lens's. The standard UsdRender switch, `disableDepthOfField`,
is read and refused with `product.unhonoured_attribute`, so a Solaris stage that
turns depth of field off still renders blurred. The lens diameter is
`focalLength / fStop` used directly as a world-space length, although
`focalLength` is authored in millimetres. A 50 mm lens at f/2.8 gets a 17.9-unit
lens: 17.9 cm on a centimetre stage, where a real lens is 1.79 cm, and 17.9 m on
a metre stage, where a real lens is 1.79 cm too. The blur is therefore 10x too
strong on centimetre stages and 1000x too strong on metre stages.
`samples/cornellbox_guided.usda` authors `fStop = 1000` to work around it.

## What Changes

- **BREAKING (images)**: the lens diameter becomes physical. `focalLength` is
  read in millimetres and converted to scene units through the stage's
  `metersPerUnit`: `diameter = focalLength / fStop × 0.001 / metersPerUnit`.
  Every stage that authors a non-zero `fStop` renders with less blur than
  before: 10x less on centimetre stages (and on stages that do not author
  `metersPerUnit`, whose fallback is 0.01), 1000x less on metre stages.
  `focusDistance` stays in scene units, and the field of view is unchanged (it
  depends only on `aperture / focalLength`).
- The import reads the stage's `metersPerUnit`, which it does not read today.
  A value that is not finite or not positive is refused with a new
  `camera.invalid_meters_per_unit` warning and reads as 0.01.
- `disableDepthOfField = true` is honoured. The render goes through a pinhole
  whatever the camera's `fStop`. The value is resolved like `disableMotionBlur`:
  the first product's authored value, else the settings prim's. It leaves the
  `product.unhonoured_attribute` warning, which keeps `pixelAspectRatio`.
- A later product whose effective `disableDepthOfField` differs from the
  first's is written with the first's setting and warned about with a new
  `product.depth_of_field_mismatch` warning, as motion blur already is.
- An MCP session's override layer copies the input's `metersPerUnit` and
  `upAxis`. A new session writes them when it creates the layer, and a resumed
  session adds whichever is missing. Without this, the session stage reads
  metadata from the override layer, which authors none, so it would fall back
  to 0.01, and a session (`render_final` included) over a metre-scale input
  would render a lens 100x wider than `crust render` of the input.
- `samples/cornellbox_guided.usda` drops its `fStop = 1000` workaround, and
  `samples/openpbr_showcase.usda` goes from `fStop = 1000` to `fStop = 100`, so
  it keeps the 0.05-unit lens it was written to show.
- No CLI flag and no environment switch: this is a scene setting, not an
  optimisation to A/B (the same choice `disableMotionBlur` made).

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `usd-scene-import`: adds "Camera depth of field" (the lens diameter from
  `focalLength`, `fStop`, `focusDistance` and the stage's `metersPerUnit`) and
  "Render settings can disable depth of field" (`disableDepthOfField`, resolved
  per product like `disableMotionBlur`, no longer warned about as not honoured,
  and a later product that differs is approximated with
  `product.depth_of_field_mismatch`).
- `mcp-session`: the override layer a session creates, or resumes, carries
  the input's `metersPerUnit` and `upAxis`, so a session renders as
  `crust render` of the input does.

## Impact

- Code: `crates/crust-core/src/scene/usd_import/camera.rs` (`lens`,
  `lens_camera`), `products.rs` (`Base`, `RenderProducts`, `warn_unhonoured`,
  the mismatch check), `mod.rs` (the settings carry the flag),
  `tracer/settings.rs` (a `depth_of_field` field beside `motion_blur`),
  `warnings.rs` (two codes), and the MCP live-edit path in
  `usd_import/live.rs`, which rebuilds the camera from the settings it reads.
  `crust-render/src/mcp/layer.rs` (`new_layer_text`) and `mcp/session.rs`
  (`open_session`, for new and resumed layers).
- Tests: `crust-core/tests/usd_inline.rs` (`f_stop_gives_the_camera_a_lens`
  plus new ones for the diameter in cm and m stages and for the switch), and
  the warnings table test.
- Images: any golden recorded from a stage with a non-zero `fStop` changes. Two
  samples author one: `cornellbox_guided.usda`, which changes only by dropping
  a near-pinhole lens, and `openpbr_showcase.usda`, whose new `fStop = 100`
  keeps its lens at 0.05 units (only float rounding of the diameter moves).
  Every other sample, `PointInstancedMedCity.usd` included, authors
  `fStop = 0` or none.
- Render throughput: none. A pinhole skips the lens sample, as it does today
  for `fStop = 0`.
- Documentation: `site/content/docs/usd/render-settings.md` (a
  `disableDepthOfField` section), `site/content/docs/usd/aovs.md` (the
  not-honoured list), `site/content/docs/reference/warnings.md` (two codes), a "Camera
  depth of field" section in `render-settings.md` for the lens units, the `mcp-session` design record
  (its "override layer carries no `upAxis`" gap is retired), and the `usd-scene-import` and
  `aovs` design records.
- Compatibility: renders no longer match OpenUSD's Hydra camera pixel for pixel
  on stages that are not in centimetres. Hydra reads the lens in tenths of a
  scene unit (`GfCamera::FOCAL_LENGTH_UNIT`), and that agrees with millimetres
  only when `metersPerUnit = 0.01`.
