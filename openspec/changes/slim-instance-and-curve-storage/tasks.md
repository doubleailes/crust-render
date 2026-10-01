## 1. Baseline (parent `a50b1a8`)

- [x] 1.1 Build the parent's release binary (`bin_base`) and record
      `scripts/check_images.sh record <dir>`, plus the two Kitchen_set goldens at 16 spp.
- [x] 1.2 Record the `--stats` kernel memory of cornellbox, nested_instancing, curves
      and Kitchen_set_instanced, plus ALab and the island at level 0 with Ptex streamed,
      under the RSS guard.
- [x] 1.3 Record callgrind instruction counts on cornellbox and nested_instancing at
      `-s 2` (`RAYON_NUM_THREADS=1`).

## 2. Kernel

- [x] 2.1 Slim `InstancePrim` to `scene`, `w2l`,
      `motion: Option<Box<InstanceMotion>>`, `geom_id`, `id_offset` and `mask`.
      `transforms_at` recomputes the static normal matrix as a transpose. Give it
      inherent `hit` / `hit_any`.
- [x] 2.2 Remove `CubicCurve` and `Instance` from `PrimNode`.
- [x] 2.3 Add `others`, `instances`, `instance_bounds`, `cubics` and the kind-tagged
      `order` table to `Primitives`. Resolve `bbox` / `clipped_aabb` through `order`,
      keeping the attach-order build index space.
- [x] 2.4 Make `collapse` write tagged ids into leaf `indices`. `Bvh` keeps the three
      arrays and drops `order` and `instance_bounds`.
- [x] 2.5 Add out-of-line `scalar_hit` / `scalar_hit_any` that decode the tag, used by
      both leaf paths.
- [x] 2.6 Update `SceneBuilder::commit` for instances and cubic spans,
      `describe_instances`, `primitive_breakdown`, `accumulate_unique` and
      `primitive_extent_sum`.

## 3. Reporting and tests

- [x] 3.1 Replace `MemoryFootprint::boxed_prims` with `instances` and `cubic_spans`,
      and update the `--stats` labels and the existing footprint tests.
- [x] 3.2 Pin the sizes: `InstancePrim` 96, `CubicCurvePrim` 96, and `PrimNode` still 64.
- [x] 3.3 Add footprint tests for the spec scenarios: `n` static instances cost 96 B
      each with nothing else growing, and `n` cubic spans cost 96 B each.

## 4. Verification

- [x] 4.1 Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`
      and `cargo test --workspace`.
- [x] 4.2 Run `scripts/test_simd_matrix.sh -p crust-rt`, and the pinned nightly clippy
      plus `--features bvh8` tests.
- [x] 4.3 `check_images.sh check` against 1.1: every sample bit-identical, and the
      Kitchen_set pair too. Spot-check `CRUST_TRI_PACKETS=indexed` on the instancing
      samples.
- [x] 4.4 Callgrind against 1.3: report the totals and the `World::intersect` /
      `Scene::occluded` inclusive deltas.
- [x] 4.5 Interleaved `bench_ab.sh` on the default scenes and on the island (`-s 4`).
- [x] 4.6 Re-measure 1.2. Diff the island and ALab frames against the base: they must
      be bit-identical.
- [x] 4.7 Run the island at `--subdiv-level 1` under the guard, and record how far it
      gets.

## 5. Documentation

- [x] 5.1 `openspec/specs/intersection-kernel/design.md`: the instance and curve
      storage, the order-table argument for bit-identity, and the figures.
- [x] 5.2 Mark Deferred item 3 as taken in
      `openspec/changes/compact-triangle-storage/design.md`.
- [x] 5.3 `docs/moana_profile.md`: a memory section with the 4.6 and 4.7 figures.
