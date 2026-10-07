## Context

See `proposal.md` for why. The three workarounds and what replaces each:

| workaround | where | upstream state |
|---|---|---|
| local `xformOp` composer (`compose_xform_ops`, `xform_op_matrix`, `local_matrix_via_openusd`, six-type `resets_xform_stack_at`) | `usd_import/xform.rs` | correct since 0.6. On 0.7.0, `Xformable::local_to_parent_transform` matched C++ USD 26.8 on 18 stacks. That covers every op kind the composer decodes, `!invert!`, suffixes, a leading reset and a time sample. |
| nested native instance skipped (`prototype_prunes`' `is_instance` arm) | `usd_import/instancing.rs` | the 0.5 `debug_assert!` abort is gone since 0.6. `proto_probe` resolves the nested prototype in a debug build. |
| `includeRoot = 1` added to `lightLink` / `shadowLink` rule maps | `usd_import/light_links.rs` (`link_query`) | fixed on main by `fe8e9e8`, after the 0.7.0 bump. Not released. |

Three constraints shape the approach:

- **openusd 0.7 offers transform composition only through the `Xformable` trait.** That
  trait is implemented per typed schema struct, and each struct's `get` checks the
  prim's type. Today's fallback inherits that limit: it reaches six types, and every
  other type composes to identity when the local composer gives up.
- **0.7's `TimeCode` has no default-time arm.** `local_to_parent_transform` is always
  evaluated at a number. `xform_time()` already maps "no `-f`" to 0.0 for this API.
- **openusd main reworks both APIs this change touches.**
  - Transforms: `Xformable::local_transformation(Option<TimeCode>)`,
    `XformQuery::for_prim`, `XformCache`, and C++ semantics for a mid-stack
    `!resetXformStack!`.
  - Collections: `CollectionAPI` replaces `Collection::new`.

  Phase 1 is written against 0.7 so it can land now. Phase 2 is the bump.

## Goals / Non-Goals

**Goals:**

- Every transform is composed by openusd. No `xformOp` semantics are spelled in crust,
  except the op-kind list that feeds one warning (D3).
- Recover the geometry of native instances nested inside prototypes, sharing the inner
  prototype's kernel scenes across every outer placement.
- Leave light-link membership unchanged when its fallback moves upstream.

**Non-Goals:**

- Composing the parent chain in `f64` too (`XformCache`). World matrices stay
  `parent · local` in `f32`. Only the local matrix gains precision.
- Adaptive subdivision of a nested inner prototype by its size on screen. It is always
  built at the uniform level (D5).
- Motion blur from animated `xformOp`s. That gap is unchanged.
- Anything in openusd itself. Every upstream item this change needs is already fixed
  on main.

## Decisions

### D1. One type-independent adapter over `Xformable` (phase 1)

`xform.rs` defines a crate-private newtype around `Prim` that implements openusd's
`SchemaBase` (`KIND = AbstractTyped`), `Imageable` and `Xformable`. Every trait method
it needs is a default method, so the impl is three empty blocks plus `prim()`.
`compose_with_parent` calls `local_to_parent_transform(xform_time())` and
`resets_xform_stack()` on it for **every** prim. That keeps today's scope ("ops apply on
any prim") and removes the six-type dispatch from both functions.

Alternatives considered:

- *Keep the per-type dispatch, widened to every typed schema crust imports.* This is
  still a list that drifts. A missing type is exactly today's identity bug.
- *Wait for `XformQuery::for_prim` (phase 2) and do nothing now.* This blocks a
  verified, self-contained fix on an unscheduled release.
- *Keep the local composer and add `translateX`-style kinds.* This keeps the duplicate
  this change exists to delete.

Phase 2 replaces the newtype with `XformQuery::for_prim` and deletes it. That also
gates ops on `Xformable` prims (C++ semantics: an untyped prim or a `Scope` contributes
no transform) and handles a mid-stack reset. Both are listed as known gaps in the delta
spec until then.

### D2. Precision: compose in `f64`, cast once

The local matrix comes back as `gf::Matrix4d` and is cast by the existing
`usd_mat_to_glam`. Today's composer decodes every op value to `f32` and multiplies in
`f32`. The new local matrix is therefore *more* accurate, but not bitwise equal: every
sample's image may move by ulps. The pixel check is in the migration plan. An
`f32`-faithful path was rejected because it would mean keeping the composer.

### D3. Keep the warning for an unknown op kind

openusd 0.7's `build_op_matrix` maps an unknown kind to identity silently. C++ errors
for it ("Invalid xform opType token"). An authored op that is ignored is a `WARN` in
this codebase, so the adapter checks the kinds in `xform_op_order()` against the
`UsdGeomXformOp` vocabulary and warns once per prim before composing:

- `translate`, `translateX/Y/Z`;
- `scale`, `scaleX/Y/Z`;
- `rotateX/Y/Z` and the six three-axis rotations;
- `orient`, `transform`.

The list is the one piece of `xformOp` vocabulary crust keeps. It only decides a
message, never a matrix. Phase 2 checks whether openusd main reports this case itself
and, if it does, deletes the list.

### D4. A composition error is identity, with a warning

`local_to_parent_transform` returns `Err` for:

- a `!resetXformStack!` after the first entry (`InvalidOpOrder` in 0.7);
- a value it cannot read.

The adapter warns, naming the prim, and uses identity for the local matrix. The parent
transform is still inherited, because `resets_xform_stack` is false unless the reset
leads. Today's composer silently skipped a mid-stack reset and composed every op, which
is a different wrong answer. Neither matches C++, which keeps only the ops after the
last reset. Phase 2 inherits C++'s behaviour from openusd main.

### D5. Nested native instances: splice the inner prototype's parts

In `collect_proto_parts`, a non-root prim that `is_instance()` is resolved:

1. Its prototype comes from `prim.prototype()`.
2. Its parts come from `prototype_parts(stage, inner_path, caches, depth + 1)`, which is
   cached under `(epoch, path)` like every prototype, so the inner geometry is built
   once.
3. Each inner part is appended to the outer list as a clone, with
   `local = this_local · inner.local`. Its `slots` and `scene` are shared by `Arc`.
4. The walk does not descend into the instance prim, as at the top level.

Details:

- **Masks** are each part's own, exactly as `attach_proto_parts` treats a top-level
  instance, whose own prim mask is not applied to its parts either.
- **Pruning** runs before the splice, so an invisible or inactive nested instance
  contributes nothing.
- **Depth** goes through the existing `MAX_INSTANCE_NESTING` guard, which also bounds a
  prototype that reaches itself.
- **Adaptive mode**: the inner prototype is always `ProtoPlace::Shared`.
  - `count_placements` does not descend into instances, so it has no count with which
    to call an inner placement unshared.
  - Sharing is the conservative answer: the uniform level, the same answer any
    placement counted more than once already gets.

Alternative considered: one grouped part per nested instance (`prototype_group`, as
nested `PointInstancer`s do). A native instance is a single placement. Grouping it buys
the BVH nothing and costs every entering ray one more transform. The outer
prototype's own top-level grouping (`TOP_LEVEL_GROUP_MIN_PARTS`) already bounds the box
count when the spliced list is long.

Light links need nothing new: membership is judged on the top-level instance prim, and
that does not change.

### D6. `includeRoot` and the bump (phase 2, blocked)

When `openusd` / `openusd-schemas` move to the first release containing `fe8e9e8`:

- **`link_query`.** Delete the pseudo-root insertion and the
  `include_root.is_some()` / expansion-rule branch that feeds it. An unauthored
  `includeRoot` then reads as `Some(true)` through the definition, so `opinions()`
  compares equal across the index stage and the chunk, and the early "restricts
  nothing" return still fires for an unlinked light.
- **Collection API.** Port `Collection::new(...).compute_membership_query(stage)` to
  `CollectionAPI`.
- **Transforms.** Replace the D1 newtype with `XformQuery::for_prim`, at
  `local_transformation(eval_time())`. `None` now means default time, as every other
  attribute read already does. Delete `xform_time()`.
- **Spec.** Move the two known-gap scenarios in the delta spec to normal behaviour.

`tests/light_linking.rs` is the guard: line 239's unauthored-`includeRoot` case must
still pass, unchanged.

## Risks / Trade-offs

- **Every image moves by ulps (D2).** Accept it once it is shown to be noise. Run
  `scripts/check_images.sh check` against goldens recorded before the change, at
  `-s 16 --indirect-clamp 0`. For any scene that differs, check that `exr_diff` relmse
  falls as 1/√N across 16 / 64 / 256 spp instead of plateauing.
  `cornellbox_transforms_compose_correctly` must pass unchanged.
- **Default value vs. time 0 (phase 1).** Without `-f`, the old composer read an op's
  *default* value. openusd 0.7 evaluates at time 0.0. They differ only for an op that
  authors both a default and time samples. The record notes it, and phase 2 removes it,
  since `None` means default time on main.
- **Import time.** Each prim now reads `xformOpOrder` up to three times: the D3 check,
  `resets_xform_stack` and `local_to_parent_transform`. These are cheap `field` reads
  next to composition. Measure with
  `scripts/bench_ab.sh -n 2 -p "Parse USD stage"` on ALab and Cornell. If it regresses,
  read the order once in the adapter and pass it on.
- **Nested-instance geometry is new.** A stage that relied on the skip renders more.
  That is the intended fix. Count the "Nested native instance … skipped" warnings on
  ALab, the island and the samples before the change, so the image diffs can be
  attributed to their source.
- **Phase 2 has no date.** The change stays open until openusd publishes a release
  after 0.7.0. If that takes too long, phase 2 can be split into its own change and
  this one archived after phase 1. The delta spec already marks its gaps.

## Migration Plan

1. **Record goldens on `main`.** Run `scripts/check_images.sh record` at 16 spp,
   `--indirect-clamp 0`, and keep a release binary for `bench_ab.sh`.
2. **Phase 1, transforms (D1–D4).** Check images, A/B the import, update the record
   and docs.
3. **Phase 1, nested instances (D5).** Rewrite the test, re-check images: only stages
   that emitted the skip warning may differ.
4. **Phase 2 (D6).** When the release lands: bump, port both APIs, delete the
   `includeRoot` insertion and the newtype, then re-run the light-linking tests and the
   image check.

Rollback is a revert per phase. No setting, file format or CLI flag changes. No
environment switch is added: this replaces workarounds with the behaviour they stood in
for, so there is no old behaviour worth A/B'ing beyond the before/after binaries.
