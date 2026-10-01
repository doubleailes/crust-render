## 1. Prerequisites and baseline

- [ ] 1.1 Confirm `usd-driven-subdivision` is archived, so the uniform-level
      requirement is in the main `usd-scene-import` spec.
- [ ] 1.2 Build the parent commit's release binary (`bin_before`). Record
      `scripts/check_images.sh record <dir>` with it, using `--indirect-clamp 0`.
- [ ] 1.3 Record the island baseline at uniform level 0 with streamed Ptex, under an
      RSS guard: peak RSS, kernel memory, render time and the EXR.

## 2. Settings and camera

- [ ] 2.1 Read `float crust:subdivisionEdgeLength` from `RenderSettings` in
      `settings.rs`, ignoring a non-positive or non-finite value with a warning.
- [ ] 2.2 Add `UsdImportOptions::subdivision_edge_length`, and
      `--subdiv-edge-length <px>` on the CLI, rejecting non-positive values at parse
      time.
- [ ] 2.3 Extend `SubdivPolicy` with an adaptive mode: target `t`, `max` (resolved
      level if given, else 3), and the camera projection (`f_px` or the orthographic
      scale, the camera position, the resolution).
- [ ] 2.4 Resolve the camera before traversal, from the index stage when composed
      there, else from a stage population-masked to the camera path with payloads
      loaded. Warn once and use the uniform level when no camera path is known.
      Debug-assert that it agrees with the camera the traversal builds.

## 3. Level function

- [ ] 3.1 Compute the mean cage-edge length `ē` from the authored topology, memoised per
      cage content hash for the load, floored at ε.
- [ ] 3.2 Implement `level(ē, M, aabb_world)`: largest column norm, nearest-point
      distance (0 inside), `ceil(log2(p / t))` clamped to `0..=max`.
- [ ] 3.3 Unit-test the level function: monotone in distance and scale, camera inside
      gives `max`, orthographic is independent of distance, and the clamp at both
      ends.

## 4. Direct meshes

- [ ] 4.1 In `mesh_source`, choose `L` per prim from its world transform and bounds
      before refinement. Keep level 0 as the smooth cage.
- [ ] 4.2 Add a test: two prims referencing one cage near and far get different levels,
      and two at the same distance share a `MeshKey` slot.

## 5. Prototypes

- [ ] 5.1 Key the prototype-parts cache by `(prototype path, epoch, q)`, with
      `q = ceil(log2(s · ρ))`, and use a constant `q` in uniform mode. Uniform-mode
      output must stay bit-identical to 1.2.
- [ ] 5.2 Mark prototypes without subdivision meshes as rate-independent on first
      build, so every `q` aliases them. Canonicalise `q` to the prototype's
      level-changing range.
- [ ] 5.3 Group PointInstancer placements by `q` before building parts, and emit each
      group against its version.
- [ ] 5.4 Let native instances compute their own `q`. Nested instancers inherit the
      outer `d` and compose the scale.
- [ ] 5.5 Add tests:
      - an instancer with near and far placements renders two versions, and the
        materials, Ptex and UV tables still resolve per part;
      - nested scatter is conservative;
      - streamed against `CRUST_STREAM_IMPORT=0` chooses the same level for every
        placement.

## 6. Reporting

- [ ] 6.1 Add the `--stats` lines `subdivision levels` (per placement) and
      `prototype versions`.
- [ ] 6.2 One INFO line when adaptive mode is on, and a DEBUG line per refined mesh with
      `L`, `ē`, `d` and `p`.

## 7. Verification and measurement

- [ ] 7.1 `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings` and
      `cargo test --workspace`.
- [ ] 7.2 `scripts/check_images.sh check <dir>` with no target set: every sample
      bit-identical to 1.2.
- [ ] 7.3 Add a sample scene, `samples/subdivision_adaptive.usda`: one cage instanced
      along a receding row, with a camera and `crust:subdivisionEdgeLength`. Check the
      level histogram and the image.
- [ ] 7.4 Island under the RSS guard with `--subdiv-edge-length 2` (and 4), streamed
      Ptex: peak RSS, kernel memory, the level histogram, prototype versions, import
      and render time against 1.3. Diff the image, and show where it changed with a
      near-camera crop.
- [ ] 7.5 ALab with `--subdiv-edge-length 2` against uniform level 1 (49 GiB peak):
      memory and image.

## 8. Documentation

- [ ] 8.1 `openspec/specs/usd-scene-import/design.md`: an "Adaptive level" subsection
      under Subdivision surfaces, with the metric, the bucketed prototype cache, the
      camera-before-traversal rule, and the 7.4 / 7.5 figures.
- [ ] 8.2 `openspec/specs/cli/design.md`: add `--subdiv-edge-length` to the command
      cookbook.
- [ ] 8.3 `docs/moana_profile.md`: the adaptive figures beside the uniform-level ones.
- [ ] 8.4 Known gaps in `usd-scene-import/design.md`: level popping across frames,
      cracks between separately refined meshes, and no frustum term.
