## 1. Baseline

- [ ] 1.1 Build the parent commit's release binary into a separate target dir, and keep
      it as the A/B baseline (`bin_before`).
- [ ] 1.2 `scripts/check_images.sh record <dir>` with the baseline, using
      `--indirect-clamp 0` on both sides.
- [ ] 1.3 Record the baseline `--stats` kernel-memory breakdown and peak RSS for
      cornellbox, Kitchen_set, ALab, and the Moana island at level 0 with streamed Ptex.
      Use an RSS guard for the island.
- [ ] 1.4 Record the baseline callgrind instruction counts on cornellbox at `-s 2`
      (`RAYON_NUM_THREADS=1`).

## 2. Packet lane vertices (decision 2)

- [ ] 2.1 Add `Tri4::lane_vertices(lane)`, plus a unit test that it returns, for every
      active lane, the exact vertices `Tri4::new` received, including the padded tail of
      a partial packet.
- [ ] 2.2 Route the scalar tie-break re-test (closest-hit and occlusion) through
      `triangle_intersect` with the lane vertices, while still reading the existing
      `TrianglePrim` for ids. `simd_matches_scalar_bitwise` and the watertight
      shared-edge tests must stay green.

## 3. Resident per-kind arrays (decisions 1, 3, 5, 6)

- [ ] 3.1 Rename the build-time enum to `BuildPrim`, and keep `build.rs` and
      `collapse.rs` working over it unchanged.
- [ ] 3.2 Add `TriRecord` (24 B, with an inline `[u32; 3]` of normal indices and the
      `NO_NORMALS` sentinel), `OtherPrim` (sphere, disk, cylinder, linear curve) and the
      `Prims` container with `tris`, `instances`, `cubics`, `others` and `normals`.
- [ ] 3.3 In `SceneBuilder::commit`, append each smooth mesh's normals once as
      `[f32; 3]`, and give every triangle `base + i` indices. Keep the per-triangle
      validity check that falls back to flat.
- [ ] 3.4 Add a `finish` step after `collapse`:
      - map `BuildPrim` indices to per-kind indices in input order;
      - rewrite `Tri4::prim[lane]` to `tris` indices;
      - rewrite leaf `indices` to kind-tagged `u32`s, and panic with kind and count at
        2^30 or more of one kind;
      - move the payloads out of their boxes;
      - drop the build array kind by kind.
- [ ] 3.5 Switch `Bvh::hit` / `intersect_leaf` and `hit_any` / `occlude_leaf` to
      `Prims`. A packet lane's hit completes through `TriRecord` plus lane vertices; a
      scalar index dispatches on its tag.
- [ ] 3.6 Store `(count, sum, max)` of the top-level primitive extents at commit, so
      that `primitive_extent_sum` no longer reads primitives.

## 4. Slim instances (decision 4)

- [ ] 4.1 Slim `InstancePrim` to `scene`, `w2l`, `motion: Option<Box<Motion { l2w,
      l2w_end }>>`, `geom_id`, `id_offset` and `mask`. Compute the normal matrix as
      `w2l.matrix3.transpose()` per hit. Pin `size_of::<InstancePrim>() == 96`.
- [ ] 4.2 Keep the instance bounds on `BuildPrim` only, and make `describe_instances`
      (`traversal-stats`) recompute an approximate box from the inner bounds and
      `w2l.inverse()`, documented as such.
- [ ] 4.3 Make the nested-instancing, `InstanceHitId` composition and motion-blur
      tests pass unchanged.

## 5. Reporting

- [ ] 5.1 Replace `MemoryFootprint::{prim_nodes, boxed_prims}` with `triangle_records`,
      `instances`, `cubic_spans` and `other_prims`. Update `accumulate_footprint`,
      `primitive_breakdown` and `accumulate_unique`.
- [ ] 5.2 Print the new labels in the `--stats` breakdown in `crust-core/src/stats.rs`.
- [ ] 5.3 Replace `a_triangle_is_one_cache_line` with size pins: `TriRecord` 24,
      `InstancePrim` 96, `CubicCurvePrim` 96, `Tri4` 192.
- [ ] 5.4 Add a footprint test per spec scenario:
      - a smooth-normal mesh reports at most 24 B per triangle plus 12 B per normal
        vertex, excluding nodes, leaves, indices and packets;
      - `n` static instances report at most 96 B each and nothing else per instance.

## 6. Verification

- [ ] 6.1 `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings` and
      `cargo test --workspace`.
- [ ] 6.2 `scripts/test_simd_matrix.sh -p crust-rt`, and on the pinned nightly:
      `cargo +nightly-2026-09-26 test -p crust-rt --features bvh8`.
- [ ] 6.3 `scripts/check_images.sh check <dir>` against the 1.2 goldens: every sample
      bit-identical.
- [ ] 6.4 Callgrind on cornellbox against 1.4: report the instruction delta for
      `Bvh::hit` and in total.
- [ ] 6.5 `scripts/bench_ab.sh -a bin_before -b bin_after` on the bench scenes and the
      island, at level 0 with `-s 4`. Report min and mean Render time.
- [ ] 6.6 Re-measure 1.3: kernel memory per array and peak RSS for each scene.
- [ ] 6.7 Run the island with `--subdiv-level 1` and streamed Ptex under the RSS guard.
      Record whether it completes, and its peak, or how far it got.

## 7. Documentation

- [ ] 7.1 `openspec/specs/intersection-kernel/design.md`: replace the "64 and 80"
      `TrianglePrim` / `PrimNode` paragraph with the resident layout, the per-triangle
      and per-instance byte costs, and the island before/after figures.
- [ ] 7.2 `docs/moana_profile.md`: add a memory section with the 2026-10-01 baseline
      (43.1 GiB, 37.3 GiB streamed, level 1 killed at 56.8 GiB) and the 6.6 and 6.7
      results.
- [ ] 7.3 `docs/architecture.md`: update any `PrimNode` / `boxed prims` references in
      the type table and the invariants (packet ↔ scalar now shares the vertices).
