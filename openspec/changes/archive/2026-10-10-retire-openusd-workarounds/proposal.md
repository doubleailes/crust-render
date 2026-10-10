## Why

The USD importer carries three workarounds for openusd bugs. All three bugs have since
been fixed upstream:

- `compose_xform_ops` re-implements `UsdGeomXformable` because openusd 0.5 composed
  multi-op stacks in the wrong order.
- `prototype_prunes` skips native instances nested inside a prototype because openusd 0.5
  aborted on them.
- `link_query` adds UsdLux's `includeRoot = true` fallback itself because openusd's
  collections did not know it.

Each workaround now costs correctness instead of protecting it:

- **Nested native instances.** The skip throws away geometry that openusd 0.6+ reads
  fine.
- **Transform composition.** The local composer composes in `f32`. It only applies
  `!resetXformStack!` on six prim types, and it cannot decode `translateX` / `scaleX`-style
  ops. On those six types it falls back to openusd. On every other type it silently
  returns identity: a `BasisCurves`, `PointInstancer` or `DiskLight` authoring
  `xformOp:translateX` is placed at its parent's origin.

The record has kept the composer only "until those op kinds are checked". They now are:
on 18 stacks covering every op kind the composer decodes, openusd 0.7.0 matches C++ USD
26.8 on every matrix entry, to the 4 decimals compared. The stacks covered: translate, scale, every single-axis
and three-axis rotation, `orient`, `transform`, `!invert!`, pivot suffixes, a leading
`!resetXformStack!`, and a time-sampled translate.

## What Changes

- **Transforms are composed by openusd.**
  - `compose_xform_ops`, `xform_op_matrix`, `local_matrix_via_openusd` and the six-type
    `resets_xform_stack_at` dispatch are deleted.
  - Every prim's local matrix and reset flag come from openusd's `UsdGeomXformable`
    composition, through one type-independent path. Composition is in `f64`, cast to
    `f32` once.
  - Op kinds the local composer could not decode (`translateX/Y/Z`, `scaleX/Y/Z`) now
    compose on every prim type instead of reading as identity off the six types.
  - `!resetXformStack!` is honoured on every prim type, not six.
  - An op kind outside the `UsdGeomXformOp` vocabulary still warns. openusd reads it as
    identity silently, so the warning stays crust's.
  - **Image change**: composition precision moves from `f32` to `f64`. Every checked-in
    sample is expected to change by at most a few ulps of placement. The change is
    verified as noise, not bias, before it lands.
- **Native instances nested inside a prototype are imported.**
  - The inner prototype's parts are spliced into the outer prototype's, with composed
    transforms, instead of being skipped with a warning.
  - **Image change** on any stage that nests native instances: the inner geometry now
    renders. The skip never fires on the Moana island. Whether it fires on ALab or a
    checked-in sample is read off its warning before the change lands.
- **Retire the `includeRoot` fallback insertion. Blocked until openusd's next release.**
  openusd main (`fe8e9e8`, after 0.7.0) reads a collection's fallbacks through its
  schema definition. `LightAPI`'s `lightLink` / `shadowLink` therefore answer
  `includeRoot = 1` on their own. The bump that brings it in also:
  - moves the collection API (`Collection::new` → `CollectionAPI`);
  - replaces `local_to_parent_transform` with `XformQuery::for_prim`, which reads only
    `Xformable` prims and gives a mid-stack `!resetXformStack!` C++ semantics. 0.7
    returns an error for that case.

  That phase is written here and gated on the release. Membership is unchanged by it.
- **Design records and docs follow.**
  - The `usd-scene-import` record's "Known gaps: openusd bugs and workarounds" section
    loses two entries and gains one: the mid-stack reset, until the bump.
  - `nested_native_instance_degrades_gracefully` becomes a test that the nested geometry
    arrives.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `usd-scene-import`:
  - adds a "Transform stacks" requirement: what composes a prim's transform, and on
    which prims;
  - adds a "Native instance nesting" requirement: nested native instances are imported,
    not skipped.

  "Light collection membership" is unchanged: the `includeRoot` fallback keeps its
  behaviour and only changes owner.

## Impact

- **Code**: `crates/crust-core/src/scene/usd_import/`:
  - `xform.rs` shrinks to a thin adapter;
  - `instancing.rs`: `prototype_prunes` loses its nested-instance arm, and
    `collect_proto_parts` gains the splice;
  - `light_links.rs` is touched in phase 2 only.
- **Tests**: `crates/crust-core/tests/usd_scene.rs`:
  - `nested_native_instance_degrades_gracefully` is rewritten;
  - new tests cover an `xformOp:translateX` on a non-`Xform` prim and a reset on a light;
  - `cornellbox_transforms_compose_correctly` stays as the guard.
- **Images**: ulp-level drift on every sample (phase 1, transforms), proved noise with
  `check_images.sh` and the 1/√N check. New geometry only on stages with nested native
  instances.
- **Performance**: transform composition runs once per prim during import. Import time
  is A/B'd on ALab and Cornell with `bench_ab.sh -p "Parse USD stage"`. Render throughput
  is untouched.
- **Dependencies**: phase 2 bumps `openusd` / `openusd-schemas` past 0.7.0. Phase 1
  needs no dependency change.
- **Upstream**:
  - The mid-stack `!resetXformStack!` divergence is already fixed on openusd main.
  - No new upstream issue is needed for anything in this change.
