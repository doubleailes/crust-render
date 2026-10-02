## 1. Prerequisites and baseline

- [x] 1.1 Confirm `adaptive-subdivision` is archived, so its "Adaptive subdivision
      level" requirement is in the main `usd-scene-import` spec.
- [x] 1.2 Build the parent commit's release binary (`bin_before`), and record
      `scripts/check_images.sh record <dir>` with it. *(31 goldens at `2abab4c`, the adaptive sample included.)*
- [x] 1.3 Measure `PatchTable::evaluate_basis` throughput: samples per second on a
      refined cube and on one island terrain mesh, single thread. Decide whether an
      allocation-free evaluation call is filed upstream before section 3. *(Single thread, allocations included: `evaluate_basis` 1.49–1.55 M/s on a 256×256 regular grid, 1.47 M/s on the all-Gregory cube at isolation 4; `evaluate` with derivatives 1.18–1.44 M/s; `find_patch` 52–110 M/s; patch table build 90 ms for 65 536 faces. Uniform refinement runs at about 0.57 M vertices/s on the island (its level-1 traverse is 4 min longer for ~137 M vertices), so evaluation is not the bottleneck and nothing is filed upstream for it.)*

## 2. Tessellation core (`scene/tessellate.rs`, no USD)

- [x] 2.1 Edge rates: `n = clamp(ceil(ℓ · σ / t), 1, 2^max)`, rounded up to even (≥ 2)
      on an edge next to an `n`-gon. Unit tests: monotone in `σ`, the clamp at both
      ends, the even rule.
- [x] 2.2 Per-Ptex-quad tessellation from four edge rates: an interior grid of
      `max(bottom, top) × max(left, right)` cells, rings stitched by shorter
      diagonal, and the one-cell-wide case.
- [x] 2.3 Tests on the tessellator alone, over a sweep of rate tuples (1…16 per edge):
      - every interior edge is shared by exactly two triangles;
      - no vertex lies on an edge it did not choose;
      - consistent orientation;
      - the triangle count;
      - `(1, 1, 1, 1)` gives two triangles.

## 3. Evaluation (`scene/subdiv.rs`)

- [x] 3.1 An adaptive path beside `subdivide()`: `refine_adaptive` (isolation
      `clamp(max, 2, 4)`, single-crease patches), the `PatchTable` and `PatchMap`,
      and the refined control values (positions, vertex-interpolated `st`).
- [x] 3.2 Shared-vertex assembly. Evaluate each cage corner once, and each edge point
      once from the edge's lowest Ptex face, walking from its lower cage vertex.
      Evaluate interior points per face. Normals come from `du × dv`. Output the
      same `MeshSource` shape as `subdivide()`.
- [x] 3.3 Tests:
      - a regular grid cage at rate `2^L` everywhere matches uniform level `L`'s
        vertices to rounding;
      - a cage with an extraordinary vertex stays within a measured Gregory bound;
      - a shared edge is bitwise one set of vertices;
      - Ptex corner `(u, v)` round-trip through `PatchMap::find_patch`.

## 4. Import

- [x] 4.1 `mesh_source` chooses the per-face path in adaptive mode for Catmull-Clark
      and bilinear meshes with no face-varying chart, and the per-mesh level
      otherwise. Count both.
- [x] 4.2 Direct meshes: per-edge distance to the box of its cage vertices and limit
      points (decision 2). Prototype versions: `σ = 2^q · s(local)`. *(Superseded for
      prototypes by section 9.)*
- [x] 4.3 The survey's bucket range from cage edge-length extremes (decision 7).
      *(Superseded: section 9 removes the survey.)*
- [x] 4.4 `FaceMap` variant with explicit per-triangle Ptex corners, resolved at the
      hit like the dyadic cell.
- [x] 4.5 `Config::adaptive_per_face` (`CRUST_ADAPTIVE_PER_FACE`, default on).
      `0` takes the per-mesh path for every mesh. Add the `docs/architecture.md`
      row and the user-docs page.

## 5. Tests through USD (`tests/usd_adaptive.rs`)

- [x] 5.1 A large plane-like cage spanning near and far: fine near, rate 1 far, and
      fewer triangles than its per-mesh level.
- [x] 5.2 Watertight: rays aimed along every shared cage edge of a mixed-rate mesh all
      hit. *(24 000 rays along the shared edges of a curved strip, where its rates step from 8 to 1.)*
- [x] 5.3 A face-varying chart and a `loop` mesh fall back, and are counted. Their UV
      texture still resolves.
- [x] 5.4 `CRUST_ADAPTIVE_PER_FACE=0` gives the per-mesh result. The config test
      covers the spelling, as the switch cannot be flipped in-process. *(The config test; the switch is read once per process.)*
- [x] 5.5 Ptex on a per-face mesh: a hit's face id and `(u, v)` agree with the patch
      coordinates of the hit point. *(A unit test in `mesh.rs`: corners resolve to their Ptex coordinates, and edges inside a face to one place from both sides.)*
- [x] 5.6 Streamed against single-stage import: the same hit distances, bitwise. *(The existing `deterministic_across_import_modes` now runs per face, and still holds.)*

## 6. Reporting

- [x] 6.1 `--stats`: `per-face meshes N (fallback: M)` and the edge-rate histogram.
- [x] 6.2 A DEBUG line per tessellated mesh: Ptex faces, triangles, rate range.

## 7. Verification and measurement

- [x] 7.1 `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`, and
      `cargo test --workspace`.
- [x] 7.2 `scripts/check_images.sh check <dir>` against 1.2: every sample
      bit-identical (none sets a target except `subdivision_adaptive.usda`, whose
      change is expected and is diffed separately).
- [x] 7.3 Island under the 56 GiB guard, Ptex streamed, `--subdiv-edge-length 2` at
      the default ceiling of 3: completes. Record peak RSS, kernel memory,
      triangles, the edge-rate histogram, versions, and import and render time,
      against the per-mesh run capped at 1 (38.47 GiB) and uniform level 1
      (51.07 GiB). Include a near-camera crop. *(**Not met, 2026-10-02:** killed at the 56 GiB guard. Every island element is `instanceable = true`, so its terrain meshes are prototype parts placed once and, per decision 7, rated at the placement's distance: whole, as under the per-mesh level. See the design question raised.)* **Met on opensubdiv-rs 0.5.0 (2026-10-02):** completes, peak 24.70 GiB, kernel 13.83 GiB, 63.6 M triangles, total 4:09 (uniform L0: 23.79 / 13.52 / 60.9 M / 3:21; uniform L1: 51.07 / 36.59 / 274.7 M / 8:19). 32 589 per-face meshes, 0 fallbacks, 86 682 shared at level 0; edge rates 1: 48.7 M · 2: 89 k · 3–4: 59 k · 5–8: 63 k. The frame is whole, no crack or hole.
- [x] 7.4 ALab at `--subdiv-edge-length 2`: at most the per-mesh adaptive figures
      (74.2 M triangles, 35.67 GiB peak). *(74.09 M triangles, kernel 9.27 GiB, peak 35.58 GiB, parse 3:38 — at the per-mesh figures, because only 164 of 5 636 subdivision mesh reads were tessellated per face; 5 472 fell back to the per-mesh level (face-varying charts, presumably — ALab's textured meshes), so on ALab this change mostly waits on face-varying patches.)*
- [x] 7.5 Triangle quality on the island: a histogram of the aspect ratio of stitched
      against interior triangles, and render throughput against the per-mesh run
      (`bench_ab.sh`-style interleaving, two reps). *(Island, 2 px, max 3: of 3.02 M refined triangles (`4√3·area / Σ edge²`), interior ≥0.9 21% · 0.5–0.9 71% · 0.1–0.5 7.3% · 0.01–0.1 0.8% · <0.01 0; stitched 19% · 69% · 10.3% · 1.2% · 6 triangles. Throughput, both at `--subdiv-edge-length 2 --subdiv-level 1 -s 16`, interleaved, two reps: per-face Render 3.631 / 3.736 s (min / mean) against per-mesh 3.752 / 3.789 s, within noise; Traverse 4:27 against 5:20 (−16%); peak 30.9 against 39.0 GiB.)*

## 8. Documentation

- [x] 8.1 `openspec/specs/usd-scene-import/design.md`: "Per-face tessellation" under
      Subdivision surfaces, with decisions 1–8, the trap list and the 7.x figures.
      Retire the "one level per mesh" known gap, and keep the face-varying and Loop
      gaps.
- [x] 8.2 `docs/moana_profile.md`: the per-face row beside the per-mesh and uniform
      ones.
- [x] 8.3 User docs (`site/`): `CRUST_ADAPTIVE_PER_FACE` in
      `reference/environment-variables.md`, and the limitation in
      `architecture/limitations.md` updated to the per-face behaviour. *(Also `docs/architecture.md` (`CRUST_ADAPTIVE_FRUSTUM` row) and the `cli/design.md` cookbook. Not built locally: the theme submodule is absent and the installed Zola is 0.22.)*
- [x] 8.4 Upstream issues on `opensubdiv-rs` for face-varying patch tables, and for an
      allocation-free evaluation call if 1.3 asks for one. *(Filed as doubleailes/OpenSubdiv-rs#21, #22 and #23. No allocation-free evaluation issue: 1.3 found evaluation is not the bottleneck.)*

## 9. Shared versus unshared geometry (decision 7, revised 2026-10-02)

- [x] 9.1 The top-level subtree partition, as `stream_roots` computes it but without
      the stream threshold or switch, so single-stage and streamed imports count in
      the same scopes. Before each subtree is walked in adaptive mode, count its
      native placements per prototype path, with the traversal's pruning.
- [x] 9.2 Native instances: a prototype placed once in its subtree is built for that
      placement, each mesh rated through `world_xf · local` (`MeshPlace::World`);
      otherwise the shared version at the uniform level, keyed `(epoch, path)`.
      `PointInstancer`: a target placed once is built for its placement, the others
      shared.
- [x] 9.3 Nested scatters inside an unshared prototype compose the world transform; an
      inner prototype placed more than once is shared.
- [x] 9.4 Remove the rate buckets, the prototype survey, `placement_bucket`,
      `nested_bucket`, the `(epoch, path, q)` keys and the `prototype versions` stat.
      `SubdivPolicy` gains the shared level (the resolved setting, else 0).
- [x] 9.5 `--stats`: `shared meshes N at level L` beside the per-face and fallback
      counts.
- [x] 9.6 Tests (`tests/usd_adaptive.rs`), rewriting those that asserted versions:
      - a `PointInstancer` scatter near and far is one shared level-0 version;
      - an `instanceable` prim placed once is tessellated like the same direct mesh
        (equal hit distances);
      - `--subdiv-level 2` caps unshared rates at 4 and refines shared prototypes to 2;
      - a prototype placed once in each of two top-level subtrees counts as unshared in
        both, streamed and single-stage alike.
- [x] 9.7 Uniform mode is untouched: `check_images.sh check` bit-identical (task 7.2
      again).

## 10. Isolation and view (revised 2026-10-02, after the island measurements)

- [x] 10.1 Isolation depth 1 (`subdiv::ADAPTIVE_ISOLATION`): at 3, the Moana ocean's
      684 416-triangle all-triangle cage cost a 19.9 GiB transient (5.8 GiB at 1). The
      accuracy test is now exact down to the isolation depth and within 0.03 below it
      (measured 0.0185 at rate 4 on the all-extraordinary cube).
- [x] 10.2 Frustum term (`adaptive::Frustum`, `CRUST_ADAPTIVE_FRUSTUM`, default on): a
      cage segment whose box, padded by its own diagonal, is wholly outside the view
      pyramid is split once, decided per edge so faces still agree; a mesh wholly out
      of view takes level 0 on the per-mesh path. The pyramid includes the eye plane,
      since four side planes alone let a box behind the camera through. `ocean_geo1`
      went from 31.2 M to 4.1 M triangles. Tested by
      `geometry_out_of_view_is_not_refined` and the frustum unit tests.
- [x] 10.3 Design and spec: decision 5's isolation level, a decision for the frustum
      term, the spec's frustum clause and scenario, and the proposal's out-of-scope
      line.
- [x] 10.4 The island still fails (2026-10-02): the first 19 subtrees load in
      13.9 GiB, then the ocean's main cage `ocean_geo` (~14.9 M triangles at level 0)
      passes 56 GiB. The patch table is opensubdiv-rs's: about 2.7 KB per Gregory
      patch, every patch of an all-triangle cage. Filed upstream as
      doubleailes/OpenSubdiv-rs#21 (Gregory end-cap storage) and #22 (flat patch
      arrays). Re-run 7.3 once they land. *(Resolved: #21 and #22 shipped in 0.4.0, #29 (selected faces) in 0.5.0; see 7.3.)*
- [x] 10.5 Face-varying charts take the per-mesh fallback (ALab: 5 078 of 5 242 unshared
      reads): filed upstream as doubleailes/OpenSubdiv-rs#23 (face-varying patch tables
      and `EvaluateBasisFaceVarying`). Once it lands, evaluate the chart with the
      face-varying basis and retire the fallback for it. *(Done on opensubdiv-rs 0.4.0: the chart is a refiner channel, isolated with `consider_fvar_channels`, evaluated with smooth face-varying patches (`evaluate_face_varying`), UVs per triangle corner from the corner's own Ptex face so seams keep each side; pinned by `a_seamed_face_varying_chart_keeps_each_side`.)*
- [x] 10.6 opensubdiv-rs 0.4.0 (#21, #22, #23 closed): pinned to tag 0.4.0, no source
      change needed. Patch table per patch, all-triangle cage at isolation 1: Gregory
      2 721 → 1 038 B; regular 179 → 94 B.
- [x] 10.7 The island still fails on `ocean_geo` (~14.9 M-triangle all-triangle cage,
      ~45 M Gregory patches even at 0.4.0's size) although almost every face ends at
      rate 1 (`ocean_geo1`: 4 107 624 triangles from 2 053 248 Ptex faces, so at most a
      few hundred refined). Filed upstream as doubleailes/OpenSubdiv-rs#29 (selected
      faces for `refine_adaptive` and the patch table factory). Once it lands: rate the
      edges first, take rate-1 faces' corners from limit masks, and build patches only
      for faces with a finer edge. *(Done on 0.5.0: edges rated from the cage first; only faces with a finer edge are selected (`refine_adaptive_selected`, `create_with_options_selected`); every other face renders its smooth cage, its corners taking a refined neighbour's limit points; the even rule applies next to selected n-gons only. Pinned by `rate_one_faces_render_their_smooth_cage` and `refined_and_cage_faces_meet_closed`. ALab: 22.8 M triangles, peak 29.46 GiB, Traverse 2:58 against 5:37 before selection.)*
- [x] 10.8 ALab at 2 px with face-varying patches (0.4.0): all 5 242 unshared mesh reads
      per face, 0 fallbacks, 381 shared at level 0; 23.9 M triangles, kernel 4.06 GiB,
      peak 29.32 GiB (uniform L1: 81.8 M / 10.47 / 36.86; uniform L0: 21.2 M / 3.82 /
      ~25); edge rates 1: 20.0 M · 2: 534 k · 3–4: 44 k · 5–8: 37 k. Traverse 5:37 against
      2:48 before face-varying: patch tables for rate-1 faces too, which #29 removes.
