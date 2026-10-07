# Tasks

## 1. Baseline

- [ ] 1.1 Build `main`'s release binary (`bin_before`), then run
      `scripts/check_images.sh record <dir>` with it, using `--indirect-clamp 0`.
      Done when every checked-in sample has a 16 spp golden EXR in `<dir>`.
- [ ] 1.2 With `bin_before`, render each sample, ALab (`-f 1004`) and the island at
      `-s 1 -l debug`, and count the `Nested native instance … skipped` and
      `could not decode the xformOp stack` warnings per scene. Done when the counts are
      written in the change's working notes. They attribute every image diff in groups
      2 and 3 to a source.

## 2. Transforms composed by openusd (phase 1, D1–D4)

- [ ] 2.1 In `usd_import/xform.rs`, add the crate-private `Prim` newtype implementing
      `SchemaBase` (`AbstractTyped`), `Imageable` and `Xformable`. Route
      `compose_with_parent` through its `local_to_parent_transform(xform_time())` and
      `resets_xform_stack()`. Delete `compose_xform_ops`, `xform_op_matrix`,
      `local_matrix_via_openusd`, `local_matrix_at` and the six-type
      `resets_xform_stack_at`. Done when `cargo build -p crust-core` and
      `cornellbox_transforms_compose_correctly` pass.
- [ ] 2.2 Add the D3 op-kind check (one `WARN` per prim naming the unknown op) and the
      D4 error arm (one `WARN`, identity local). Done when unit tests in `xform.rs`
      cover an unknown kind and a mid-stack reset, asserting the matrix and that the
      call returns.
- [ ] 2.3 Add integration tests in `crates/crust-core/tests/usd_scene.rs` for the delta
      spec's "Transform stacks" scenarios. Done when these pass:
      - `xformOp:translateX` on a `BasisCurves` and on a `DiskLight` is placed off its
        parent's origin;
      - a leading `!resetXformStack!` on a `DiskLight` under a translated `Xform` drops
        the parent;
      - the pivot stack from `ops.usda` matches C++ USD's matrix
        (`0 2 0 0 / -2 0 0 0 / 0 0 2 0 / 5 2 0 1`).
- [ ] 2.4 Run `scripts/check_images.sh check <dir>`. For every scene that differs,
      record `exr_diff` relmse at 16 / 64 / 256 spp. Done when every difference falls
      as 1/√N, or is traced to a stack that composed wrongly before (from 1.2's
      counts).
- [ ] 2.5 A/B the import: `scripts/bench_ab.sh -n 2 -p "Parse USD stage"` with
      `bin_before` against the new binary, on `samples/cornellbox.usda` and ALab
      (`-x "-f 1004 --camera … -s 1"`). Done when min and mean are recorded and neither
      regresses beyond noise. If it does, read `xformOpOrder` once in the adapter
      (design, Risks) and re-run.
- [ ] 2.6 Update the docs:
      - `openspec/specs/usd-scene-import/design.md` § "Known gaps: openusd bugs and
        workarounds": replace the xformOp entry with the verification (openusd 0.7.0 vs
        C++ USD 26.8, 18 stacks) and the two remaining gaps (mid-stack reset, ops on
        non-`Xformable` prims), plus the default-vs-time-0 note.
      - `site/content/docs/usd/geometry.md`: one sentence that every `UsdGeomXformOp`
        kind composes, on any prim type.

      Done when `zola build` in `site/` passes.

## 3. Nested native instances (phase 1, D5)

- [ ] 3.1 In `usd_import/instancing.rs`:
      - delete `prototype_prunes`' nested-instance arm and its warning;
      - in `collect_proto_parts`, resolve a non-root instance's prototype, take its
        parts from `prototype_parts(…, depth + 1)` and splice clones with
        `local = this_local · inner.local`, without descending into the instance prim;
      - pass `ProtoPlace::Shared` for the inner prototype.

      Done when `cargo build -p crust-core` passes.
- [ ] 3.2 Replace `nested_native_instance_degrades_gracefully` with
      `nested_native_instance_is_imported`. It covers the delta spec's "Native instance
      nesting" scenarios: both spheres hit at x = 0 and x = 3, two outer placements
      give four hits with one shared inner scene, and an invisible inner instance
      contributes none. Also run the test in a debug build, the build that used to
      abort. Done when `cargo test -p crust-core --test usd_scene nested_native` passes
      in debug and release.
- [ ] 3.3 Re-run `scripts/check_images.sh check <dir>`. Done when the only new
      differences beyond group 2's are scenes that emitted the skip warning in 1.2.
- [ ] 3.4 Update the docs:
      - the `usd-scene-import` design record: delete the "Nested native instances are
        still skipped" gap; add the splice to "Rendering the Moana island" / the
        instancing text; add the adaptive-mode limit (inner prototypes always share)
        to "Known gaps: instancing";
      - `site/content/docs/usd/geometry.md`: the instancing row now says nested native
        instances are imported.

      Done when `zola build` passes.

## 4. Phase 1 integration

- [ ] 4.1 Run CI locally: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast` and
      `scripts/test_simd_matrix.sh -p crust-rt`. Done when all four are green.
- [ ] 4.2 Validate the change with `openspec validate retire-openusd-workarounds
      --strict`. Done when it passes.

## 5. openusd bump (phase 2, D6). Blocked until an openusd release after 0.7.0 contains `fe8e9e8`

- [ ] 5.1 Bump `openusd` / `openusd-schemas` in the workspace `Cargo.toml` and
      `Cargo.lock`. Port `Collection::new(…).compute_membership_query(stage)` to
      `CollectionAPI`, plus any other API break the release notes list. Done when
      `cargo build --workspace` and `cargo deny --locked check` pass.
- [ ] 5.2 In `light_links.rs`, delete the pseudo-root insertion and the
      `include_root.is_some()` / expansion-rule branch in `link_query`, and update its
      doc comment. Done when `cargo test -p crust-core --test light_linking` passes
      unchanged, including the unauthored-`includeRoot` case.
- [ ] 5.3 Replace the D1 newtype with `XformQuery::for_prim(prim)?.local_transformation(eval_time())`
      and `.resets_xform_stack()`, and delete `xform_time()`. Re-check whether openusd
      now reports unknown op kinds, and if it does, delete the D3 list. Turn the delta
      spec's two known-gap scenarios into normal behaviour, with tests:
      - a mid-stack reset keeps only the ops after it (C++: `(0, 2, 0)` for the
        `edge.usda` case);
      - a `Scope`'s ops are ignored.

      Done when those tests and `check_images.sh check` pass.
- [ ] 5.4 Update the `usd-scene-import` design record (`openusd version and API
      history`; delete the remaining workaround gaps; in "Light links are decided after
      the last chunk", say the `includeRoot` fallback now comes from openusd). Done when no "workaround" for an openusd bug remains in
      "Known gaps: openusd bugs and workarounds".

## Workflow follow-up

- If phase 2 stays blocked long after phase 1 lands, split group 5 into its own change
  and archive this one, keeping the delta spec's known-gap scenarios.
- Archive with `/opsx:archive` once groups 1–4 (and 5, unless split) are done.
