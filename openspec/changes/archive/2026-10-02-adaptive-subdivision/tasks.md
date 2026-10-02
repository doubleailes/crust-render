## 1. Prerequisites and baseline

- [x] 1.1 Confirm `usd-driven-subdivision` is archived, so the uniform-level
      requirement is in the main `usd-scene-import` spec.
- [x] 1.2 Build the parent commit's release binary (`bin_before`). Record
      `scripts/check_images.sh record <dir>` with it, using `--indirect-clamp 0`. *(Done with the branch-parent binary; `check_images.sh` takes no clamp option, so both sides use the default clamp, which is fine for a bit-identity gate. 30 goldens, both Kitchen_set variants included via the ignored `samples/Kitchen_set` link.)*
- [x] 1.3 Record the island baseline at uniform level 0 with streamed Ptex, under an
      RSS guard: peak RSS, kernel memory, render time and the EXR. *(Reused the 2026-10-01 level-0 measurement of the same code, `docs/moana_profile.md`: peak 23.79 GiB, kernel 13.52 GiB, render 0.961 s at `-s 4`.)*

## 2. Settings and camera

- [x] 2.1 Read `float crust:subdivisionEdgeLength` from `RenderSettings` in
      `settings.rs`, ignoring a non-positive or non-finite value with a warning.
- [x] 2.2 Add `UsdImportOptions::subdivision_edge_length`, and
      `--subdiv-edge-length <px>` on the CLI, rejecting non-positive values at parse
      time. *(Also `allow_negative_numbers`, so `-1` is rejected by the parser with the flag named.)*
- [x] 2.3 Extend `SubdivPolicy` with an adaptive mode: target `t`, `max` (resolved
      level if given, else 3), and the camera projection (`f_px` or the orthographic
      scale, the camera position, the resolution). *(The camera projection is perspective only: the camera importer builds no orthographic camera, so there is no orthographic branch to read.)*
- [x] 2.4 Resolve the camera before traversal, from the index stage when composed
      there, else from a stage population-masked to the camera path with payloads
      loaded. Warn once and use the uniform level when no camera path is known.
      Debug-assert that it agrees with the camera the traversal builds.

## 3. Level function

- [x] 3.1 Compute the mean cage-edge length `ē` from the authored topology, memoised per
      cage content hash for the load, floored at ε. *(Computed per cage read rather than memoised: hashing the cage for a memo key is the same one pass over its indices.)*
- [x] 3.2 Implement `level(ē, M, aabb_world)`: largest column norm, nearest-point
      distance (0 inside), `ceil(log2(p / t))` clamped to `0..=max`. *(The stretch is the exact spectral norm, not the largest column norm: the column norm is not an upper bound under a scale applied after a rotation or a shear, and it is not submultiplicative. The two agree when the columns are orthogonal. In `usd_import/adaptive.rs`, not `subdiv.rs`.)*
- [x] 3.3 Unit-test the level function: monotone in distance and scale, camera inside
      gives `max`, orthographic is independent of distance, and the clamp at both
      ends. *(Orthographic: not applicable, see 2.3.)*

## 4. Direct meshes

- [x] 4.1 In `mesh_source`, choose `L` per prim from its world transform and bounds
      before refinement. Keep level 0 as the smooth cage.
- [x] 4.2 Add a test: two prims referencing one cage near and far get different levels,
      and two at the same distance share a `MeshKey` slot.

## 5. Prototypes

- [x] 5.1 Key the prototype-parts cache by `(prototype path, epoch, q)`, with
      `q = ceil(log2(s · ρ))`, and use a constant `q` in uniform mode. Uniform-mode
      output must stay bit-identical to 1.2. *(Uniform mode: all 30 goldens bit-identical.)*
- [x] 5.2 Mark prototypes without subdivision meshes as rate-independent on first
      build, so every `q` aliases them. Canonicalise `q` to the prototype's
      level-changing range. *(The range comes from a survey walk of the prototype's cages, memoised per epoch, rather than from its first build: the first build depends on which placement is met first, which would break determinism. The survey also gives the cage bounds that bound every version.)*
- [x] 5.3 Group PointInstancer placements by `q` before building parts, and emit each
      group against its version.
- [x] 5.4 Let native instances compute their own `q`. Nested instancers inherit the
      outer `d` and compose the scale.
- [x] 5.5 Add tests:
      - an instancer with near and far placements renders two versions, and the
        materials, Ptex and UV tables still resolve per part;
      - nested scatter is conservative;
      - streamed against `CRUST_STREAM_IMPORT=0` chooses the same level for every
        placement. *(`tests/usd_adaptive.rs`. Per-part resolution is checked by material; UV and Ptex tables attach through the same per-version slots, but no test fixture carries them.)*

## 6. Reporting

- [x] 6.1 Add the `--stats` lines `subdivision levels` (per placement) and
      `prototype versions`. *(The histogram counts subdivision meshes read: a direct prim once per placement, a prototype's mesh once per version.)*
- [x] 6.2 One INFO line when adaptive mode is on, and a DEBUG line per refined mesh with
      `L`, `ē`, `d` and `p`.

## 7. Verification and measurement

- [x] 7.1 `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings` and
      `cargo test --workspace`. *(1 176 tests, 0 failures, 42 suites.)*
- [x] 7.2 `scripts/check_images.sh check <dir>` with no target set: every sample
      bit-identical to 1.2. *(All 30 bit-identical.)*
- [x] 7.3 Add a sample scene, `samples/subdivision_adaptive.usda`: one cage instanced
      along a receding row, with a camera and `crust:subdivisionEdgeLength`. Check the
      level histogram and the image. *(Levels 3, 3, 2, 1, 0 for distances 8 to 400 at an 8 px target: `L0 1 · L1 1 · L2 1 · L3 2`, 5 versions, 1 022 unique triangles; pinned by `the_adaptive_sample_refines_each_placement_to_its_distance`.)*
- [x] 7.4 Island under the RSS guard with `--subdiv-edge-length 2` (and 4), streamed
      Ptex: peak RSS, kernel memory, the level histogram, prototype versions, import
      and render time against 1.3. Diff the image, and show where it changed with a
      near-camera crop. *(At the default ceiling 3, both 2 px and 4 px pass the 56 GiB guard mid-import, and so does 2 px capped at 2: the island's terrain and beach meshes are kilometres wide but pass close to `shotCam`, and a per-mesh level refines all of each. 2 px capped at 1 fits: 187.2 M triangles, kernel 27.75 GiB, peak 38.47 GiB, Traverse 6:01, against uniform L0's 60.9 M / 13.52 / 23.79 / 2:37 and uniform L1's 274.7 M / 36.59 / 51.07 / 6:41; 5 285 of 188 959 mesh reads refined; 544 prototype versions (509 rate-dependent). The image at 640×360 / 4 spp is indistinguishable from L0 by eye; relmse 0.18 is sampling divergence, two seeds differ by 1.40. The 35 "contributed no geometry" warnings are uniform mode's too.)*
- [x] 7.5 ALab with `--subdiv-edge-length 2` against uniform level 1 (49 GiB peak):
      memory and image. *(2 px at the default ceiling 3 fits and beats uniform L1 on every count: 74.2 M triangles against 81.8 M, kernel 9.28 against 10.47 GiB, peak 35.67 against 36.86 GiB, parse 3:35 against 4:15, with levels L0 4 678 · L1 444 · L2 272 · L3 240 (more detail near the camera than L1 gives anywhere); 1 734 prototype versions. Images at 1 spp not compared.)*

## 8. Documentation

- [x] 8.1 `openspec/specs/usd-scene-import/design.md`: an "Adaptive level" subsection
      under Subdivision surfaces, with the metric, the bucketed prototype cache, the
      camera-before-traversal rule, and the 7.4 / 7.5 figures.
- [x] 8.2 `openspec/specs/cli/design.md`: add `--subdiv-edge-length` to the command
      cookbook.
- [x] 8.3 `docs/moana_profile.md`: the adaptive figures beside the uniform-level ones.
- [x] 8.4 Known gaps in `usd-scene-import/design.md`: level popping across frames,
      cracks between separately refined meshes, and no frustum term. *(Also: one level per mesh, the gap the island measurement exposed, and the level ignoring motion.)*
- [x] 8.5 User documentation (`site/`, per CLAUDE.md): `--subdiv-edge-length` in
      `reference/command-line.md` (table row and section), `crust:subdivisionEdgeLength`
      in `usd/render-settings.md`, the per-mesh level in `architecture/limitations.md`.
