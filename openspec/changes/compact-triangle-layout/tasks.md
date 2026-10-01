## 1. Prerequisites and baseline

- [ ] 1.1 Confirm `compact-geometry-storage` is implemented and `usd-driven-subdivision`
      is archived. Then rebase the `cli` and `usd-scene-import` deltas on the archived
      main specs.
- [ ] 1.2 Build the parent commit's release binary (`bin_before`), and record
      `scripts/check_images.sh record <dir>` with it, using `--indirect-clamp 0`.
- [ ] 1.3 Record the baseline callgrind instruction count on cornellbox
      (`RAYON_NUM_THREADS=1`, `-s 2`).

## 2. Kernel: shared packet assembly

- [ ] 2.1 Extract `Tri4::from_lanes` (vertices, record indices and masks for up to four
      lanes, with tail padding) and make `Tri4::new` call it. `Packed` must stay
      bit-identical (`simd_matches_scalar_bitwise`).
- [ ] 2.2 Rename `Leaf::pkt_first` / `pkt_count` to `tri_first` / `tri_count`, and
      document that their meaning depends on the layout.

## 3. Kernel: compact layout

- [ ] 3.1 Add `TriangleLayout { Packed, Compact }` and `SceneBuilder::with_layout`,
      defaulting to `Packed`, and store the layout on `Bvh`.
- [ ] 3.2 In compact commit, append each mesh's vertices once to
      `Prims::positions` as `[f32; 3]`, and fill `Prims::meshes[geom_id]` with
      `{ vert_base, normal_base, n_normals }`. Triangle records carry absolute position
      indices.
- [ ] 3.3 In compact `collapse`, emit `tri_refs` instead of packets, in the order the
      packed path would have packed them.
- [ ] 3.4 In compact `intersect_leaf` / `occlude_leaf`, gather each group of up to four
      references into a stack `Tri4` via `from_lanes`, then run the unchanged
      intersector, tie-break and `hit_from_barycentric`. Smooth normals are resolved
      through `MeshBase`, applying the per-triangle "all three indices under
      `n_normals`" rule.
- [ ] 3.5 Add `positions` and `tri_refs` to `MemoryFootprint` and
      `accumulate_footprint`.

## 4. Kernel tests

- [ ] 4.1 Add a layout-equivalence test: the same random meshes (including a partial
      normal array, degenerate slivers, and shared-edge grids that trigger the
      tie-break) committed in both layouts, the same rays, and bitwise-equal
      closest-hit and occlusion results. Include an instanced, nested and moving
      scene.
- [ ] 4.2 Extend `simd_matches_scalar_bitwise` to gathered packets.
- [ ] 4.3 Add a compact footprint test for the spec scenario: at most 24 B per
      triangle plus 24 B per vertex, excluding nodes, leaves and references, and zero
      packets.
- [ ] 4.4 Run `scripts/test_simd_matrix.sh -p crust-rt`, and
      `cargo +nightly-2026-09-26 test -p crust-rt --features bvh8`.

## 5. Host plumbing

- [ ] 5.1 Add `geometry_layout` to `UsdImportOptions`, read `token crust:geometryLayout`
      from `RenderSettings` before traversal with the subdivision level, and warn once
      on an unknown token and fall back to `packed`.
- [ ] 5.2 Pass the resolved layout to every `SceneBuilder` the import and
      `WorldBuilder` create.
- [ ] 5.3 Add `--geometry-layout packed|compact` to the CLI, overriding the stage.
- [ ] 5.4 Print `geometry layout` and the new memory lines in `--stats`. Log the layout
      at DEBUG, and at INFO when it is not the default.
- [ ] 5.5 Add an import test: a stage authoring `crust:geometryLayout = "compact"`
      commits compact, and the CLI override wins.

## 6. Verification and measurement

- [ ] 6.1 `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings` and
      `cargo test --workspace`.
- [ ] 6.2 `scripts/check_images.sh check` with the default layout against the 1.2
      goldens: bit-identical.
- [ ] 6.3 Render every sample at 16 spp with `--geometry-layout compact` and diff it
      against the packed render with `exr_diff`: bit-identical.
- [ ] 6.4 Callgrind on cornellbox, default layout, against 1.3: no instruction
      regression on `Packed`. Also record the compact count.
- [ ] 6.5 `bench_ab.sh`, same binary, `-x "--geometry-layout packed"` against
      `-x "--geometry-layout compact"`, on cornellbox, Kitchen_set, ALab and the island
      at level 0: min and mean Render time.
- [ ] 6.6 Island memory with streamed Ptex under the RSS guard: kernel memory per
      array and peak RSS in both layouts at level 0, and whether level 1 completes in
      compact, with its peak and render time.

## 7. Documentation

- [ ] 7.1 `openspec/specs/intersection-kernel/design.md`: the compact layout, its bit-identity argument,
      the measured speed and memory trade, and when to choose it.
- [ ] 7.2 `openspec/specs/cli/design.md`: add `--geometry-layout` to the command
      cookbook.
- [ ] 7.3 `docs/moana_profile.md`: the 6.6 figures beside the
      `compact-geometry-storage` ones.
- [ ] 7.4 `docs/architecture.md`: the packed ↔ compact bit-identity pair in
      § Invariants.
