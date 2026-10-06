## 1. Baseline and scaffolding

- [ ] 1.1 Build the parent commit's release binary (`bin_before`). Record goldens with
      `scripts/check_images.sh record <dir>` (`--indirect-clamp 0`) and the
      `cornellbox` callgrind instruction count (`RAYON_NUM_THREADS=1`, `-s 2`). Keep
      both paths in the change notes; verify `check_images.sh check <dir>` passes
      against itself.
- [ ] 1.2 Add the `neural-bvh` feature to `crust-rt`, with `openqmc-rs` as an optional
      dependency it enables. Forward it from `crust-core` and `crust-render` as `bvh8`
      is. Add an empty `neural/` module under `cfg(feature)`. Verify `cargo build` and
      `cargo build --features neural-bvh` both succeed, and `cargo deny --locked check`
      is clean.

## 2. Model: hash grid, MLP, optimizer

- [ ] 2.1 Implement the multi-resolution hash grid (`neural/grid.rs`): L = 8, T = 2^14,
      F = 2, resolutions 16 → 1 024, trilinear lookup and its sparse backward pass.
      Verify with a finite-difference gradient test and a test that every lookup reads
      within the table.
- [ ] 2.2 Implement the 68 → 32 → 32 → 1 MLP (`neural/mlp.rs`) with ReLU and a
      sigmoid, forward and backward in plain `f32`. Verify with a finite-difference
      gradient test and a test that forward is bit-identical across
      `scripts/test_simd_matrix.sh -p crust-rt --features neural-bvh`.
- [ ] 2.3 Implement Adam (`neural/adam.rs`) with separate grid and MLP learning rates.
      Verify on a test that fits a known 1-D step function to below a fixed loss.

## 3. Cut, training data and deterministic training

- [ ] 3.1 Implement the subtree-size cut (`neural/cut.rs`, K = 4 096 references) as a
      `(wide node, lane)` bitset over the collapsed BVH4. Verify with a test that the
      cut covers every leaf exactly once, and that two builds give identical bitsets.
- [ ] 3.2 Implement the three-family segment generator (surface-origin 50 %, through
      35 %, short 15 %) from keyed openqmc draws, reaching triangles through static
      instances. Implement exact labelling by the prototype's `hit_any` on the clipped
      segment, with the trained mask. Verify that every label equals a direct
      `Scene::occluded` call on the same segment.
- [ ] 3.3 Implement the trainer (`neural/train.rs`): 64 fixed chunks per batch, rayon
      per chunk, a chunk-ordered sparse merge, class-balanced BCE and a held-out
      validation set. Verify the spec scenario "Training twice at different thread
      counts" (`RAYON_NUM_THREADS=1` and `8` give bit-identical parameters).

## 4. Kernel integration

- [ ] 4.1 Add `ProxyRequest` and `CommitOptions::occlusion_proxy`. Implement the
      eligibility check (triangles directly or through static instances, no motion,
      instanced triangles counted against the threshold), and add
      `Scene::has_occlusion_proxy`. Verify with tests for the three
      "Only eligible scenes get a proxy" scenarios.
- [ ] 4.2 Add a cut-aware `hit_any` that evaluates the proxy at cut children when the
      ray mask matches the trained mask. Consult it at instance descent and from
      `Scene::occluded` on a proxied scene. Verify the "Another mask is exact" and
      "A segment that misses the scene bounds" scenarios, and that `intersect` on a
      proxied scene is bit-identical ("Closest hit ignores the proxy").
- [ ] 4.3 Add `MemoryFootprint::occlusion_proxies`, counted once per shared scene.
      Verify the "A proxied prototype placed many times" scenario.
- [ ] 4.4 Verify the feature-off build changes nothing:
      `scripts/check_images.sh check <dir>` against the 1.1 goldens, and
      `cargo clippy --workspace --all-targets -- -D warnings` with and without the
      feature.

## 5. Phase 1 measurement and the gate

- [ ] 5.1 Add `--neural` to `crust-rt/examples/ray_throughput.rs`
      (`required-features`): exact against proxied Mray/s (min-of-N), FP%, FN%
      (overall, and for segments starting within one finest cell of a surface),
      training seconds, proxy bytes, and a τ sweep. Verify it runs on the default
      fixtures and on `--large`.
- [ ] 5.2 Add the N-BVH error-driven cut refinement as a second cut option, selected in
      the probe. Verify it is deterministic (two runs, identical bitsets) and compare
      it with the size cut in the probe output.
- [ ] 5.3 Add the `crust-render` example `neural_probe`. It loads a USD stage, picks the
      largest eligible prototypes, trains proxies and reports the same columns on a
      shadow-ray distribution from surface points. Document it in the command cookbook
      (`openspec/specs/cli/design.md`). Verify it runs on a
      `scripts/gen_stress_scene.py` instanced scene, and on ALab or the island if
      present.
- [ ] 5.4 Evaluate the gate (design D8: at least 1.25× exact Mray/s on a prototype of
      1 M+ triangles out of cache, with FP + FN ≤ 1 %). Write the numbers, methods and
      verdict into `openspec/specs/intersection-kernel/design.md`, including the
      negative case. Verify the record names the scenes, binaries and commands so the
      numbers can be reproduced.
- [ ] 5.5 If the gate fails: revise this change with `/opsx:update` to drop the
      `rendering` and `cli` deltas and groups 6–7, keep the branch, and stop. If it
      passes, tick this task and continue. Verify by
      `openspec validate neural-bvh-experiment` passing on the revised or unrevised
      change.

## 6. Phase 2: render integration (only if 5.4 passes)

- [ ] 6.1 Add `neural_occlusion` and `neural_min_tris` to `crust_core::Config`
      (`CRUST_NEURAL_OCCLUSION`, `CRUST_NEURAL_MIN_TRIS`, default `off` / 250 000).
      Warn once on a bad value, and warn once when the switch is set on a build without
      the feature. Verify with `Config::from_lookup` tests for each spec scenario under
      "Neural shadow-ray occlusion switch".
- [ ] 6.2 Pass a proxy request from the instancing import only for prototypes with no
      pass-through material bound (cutout or thin-walled straight transmission), never
      from the top-level `WorldBuilder` commit. Verify the "A pass-through prototype" and "Camera rays stay exact" scenarios with an
      integration test on a generated scene.
- [ ] 6.3 Log one INFO line with the proxy count when any were built, and per-proxy
      DEBUG lines (triangles, cut size, training seconds, validation FP/FN). Verify a
      default render with the switch on prints exactly one extra INFO line.
- [ ] 6.4 Add the `--stats` rows (proxy count, a kernel-memory row, training seconds).
      Verify both scenarios of "The stats report shows occlusion proxies".
- [ ] 6.5 Verify switch-off cost and identity on the feature-on build: callgrind on
      `cornellbox` stays within +0.1 % of 1.1's count, and `check_images.sh check`
      passes with the switch unset and set to `off`.
- [ ] 6.6 Measure the bias: relmse on against off at 16 / 64 / 256 spp with
      `--indirect-clamp 0` on the stress scene (and ALab or the island if present),
      plus the render-time A/B with `scripts/bench_ab.sh`. Record the plateau and the
      speed in `openspec/specs/intersection-kernel/design.md` and
      `openspec/specs/rendering/design.md`. Verify the "The error plateaus" scenario
      is answered by the recorded numbers.
- [ ] 6.7 Document the switches: add rows to `docs/architecture.md` § Environment
      switches; update `site/content/docs/reference/environment-variables.md` and the
      architecture limitations page (biased, shadow rays only, no memory saving); and
      add the `--stats` rows to the user docs. Verify `zola build` (0.21) in `site/`
      passes its link check.

## 7. Integration checks

- [ ] 7.1 Run the four CI jobs locally (`cargo fmt --all -- --check`, clippy, tests and
      `cargo deny --locked check`) with and without `--features neural-bvh`, plus
      `scripts/test_simd_matrix.sh -p crust-rt --features neural-bvh`. Verify all are
      green.
- [ ] 7.2 Run `openspec validate neural-bvh-experiment --strict`. Verify it passes.

## Workflow follow-up

- Archive the change with `/opsx:archive` once merged. If the gate failed, archive only
  the revised, record-only change.
