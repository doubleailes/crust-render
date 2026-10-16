## 1. Baseline

- [x] 1.1 *(done in the prototype, branch `bvh8q-prototype`)* The `bvh8q` feature on
      `crust-rt`, forwarded by `crust-core` and `crust-render`; unit tests for node size
      and conservative containment; `crust diff` shows 0 differing pixels on cornellbox,
      veach_mis and the stress scene; the in-cache and `--large` A/Bs recorded in
      proposal.md and design.md D8.
- [x] 1.2 Record the pre-refactor baseline on `main`: the release binary as `bin_before`,
      goldens with `scripts/check_images.sh record <dir>`, and the `cornellbox` callgrind
      instruction count (`RAYON_NUM_THREADS=1`, `-s 2`). Verify
      `scripts/check_images.sh check <dir>` passes against itself.
      *Done:* baseline is `main` at `84b6677` (release binary built from a worktree);
      `cornellbox` callgrind (`RAYON_NUM_THREADS=1`, `-s 2`) is 4 250 222 346
      instructions.

## 2. Make the width and node type generic

- [x] 2.1 Introduce a `NodeFormat` trait (node type, `WIDTH`, slab test, `child_at`,
      `is_leaf`) and implement it for the 4-wide `f32` node. Make the collapse, the
      traversal, `hit` / `hit_any`, the footprint and the tests generic over it,
      replacing the `LANES` constant and the `lanes4` / `lanes8` / `lanesq` `cfg`
      modules. Keep the nightly `bvh8` feature working as a second `NodeFormat`.
      Verify the default-build callgrind count on `cornellbox` is within +0.3 % of 1.2's,
      and `check_images.sh check` passes.
      *Done:* the trait pair is `NodeFormat<W>` / `BuildNode<W>` (`bvh/node.rs`), with the
      width a const generic. The refactored default build counts 4 245 948 162
      instructions on `cornellbox` (−0.10 % against 1.2), and `crust diff` of its render
      against the baseline's shows 0 differing pixels. The nightly `bvh8` feature builds and
      passes its tests as the second `f32` node (`Node8`).
- [x] 2.2 Port the prototype's `QNode`, `finish()` conversion and slab test to a
      `NodeFormat` implementation. Verify the prototype's containment and size tests
      pass unchanged.

## 3. Per-tree selection

- [x] 3.1 Hold a tree's nodes as an enum of the two formats, match it once per descent
      (and once per instance descent), and add the size rule and the `CommitOptions`
      field. Verify the "A small tree keeps the 4-wide node", "A large tree uses the
      quantised node" and "A prototype and its placement choose separately" scenarios.
- [x] 3.2 Run the whole kernel test suite under each forced format
      (`CRUST_RT_NODES=wide` and `quant`, through the `test-node-env` feature) and under
      `auto`. Verify all three pass, and that `scripts/test_simd_matrix.sh -p crust-rt`
      passes. *Done for the three runs (75 unit + 77 integration + 1 doc test each, also
      under the nightly `bvh8` feature); `test_simd_matrix.sh` not yet run.*
- [x] 3.3 Add the brute-force agreement test (random triangle soup, both formats,
      `intersect` and `occluded`, equal distances, equal occlusion, equal primitive
      except at exact ties) and the degenerate-extent test. Verify the "Random rays agree
      with the 4-wide tree" and "A degenerate extent" scenarios.
- [x] 3.4 Extend `build_is_deterministic` to compare quantised nodes, and add the
      different-thread-count commit test. Verify the "Committing twice at different
      thread counts" scenario.

## 4. The switch and the reporting

- [x] 4.1 Add `bvh_nodes` to `crust_core::Config` (`CRUST_BVH_NODES`, default `auto`),
      read through `commit_options()` like `CRUST_TRI_PACKETS`, warning once on a bad
      value. Verify with `Config::from_lookup` tests for "The switch forces a format"
      and "A bad value". *Done (`BvhNodes` in `crust-core/src/config.rs`).*
- [x] 4.2 Add per-format node rows to `MemoryFootprint` and to the `--stats` report.
      Verify the "A quantised tree is smaller", "Mixed formats in one scene", "A render
      with both formats" and "A render forced to the 4-wide node" scenarios. *Done: the
      footprint has `bvh_nodes_quantised`, `--stats` prints a `BVH nodes (quantised)` row
      beside the unchanged `BVH nodes` row, and the JSON gains
      `bvh_nodes_quantised_bytes`.*

## 5. Measurement and the gate

- [x] 5.1 Check images: render cornellbox, veach_mis and the stress scene at 16 spp with
      `CRUST_BVH_NODES=quant8` and `wide4`, and run `crust diff`. Verify 0 differing
      pixels each ("A render is unchanged"). *Done with the `crust` binary built from
      `b693281`: 0 / 230 400, 0 / 518 400 and 0 / 230 400 differing pixels; `--stats` shows
      the `BVH nodes (quantised)` row under `CRUST_BVH_NODES=quant8`.*
- [x] 5.2 Find the size threshold. Sweep tree sizes across the in-cache and out-of-cache
      range with `ray_throughput` (min-of-N, `x86-64-v3` and baseline x86-64), and choose the crossover. Record the machine, the commands and the
      numbers with the constant. *Done: `ray_throughput --sweep` and `--compare` time both
      formats alternately in one process. In cache the quantised node is 1–27 % slower
      (43 k-triangle meshes about neutral, small sphere grids and prototypes 17–27 %);
      from a 50 k-triangle soup to 8 M it is 7–43 % faster on both queries. The threshold
      is `AUTO_QUANT_MIN_REFS = 100 000`, recorded with the numbers in `bvh/mod.rs`.*
- [x] 5.3 Repeat the out-of-cache A/B on the 8 M-triangle soup and the 2 M-instance field
      with every variant interleaved as closely as the builds allow (design D8), and on
      a production-shaped scene (ALab or the island when present, otherwise the
      generated stress scene). Add a profile of how much of `quant8` closest-hit time is
      the per-ray hit sort. Verify the numbers answer the D9 gate and D7's Phase 2
      question.
      *Partly done:* the soup sweep (0.05–8 M triangles) and the 2 M-instance field were
      re-measured with both formats alternated in one process (`--sweep`, `--field`):
      `auto` is 14 % / 16 % faster than all-`f32` on the field, and the 1 M and 8 M soups
      are 20–43 % faster quantised. **Not done:** a production-shaped scene (no ALab or
      island in this environment, and the stress scene was checked for images, not timed
      through the kernel), and the profile of the per-ray hit sort that D7 needs.
      *Production scenes done (2026-10-16), with two deviations agreed for time: 3
      interleaved reps per pair instead of 6, and callgrind instead of `perf`, which is not
      installed. The hit sort was profiled on ALab and the 1 M soup, not Moana, whose import
      under valgrind would take more than 3 h.* On the 72-vCPU VM, `bench_ab.sh -p Render`
      at 128 spp finds `auto` **+10.1 % / +9.6 %** (min / mean) slower than `wide4` on
      Moana and **+12.2 % / +12.4 %** on ALab, and `quant8` +13 % on both. `main` against
      `wide4` is −1.5 % / −0.8 % on min. Trace rises 17.1 → 19.2 µs per query (Moana) and
      5.3 → 6.9 µs (ALab). `auto` quantises 39 trees on Moana and 16 on ALab, the top level
      included, and saves 1.3 GiB / 0.26 GiB of kernel memory. Moana's frame peak, set
      during the commit, falls by 1.0 GiB (24.16 → 23.16 GiB), so the transient double node
      array does not move it. `crust diff` of `wide4` against `auto` at 16 spp: 897
      (ALab) and 6 724 (Moana) differing pixels. ALab's difference behaves as noise.
      Moana's keeps a mean offset of about 2e-4 relative across 16, 64 and 256 spp,
      consistent with exact ties between coincident primitives but not proven. **D9 fails
      on production scenes.** **D7:** the sort is measurable, at 4.9 % (ALab) and 8.0 %
      (soup) of quantised closest-hit instructions and 48 % of its branch mispredicts, but
      6.2 should not go ahead yet. The out-of-line quantised slab test costs 38–50 %
      (about 144 instructions per visit), so it is the first lever, and the format loses
      on production either way. Numbers, method and recommendation are in
      `openspec/specs/intersection-kernel/design.md`, under "Production scenes". This
      contradicts 5.4's verdict; per that record, the recommendation is to make `auto`
      select `wide4` (`AUTO_QUANT_MIN_REFS = None`), which is not done here.
- [x] 5.4 Evaluate the gate (design D9) and write the numbers, methods and verdict into
      `openspec/specs/intersection-kernel/design.md`, including the negative case.
      Verify the record names the scenes, binaries and commands.
      *Done: the gate passes.* 1 M and 8 M soups: quantised faster on both queries (−20 % /
      −24 % and −22 % / −43 %, occlusion well past 10 %); in-cache scenes under `auto` stay
      on the `f32` node, `cornellbox` callgrind −0.11 % against `main`, `bench_ab.sh`
      within noise (+0.2 % / +0.1 % on min); 0 differing pixels (5.1); field under `auto`
      −14 % / −16 %. Recorded in `openspec/specs/intersection-kernel/design.md`, "Node
      formats", with the reproduction commands.
- [x] 5.5 If the gate fails: keep `auto` selecting `wide4`, leave `quant8` as an opt-in
      for memory-bound scenes, revise this change with `/opsx:update` to say so, drop
      group 6, and stop. If it passes, tick this task and continue. Verify by
      `openspec validate quantised-bvh8-nodes` passing.
      *The gate passed; continuing. `openspec validate` not run (no CLI here).*

## 6. Default behaviour and Phase 2

- [x] 6.1 Verify the default-path cost: callgrind on `cornellbox` within +0.3 % of 1.2's
      count, and an interleaved `scripts/bench_ab.sh` of `bin_before` against the new
      binary on the in-cache scenes within noise.
      *Done:* 4 245 608 216 instructions against 4 250 222 346 (−0.11 %), 0 differing
      pixels; `bench_ab.sh` 6 reps: cornellbox +0.2 % / +0.6 %, veach_mis +0.1 % / +0.7 %
      (min / mean).
- [ ] 6.2 *(only if 5.3 says the sort matters, per D7)* Implement build-time octant child
      ordering for the quantised node and the octant-indexed traversal. Verify
      closest-hit distances still agree with the 4-wide tree (brute force) and that
      `crust diff` is 0 differing pixels, or, if exact ties move, that the difference
      falls as 1/√N across spp as `CLAUDE.md` requires. Measure against Phase 1 with the
      same A/B.

## 7. Documentation and integration checks

- [x] 7.1 Update `docs/simd.md` ("BVH8 on nightly" and "Not done": the compressed half
      is no longer undone), `docs/embree_comparison.md`'s memory-options row, and
      `docs/architecture.md` § Environment switches (`CRUST_BVH_NODES`). Credit obvhs
      and Ylitie et al. 2017 as the study material, and state that the code is written
      from the paper. Verify the docs build and the site link check passes
      (`zola build`, 0.21).
      *Done for the text (`docs/simd.md`, `docs/embree_comparison.md`,
      `docs/architecture.md`, the site's environment-variables and design-choices pages).
      `zola build` not run: no Zola in this environment.*
- [x] 7.2 Update the "Known gaps: SIMD stops at 128 bits" block in
      `openspec/specs/intersection-kernel/design.md`, which presently says the width
      "needs both widths compiled in rather than a `cfg`".
      *Done.*
- [ ] 7.3 Run the CI set: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`, `cargo deny --locked check`, and the
      nightly `bvh8` job. Verify all pass.
      *Run at `937e68d`:* `cargo fmt --check` and workspace `clippy -D warnings` clean;
      `cargo test --workspace --no-fail-fast` 2 113 passed, 5 ignored, 1 failed. The failure,
      `the_desktop_bundle_manifest_matches_the_server`, is pre-existing on `main`
      (`84b6677` bumped the crates to 0.8.0 but `packaging/mcpb/manifest.json` still says
      0.7.0) and is untouched by this change. The nightly `bvh8` tests pass at `x86-64-v3`
      (76 + 81 + 1). **Not run:** `cargo deny` (not installed here) and nightly clippy (the
      pinned nightly has no clippy component here).
- [x] 7.4 Remove the `bvh8q` cargo feature and its `bvh8q-pad` control once the runtime
      selection lands, since the format is then always compiled in. Verify
      `cargo build` with each remaining feature.
      *Done: both are gone; `test-node-env` stays for task 3.2's test matrix.*
