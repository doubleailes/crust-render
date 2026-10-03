## Why

The importer's contract is that a bad stage produces an `Error` or a `WARN`, never a dead
process. A code audit (2026-10-03) found four places where it does not hold:

- **Malformed counts crash the import.** A negative `faceVertexCounts` entry is cast
  straight to `usize`. With `panic = "abort"` the whole render dies. Every mesh reaches
  this code, because a subdivision failure falls back to the same cage triangulation.
  Negative `curveVertexCounts`, an empty `widths` array, and oversized volume
  `gridDims` fail the same way.
- **Negative render settings become huge values.** `crust:samplesPerPixel` and
  `crust:maxDepth` are cast with `as u32`, so -1 becomes about 4 billion. A
  non-positive `resolution` is cast with `as usize`, which asks for an
  exabyte-sized framebuffer. `crust:minSamplesPerPixel`, right beside them, is already
  guarded against exactly this.
- **`!resetXformStack!` is honoured on only six prim types.** It works on `Xform`,
  `Mesh`, `Sphere`, `Camera`, `SphereLight` and `RectLight`. On `BasisCurves`, a
  `PointInstancer`, the other lights and any other `Xformable`, the token is silently
  dropped, so the prim inherits a transform it asked not to inherit.
- **Nested native instances are dropped.** This works around an openusd 0.5 assertion
  that the design record says was fixed in 0.6.0, and the workspace is on 0.7. The
  geometry is lost, and the warning blames a library version we no longer use.

## What Changes

**Malformed geometry is refused per prim, never fatal:**
- A mesh with a negative `faceVertexCounts` entry is skipped with one `WARN` naming
  the prim and the face. This applies on every path: uniform and adaptive
  subdivision, the cage fallback, and prototype parts.
- `BasisCurves` with a negative `curveVertexCounts` entry are skipped with one `WARN`.
- An authored but empty `widths` array is treated like an unauthored one (width 1).
- Volume `gridDims` whose cell count overflows are refused through the existing
  dims/data mismatch `WARN`.

**Render settings are validated rather than cast:**
- `crust:samplesPerPixel` below 1 is refused with a `WARN`, and the default is used.
- A negative `crust:maxDepth` is refused the same way. Depth 0 stays valid.
- A `resolution` with either component below 1 is refused, and the default
  640×360 is used.
- This matches how `crust:minSamplesPerPixel` is already handled.

**`!resetXformStack!` is read from `xformOpOrder` itself, on every prim:**
- The importer reads the token from the stack it already composes, so it no longer
  depends on the prim's type.
- This applies at all four places transforms are composed: traversal, the placement
  count, prototype parts, and the camera.
- The token counts only as the first entry, as USD defines it. Anywhere else it is
  ignored with a `WARN`.

**Nested native instances are imported:**
- An `instanceable` prim inside another prototype now has its prototype's parts
  spliced in, with the transforms composed. A native instance is one placement, so it
  needs no extra level of kernel indirection.
- The nested prototype's parts are built once per stage epoch and shared, like any
  prototype's.
- The existing nesting-depth cap still applies.
- **Image change:** stages with nested native instances gain the geometry they were
  missing. No checked-in sample has one, so the goldens do not move.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `usd-scene-import`:
  - "Render settings from USD with defaults": out-of-range integer settings are
    refused with a warning instead of being cast.
  - New requirement: malformed geometry topology is skipped per prim with a warning,
    and the load completes.
  - New requirement: transforms honour `!resetXformStack!` on every transformable
    prim.
  - New requirement: native instances nested inside a prototype are imported.

## Impact

- **`crates/crust-core/src/scene/usd_import/`:**
  - `mesh.rs`: topology check before triangulation and subdivision.
  - `shapes.rs`: curve counts and widths.
  - `volume.rs`: `checked_mul` on the grid dimensions.
  - `settings.rs`: validated integer settings.
  - `xform.rs`: reset decided from `xformOpOrder`, and the type-probing
    `resets_xform_stack_at` is removed.
  - `mod.rs`, `camera.rs` and `instancing.rs`: one shared "compose with parent"
    helper.
  - `instancing.rs`: nested native instances spliced in; the skip arm in
    `prototype_prunes` is deleted.
- **Tests:**
  - New inline-stage tests for each malformed input. Each is a load that must
    complete, which `cargo test` runs in debug, so overflow checks are on.
  - New tests for reset on curves and on a `PointInstancer`.
  - `nested_native_instance_degrades_gracefully` (`tests/usd_scene.rs`) is replaced by
    a test asserting the nested geometry arrives where it is placed.
- **Docs:**
  - `site/content/docs/usd/render-settings.md`: the refusals.
  - `site/content/docs/usd/geometry.md`: malformed topology and the reset token.
  - `openspec/specs/usd-scene-import/design.md`: retire the "Nested native instances
    are still skipped" gap and update the openusd workaround section.
  - `docs/issues/README.md`: the openusd version it says the workspace tracks.
- **Performance:**
  - The topology check is one pass over `faceVertexCounts`, which triangulation
    already walks.
  - Reading the reset from `xformOpOrder` replaces up to six schema `get`s per prim
    with zero extra reads, so import gets slightly faster.
  - Render throughput is untouched.
- **Out of scope** (left for later changes from the same audit):
  - the shared attribute-decoder layer;
  - `prune_reason()` consolidation;
  - the openusd composition fallback in `local_matrix_via_openusd`, which still
    covers only the six types;
  - adaptive refinement of a nested native prototype, which stays at the uniform
    level, like any shared prototype.
