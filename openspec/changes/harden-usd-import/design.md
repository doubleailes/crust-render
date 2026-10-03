## Context

See `proposal.md` for the four defects. This section covers only what shapes the fix.

**One mesh path:**
- `mesh_source` (`usd_import/mesh.rs`) reads `faceVertexCounts` / `faceVertexIndices`
  for every mesh, direct or prototype part.
- It then picks a route: uniform `subdivide`, per-face `tessellate_adaptive`, or the
  base cage.
- Both subdivision routes run `validate_cage`, which already rejects a count below 3.
  But a failure there falls back to `cage(...)`, and the cage goes to `triangulate`,
  which casts `fc as usize`.
- So the crash is reachable from every route. `release` has no overflow checks and
  `panic = "abort"`, so the wrapped offset becomes an out-of-bounds index and the
  process dies.

**One transform composer:**
- `compose_xform_ops` (`usd_import/xform.rs`) already reads `xformOpOrder` and skips
  `!resetXformStack!` wherever it appears.
- Whether the prim resets is answered separately by `resets_xform_stack_at`. It probes
  six schema types and calls each one's `resets_xform_stack()`, which re-reads the
  same `xformOpOrder`.
- Four sites combine the two answers into "reset ? local : parent · local":
  `count_placements` and `traverse_into` in `mod.rs`, `camera.rs:135`, and
  `instancing::part_local`.
- `count_placements` must agree with `traverse_into`. Otherwise a prototype's
  shared/unshared verdict changes, which changes adaptive levels.

**Native prototypes:**
- `prototype_parts` (`instancing.rs`) builds and caches a prototype's `ProtoPart`s,
  keyed by `(epoch, path)`.
- `placed_parts` decides how a top-level placement attaches them: parts one instance
  each when fewer than `TOP_LEVEL_GROUP_MIN_PARTS` (64), else one grouped part.
- `collect_proto_parts` walks a prototype. `prototype_prunes` currently cuts off any
  `is_instance()` prim below the root, before any schema lookup.
- On openusd 0.5 the schema lookup was what tripped the
  `materialize_prototype` assertion. The design record's "Known gaps: openusd bugs"
  says 0.6.0 resolves the nested prototype in a debug build. The workspace is on 0.7.0.

## Goals / Non-Goals

**Goals:**
- Every finding in the proposal fixed at the point where all callers share it, so no
  route is left unprotected.
- No image change on any checked-in sample. `check_images.sh` must report every scene
  bit-identical, because none of them authors malformed counts, a reset token, or a
  nested native instance.
- Import gets no slower: the reset fix removes schema probes rather than adding reads.

**Non-Goals:**
- The general attribute-decoder layer from the audit (`value_at` + `decode_*`). This
  change adds no new decoders. The settings helper below is local to `settings.rs`.
- Making `local_matrix_via_openusd` cover every `Xformable`. It is only reached when an
  op kind cannot be decoded, and it keeps its six-type reach.
- Applying a native instance prim's own `crust:rayMask` to its prototype's parts.
  Top-level native instances do not do this either, so nested ones match.
- Adaptive (unshared) refinement of nested native prototypes.

## Decisions

### 1. Validate topology once, in `mesh_source`, before routing

`mesh_source` checks `faceVertexCounts` for a negative entry right after reading it,
before choosing subdivision or cage:
- On failure it logs one `WARN` naming the prim and the first bad face, and returns
  `None`.
- Both callers (the top-level emitter and `collect_proto_parts`) already treat `None`
  as "no geometry".
- The prototype walk currently drops `None` silently. Give it the same `debug!` the
  top-level path logs.

`triangulate` also stops casting:
- It converts each count with `usize::try_from` and advances the offset with
  `checked_add`.
- On failure it returns `None` rather than panicking, so a future caller that skips
  `mesh_source` cannot reintroduce the crash.

Alternatives considered:
- **Make `triangulate` skip negative faces and continue.** Rejected: once a count is
  negative, the counts no longer say where the next face's indices start. Every later
  face would read the wrong vertices and render as plausible-looking garbage, which is
  worse than a refused prim.
- **Reuse `validate_cage`.** Rejected: it also rejects counts of 1–2 and an
  index-length mismatch. `triangulate` deliberately tolerates both today (it skips the
  face and keeps the offset aligned), so tightening that is a behaviour change outside
  this proposal.

### 2. Curves, widths and grid dims get the same local guards

- `curve_segments` checks `curveVertexCounts` for a negative entry up front and refuses
  the prim with one `WARN`. Today an overrun `break`s the loop; that stays as it is.
- An authored empty `widths` array is mapped to the unauthored default `[1.0]` where it
  is read, so `width_of` never indexes an empty array.
- In `emit_volume`, `nx * ny * nz` becomes a `checked_mul` chain. `None` takes the
  existing dims/data mismatch `WARN` and skip.

### 3. Render-setting integers go through two small validators

Add `count_setting(prim, name, min, default) -> u32` in `settings.rs`:
- It reads `custom_i32`; a value below `min` gets a `WARN` naming the attribute and the
  value, and the default is used.
- `crust:samplesPerPixel` uses `min = 1`.
- `crust:maxDepth` and `crust:minSamplesPerPixel` use `min = 0`. The existing
  hand-written `minSamplesPerPixel` match collapses into this helper, unchanged in
  behaviour.

`resolution`:
- Either component below 1 refuses the whole pair with one `WARN`, and 640×360 is used.
- Refusing the pair, rather than one axis, keeps the authored aspect ratio from being
  silently half-applied.

Alternative considered: **clamp to the minimum**, as the CLI's `with_samples_per_pixel`
does with `max(1)`.
- Rejected for USD: an authored `-1` is a mistake, not a request for 1 sample.
- The existing `minSamplesPerPixel` refusal already set this precedent, and the site's
  render-settings page documents it.
- The CLI keeps its clamp, because a host flag is a different contract.

### 4. Reset is part of the composed local transform

`compose_xform_ops` returns `LocalXform { matrix: GMat4, resets: bool }`:
- `resets` is `order.first() == "!resetXformStack!"`, read from the same token list
  that is being composed.
- A reset token at any other index is skipped with a `WARN`. Today it is skipped
  silently. openusd's own composer returns `InvalidOpOrder` for it; a warning and
  carry-on matches the importer's refuse-and-continue rule.
- `local_matrix_at` becomes `local_xform_at`. When it falls back to openusd's
  composition, `resets` is still the value from the order it read.
- An unreadable order (not a `TokenVec`) keeps today's behaviour: no reset, plus the
  existing fallback warning.

One helper, `child_world(parent: GMat4, local: LocalXform) -> GMat4`, replaces the
four hand-written combinations. `resets_xform_stack_at` and its six schema probes are
deleted.

Alternatives considered:
- **Extend `resets_xform_stack_at` to probe every `Xformable` type.** Rejected: it
  would still be a list that can fall behind the prim types the importer reads, and it
  costs a schema `get` per type per prim at all four sites.
- **Call openusd's `Xformable::resets_xform_stack`.** Rejected: it needs a typed
  schema handle, which is the probing we want to remove. It also reads only the
  `default` field, while the composer reads at `eval_time()`. Using one read for both
  answers guarantees they never disagree.

### 5. Nested native instances: splice small prototypes, group large ones

In `collect_proto_parts`, a prim below the root with `is_instance()` is handled before
schema dispatch, in the slot the `prototype_prunes` arm used to occupy:
- Resolve `prim.prototype()`. If it fails or returns `None`, log a `WARN` and skip the
  prim.
- Get the inner parts from `prototype_parts(stage, &inner, caches, depth + 1)`. That
  call is cached per `(epoch, path)` and built `Shared`.
- Attach them with the same rule as `placed_parts`:
  - fewer than `TOP_LEVEL_GROUP_MIN_PARTS` parts: splice each one in, with
    `local = this_local * part.local`;
  - otherwise: push the prototype's cached group (`prototype_group`) as one part at
    `this_local`.
- Do not descend into the instance's children. They are instance proxies of the same
  geometry.

Alternatives considered:
- **Always splice.** Rejected: an outer prototype holding hundreds of nested instances
  of a large prototype would put one BVH box per inner part per occurrence into the
  outer prototype. That is the blow-up the top-level grouping threshold exists to
  prevent.
- **Always group,** as nested `PointInstancer`s do. Rejected: a nested instancer groups
  because each of its parts spans a whole scatter. A native instance is a single
  placement, and grouping a three-part prototype costs every entering ray one more
  transform for nothing.
- **Mirroring `placed_parts`** means the outer prototype treats a nested instance
  exactly as the stage root would treat a top-level one.

The skip arm in `prototype_prunes` is deleted, together with its 0.5 commentary. The
function's stale doc ("the placement count's walk passes `false`") is corrected; it
has one caller.

`count_placements` still does not descend into instances. A nested prototype therefore
has no placement count, so it is `Shared` and refined at the uniform level, as the spec
states.

### 6. Tests run where the crash happens

- Each malformed-input test is a `Scene::from_usd` of a small inline stage that must
  return `Ok`.
- `cargo test` builds with debug assertions and overflow checks. A regression
  therefore panics in the test itself instead of passing silently in a release build.
- Assertions are on geometry (counts of world entries, ray hits at known positions),
  not on log text: the crate has no log-capture harness, and adding one is out of
  scope.

## Risks / Trade-offs

- **[Risk] openusd 0.7 still trips the nested-prototype assertion in some shape the
  0.6 probe did not cover.** → The rewritten test runs in a debug build, where the
  assertion fires. It is written first (task 4.1), and both the class-prototype and
  def-prototype shapes from the old test's notes are covered. If either aborts, keep
  the skip arm, report upstream, and ship the other three fixes.
- **[Risk] The `(epoch, path)` cache ignores nesting depth.** A prototype first reached
  at the nesting cap is cached truncated and reused at shallower depths. → Accepted:
  native composition cannot form cycles, so the cap of 8 is only reachable on
  pathological stages, which already get a `WARN`. Nested `PointInstancer`s share the
  same property today.
- **[Trade-off] A reset token at a non-zero index now warns once per prim.** That count
  grows with the stage. CLAUDE.md allows a `WARN` whenever something authored is
  refused, and this is malformed authoring, so the per-prim count is acceptable.
- **[Risk] The `child_world` refactor changes which matrix `count_placements` uses, and
  so a prototype's shared/unshared verdict.** → Both `count_placements` and
  `traverse_into` go through the same helper. The adaptive-subdivision tests
  (`tests/usd_adaptive.rs`) and `check_images.sh` on `subdivision_adaptive.usda` pin
  the verdicts.
- **[Risk] `check_images.sh` reuses a stale binary.** It only builds when
  `target/release/crust-render` is missing, so a `check` could report bit-identical
  against the old code. → The tasks run `cargo build --release -p crust-render`
  explicitly before every `check`. Fixing the script itself belongs to the CI/tooling
  change from the same audit.
