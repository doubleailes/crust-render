## 1. Baseline

- [ ] 1.1 Build the parent commit's release binary explicitly (`cargo build --release -p crust-render`), then run `scripts/check_images.sh record <dir>`. Verify: one golden per `samples/*.usda` (those whose external references resolve on this machine), with the scene count noted here.
- [ ] 1.2 Record a callgrind instruction count for the import phase of `samples/nested_instancing.usda` and `samples/instancing.usda` (`RAYON_NUM_THREADS=1`, `-s 1`). Verify: the numbers are noted here for comparison in 5.4.

## 2. Malformed topology is refused per prim

- [ ] 2.1 In `usd_import/mesh/source.rs` `mesh_source`, reject a negative `faceVertexCounts` entry before routing. Log one `WARN` naming the prim and the first bad face, and return `None`. Verify: the new test in 2.5 loads, and the other prims on its stage still import.
- [ ] 2.2 Make `triangulate` cast-free: `usize::try_from` per count and `checked_add` on the offset, returning `None` on failure. Fix the misplaced doc comment so it sits on `fn triangulate`, not `type Triangulated`. Verify: a unit test in `mesh/tests.rs` calling `triangulate(&[3, -1], …)` returns `None` without panicking.
- [ ] 2.3 In `collect_proto_parts`, log the same `debug!` the top-level path logs when `mesh_source` returns `None`. Change both messages to say "skipped" without guessing the cause, since the `WARN` from 2.1 names it. Verify: `cargo test -p crust-core` passes.
- [ ] 2.4 In `usd_import/shapes.rs` `curve_segments`, refuse a negative `curveVertexCounts` entry with one `WARN` and return `None`, and map an authored empty `widths` to `[1.0]`. In `usd_import/volume.rs`, use a `checked_mul` chain for `nx * ny * nz` that falls through to the existing mismatch `WARN`. Verify: the tests in 2.5.
- [ ] 2.5 Add inline-stage tests in `crates/crust-core/tests/usd_inline.rs`, one per spec scenario under "Malformed topology is refused per prim":
  - negative face count on a plain mesh, beside a valid mesh;
  - negative face count on a `catmullClark` mesh at level 2;
  - the same mesh as a prototype part;
  - negative curve count;
  - empty `widths`;
  - grid dims `[2147483647, 2147483647, 2147483647]`.

  Each must load with `Ok`. Assert world counts and hits at the valid prims' positions. Verify: `cargo test -p crust-core --test usd_inline` passes in the debug profile.
- [ ] 2.6 Document it in `site/content/docs/usd/geometry.md`: a negative `faceVertexCounts` / `curveVertexCounts` skips the prim with a warning, and an empty `widths` means width 1. Verify: `zola build` in `site/` (0.21) succeeds with no link errors.

## 3. Render settings are validated, not cast

- [ ] 3.1 Add `count_setting(prim, name, min, default) -> u32` in `usd_import/settings.rs`, and route through it:
  - `crust:samplesPerPixel` (min 1);
  - `crust:maxDepth` (min 0);
  - `crust:minSamplesPerPixel` (min 0), replacing its hand-written match without changing its behaviour.

  Verify: `cargo test -p crust-core` passes, including the existing min-spp test.
- [ ] 3.2 Refuse a `resolution` with either component below 1: one `WARN`, and 640×360 is used. Verify: the tests in 3.3.
- [ ] 3.3 Add tests to `crates/crust-core/tests/usd_inline.rs` for `samplesPerPixel = -1` and `0`, `maxDepth = -4`, `maxDepth = 0` (kept), and `resolution = (0, 360)` and `(-640, 360)`. Each asserts the resulting `RenderSettings` field. Verify: `cargo test -p crust-core --test usd_inline` passes.
- [ ] 3.4 Update `site/content/docs/usd/render-settings.md` for `crust:samplesPerPixel`, `crust:maxDepth` and `resolution`: a value out of range is refused with a warning and the default is used. Verify: `zola build` succeeds.

## 4. Nested native instances are imported

- [ ] 4.1 Gate task. Replace `nested_native_instance_degrades_gracefully` (`crates/crust-core/tests/usd_scene.rs`) with `nested_native_instance_is_imported`. Cover the class-prototype stage from the old test plus a `def`-prototype variant, both asserting hits at `x = 0` and `x = 3`. Use a pid-unique temp directory. Run it in the debug profile against the unchanged importer: it must fail on the assertion, not abort. Verify: the failure is an assertion failure, not an abort. If openusd aborts, stop here: keep the skip arm, file the upstream reproduction, and drop the rest of section 4 from this change.
- [ ] 4.2 In `instancing.rs` `collect_proto_parts`, handle `is_instance()` below the root before schema dispatch:
  - resolve `prim.prototype()`, or log a `WARN` and skip if it can't be resolved;
  - fewer than `TOP_LEVEL_GROUP_MIN_PARTS` parts from `prototype_parts(…, depth + 1)` → splice them in with `local = this_local * part.local`;
  - otherwise → push `prototype_group(…, depth + 1)` at `this_local`;
  - don't descend into the instance's children.

  Verify: 4.1 passes.
- [ ] 4.3 Delete the nested-instance arm and its openusd-0.5 commentary from `prototype_prunes`, and correct its doc's `report` sentence. Verify: `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- [ ] 4.4 Add tests:
  - two placements of `_Outer` at different positions, each showing `_Inner` offset from its own placement;
  - an inner prototype of ≥ 64 parts, which goes through the grouped branch;
  - a nesting chain deeper than `MAX_INSTANCE_NESTING`, which loads with the deepest level missing.

  Verify: `cargo test -p crust-core --test usd_scene` passes.
- [ ] 4.5 Update the docs:
  - `openspec/specs/usd-scene-import/design.md` § "Known gaps: openusd bugs and workarounds": remove "Nested native instances are still skipped", record the splice/group rule, and keep the xformOp-fallback paragraph;
  - `docs/issues/README.md`: the tracked openusd version is 0.7;
  - the stale comment at `samples/nested_instancing.usda:99`.

  Verify: `grep -rn "openusd 0.5 cannot" crates samples` returns nothing.

## 5. resetXformStack on every prim

- [ ] 5.1 In `usd_import/xform.rs`:
  - `compose_xform_ops` returns `LocalXform { matrix, resets }`, with `resets` meaning `order[0] == "!resetXformStack!"`;
  - a reset token at any other index is skipped with a `WARN`;
  - rename `local_matrix_at` → `local_xform_at`, and carry `resets` through the openusd fallback;
  - add `child_world(parent, local)`;
  - delete `resets_xform_stack_at`.

  Verify: `cargo build -p crust-core` has no remaining references to it.
- [ ] 5.2 Route all four composition sites through `child_world`: `count_placements` and `traverse_into` in `mod.rs`, `camera.rs`'s camera chain, and `instancing::part_local`. Verify: `cargo test -p crust-core` passes, including `tests/usd_adaptive.rs` (shared/unshared verdicts unchanged).
- [ ] 5.3 Add tests, one per spec scenario under "Transforms honour resetXformStack on every prim":
  - `BasisCurves` under a translated parent;
  - a `PointInstancer` under a translated parent;
  - a reset on a `DiskLight` (light position);
  - a reset inside a prototype;
  - a misplaced reset token.

  Each asserts a hit or light position. Verify: `cargo test -p crust-core --test usd_inline` passes.
- [ ] 5.4 Re-measure the callgrind import counts from 1.2. Verify: no increase on either scene, with the numbers recorded here.
- [ ] 5.5 Document in `site/content/docs/usd/geometry.md` that `!resetXformStack!` is honoured on every transformable prim, and only as the first entry of `xformOpOrder`. Verify: `zola build` succeeds.

## 6. Integration

- [ ] 6.1 Run the three CI legs locally: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`. Verify: all three pass.
- [ ] 6.2 Rebuild explicitly (`cargo build --release -p crust-render`), then run `scripts/check_images.sh check <dir>` against the goldens from 1.1. Verify: every scene is bit-identical.
- [ ] 6.3 Run `openspec validate harden-usd-import --strict`. Verify: it passes.
