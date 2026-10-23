# Proposal

## Why

`crust:maxDepth` was documented as "the maximum number of bounces along a path", but
the integrator counts shaded path vertices: 1 shades the camera hit alone (visible
emission and direct lighting), and 0 shades nothing, so `crust:maxDepth = 0` rendered
an entirely black frame (#344). A negative value was cast straight to `u32` and wrapped
to `u32::MAX`, an unbounded path.

## What Changes

- **The semantics stay, the documentation states them.** `crust:maxDepth` counts shaded
  vertices; 1 is direct lighting only. Counting bounces instead (Arnold's
  `GI_total_depth`) would shift every authored depth by one and change every default
  render, for no gain in what can be expressed.
- **A depth below 1 is refused.** The importer warns with `settings.invalid_value`,
  naming `crust:maxDepth`, and uses the default (32), as it already does for a negative
  `crust:minSamplesPerPixel`.
- No image changes for any depth of 1 or more.

## Impact

- `crust-core/src/scene/usd_import/settings.rs` (the refusal),
  `crust-core/tests/usd_inline.rs` (its test).
- `site/content/docs/usd/render-settings.md` § `crust:maxDepth`.
