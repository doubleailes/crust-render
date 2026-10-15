## 1. Baseline

- [x] 1.1 *(done in the prototype, branch `bvh8q-prototype`)* The `bvh8q` feature on
      `crust-rt`, forwarded by `crust-core` and `crust-render`; unit tests for node size
      and conservative containment; `crust diff` shows 0 differing pixels on cornellbox,
      veach_mis and the stress scene; the in-cache and `--large` A/Bs recorded in
      proposal.md and design.md D8.
- [ ] 1.2 Record the pre-refactor baseline on `main`: the release binary as `bin_before`,
      goldens with `scripts/check_images.sh record <dir>`, and the `cornellbox` callgrind
      instruction count (`RAYON_NUM_THREADS=1`, `-s 2`). Verify
      `scripts/check_images.sh check <dir>` passes against itself.

## 2. Make the width and node type generic

- [ ] 2.1 Introduce a `NodeFormat` trait (node type, `WIDTH`, slab test, `child_at`,
      `is_leaf`) and implement it for the 4-wide `f32` node. Make the collapse, the
      traversal, `hit` / `hit_any`, the footprint and the tests generic over it,
      replacing the `LANES` constant and the `lanes4` / `lanes8` / `lanesq` `cfg`
      modules. Keep the nightly `bvh8` feature working as a second `NodeFormat`.
      Verify the default-build callgrind count on `cornellbox` is within +0.3 % of 1.2's,
      and `check_images.sh check` passes.
- [ ] 2.2 Port the prototype's `QNode`, `finish()` conversion and slab test to a
      `NodeFormat` implementation. Verify the prototype's containment and size tests
      pass unchanged.

## 3. Per-tree selection

- [ ] 3.1 Hold a tree's nodes as an enum of the two formats, match it once per descent
      (and once per instance descent), and add the size rule and the `CommitOptions`
      field. Verify the "A small tree keeps the 4-wide node", "A large tree uses the
      quantised node" and "A prototype and its placement choose separately" scenarios.
- [ ] 3.2 Run the whole kernel test suite under each forced format
      (`CRUST_BVH_NODES=wide4` and `quant8`) and under `auto`. Verify all three pass, and
      that `scripts/test_simd_matrix.sh -p crust-rt` passes.
- [ ] 3.3 Add the brute-force agreement test (random triangle soup, both formats,
      `intersect` and `occluded`, equal distances, equal occlusion, equal primitive
      except at exact ties) and the degenerate-extent test. Verify the "Random rays agree
      with the 4-wide tree" and "A degenerate extent" scenarios.
- [ ] 3.4 Extend `build_is_deterministic` to compare quantised nodes, and add the
      different-thread-count commit test. Verify the "Committing twice at different
      thread counts" scenario.

## 4. The switch and the reporting

- [ ] 4.1 Add `bvh_nodes` to `crust_core::Config` (`CRUST_BVH_NODES`, default `auto`),
      read through `commit_options()` like `CRUST_TRI_PACKETS`, warning once on a bad
      value. Verify with `Config::from_lookup` tests for "The switch forces a format"
      and "A bad value".
- [ ] 4.2 Add per-format node rows to `MemoryFootprint` and to the `--stats` report.
      Verify the "A quantised tree is smaller", "Mixed formats in one scene", "A render
      with both formats" and "A render forced to the 4-wide node" scenarios.

## 5. Measurement and the gate

- [ ] 5.1 Check images: render cornellbox, veach_mis and the stress scene at 16 spp with
      `CRUST_BVH_NODES=quant8` and `wide4`, and run `crust diff`. Verify 0 differing
      pixels each ("A render is unchanged").
- [ ] 5.2 Find the size threshold. Sweep tree sizes across the in-cache and out-of-cache
      range with `ray_throughput` (min-of-N, `x86-64-v3` and baseline x86-64), and choose the crossover. Record the machine, the commands and the
      numbers with the constant.
- [ ] 5.3 Repeat the out-of-cache A/B on the 8 M-triangle soup and the 2 M-instance field
      with every variant interleaved as closely as the builds allow (design D8), and on
      a production-shaped scene (ALab or the island when present, otherwise the
      generated stress scene). Add a profile of how much of `quant8` closest-hit time is
      the per-ray hit sort. Verify the numbers answer the D9 gate and D7's Phase 2
      question.
- [ ] 5.4 Evaluate the gate (design D9) and write the numbers, methods and verdict into
      `openspec/specs/intersection-kernel/design.md`, including the negative case.
      Verify the record names the scenes, binaries and commands.
- [ ] 5.5 If the gate fails: keep `auto` selecting `wide4`, leave `quant8` as an opt-in
      for memory-bound scenes, revise this change with `/opsx:update` to say so, drop
      group 6, and stop. If it passes, tick this task and continue. Verify by
      `openspec validate quantised-bvh8-nodes` passing.

## 6. Default behaviour and Phase 2

- [ ] 6.1 Verify the default-path cost: callgrind on `cornellbox` within +0.3 % of 1.2's
      count, and an interleaved `scripts/bench_ab.sh` of `bin_before` against the new
      binary on the in-cache scenes within noise.
- [ ] 6.2 *(only if 5.3 says the sort matters, per D7)* Implement build-time octant child
      ordering for the quantised node and the octant-indexed traversal. Verify
      closest-hit distances still agree with the 4-wide tree (brute force) and that
      `crust diff` is 0 differing pixels, or, if exact ties move, that the difference
      falls as 1/√N across spp as `CLAUDE.md` requires. Measure against Phase 1 with the
      same A/B.

## 7. Documentation and integration checks

- [ ] 7.1 Update `docs/simd.md` ("BVH8 on nightly" and "Not done": the compressed half
      is no longer undone), `docs/embree_comparison.md`'s memory-options row, and
      `docs/architecture.md` § Environment switches (`CRUST_BVH_NODES`). Credit obvhs
      and Ylitie et al. 2017 as the study material, and state that the code is written
      from the paper. Verify the docs build and the site link check passes
      (`zola build`, 0.21).
- [ ] 7.2 Update the "Known gaps: SIMD stops at 128 bits" block in
      `openspec/specs/intersection-kernel/design.md`, which presently says the width
      "needs both widths compiled in rather than a `cfg`".
- [ ] 7.3 Run the CI set: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`, `cargo deny --locked check`, and the
      nightly `bvh8` job. Verify all pass.
- [ ] 7.4 Remove the `bvh8q` cargo feature and its `bvh8q-pad` control once the runtime
      selection lands, since the format is then always compiled in. Verify
      `cargo build` with each remaining feature.
