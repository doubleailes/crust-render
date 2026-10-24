## 1. Kernel and world

- [x] 1.1 `crust-rt`: `Geometry` and `SceneBuilder` are `Clone`;
      `SceneBuilder::geometry`.
- [x] 1.2 `WorldBuilder::keep_inputs`, `World::{replace_geometry,
      check_geometry}`, `GeometryEdit`.

## 2. Import

- [x] 2.1 `flush_meshes` records `MeshOrigin`s and `MeshSlotFacts`; the
      traversal records static spheres; `UsdImportOptions::keep_geometry`.
- [x] 2.2 `Provenance::{movable, reshapable, lights_at}`; `Change::{moved,
      reshaped, light_geometry}`; `Update::geometry`.
- [x] 2.3 The rebuild: instances, baked meshes, spheres, area-light emitters.

## 3. Session

- [x] 3.1 `Applied::Geometry`; the classifier's rows; the session imports with
      kept geometry; a geometry update is new geometry for `probe`.
- [x] 3.2 Tests (`session/live_tests.rs`, `mcp/tests.rs`): baked, instanced and
      mirrored moves, new points on a baked mesh, an area light's move and
      size, a distant light's turn, each bitwise as a fresh session; new points
      on an instanced mesh, new points equal to another mesh, a transform above
      the camera, each `full`.

## 4. Gates

- [x] 4.1 `cargo fmt`, `clippy -D warnings`, `cargo test --workspace`.
- [x] 4.2 `scripts/check_images.sh` against goldens of the parent commit (6905b29):
      all 45 samples in the checkout bit-identical.

## 5. Review (Qodo, #384)

- [x] 5.1 A transform the runs show nothing for, or at an arc source or
      instancer prototype, is `full` (`Provenance::arc_sources`). Tests:
      `moving_a_point_instancer_prototype_part_is_full`,
      `moving_a_mesh_another_prim_references_is_full`.
- [x] 5.2 Reshape keys follow in-place reshapes, within and across batches.
      Test: `a_second_reshape_onto_a_reshaped_mesh_is_full`.
- [x] 5.3 Foreign-layer stamps advance only after the import they trigger
      succeeds. Test: `a_foreign_change_whose_import_failed_is_imported_later`.
- [x] 5.4 A sphere rebuild is emitted at id 0 and re-addressed (no padding).
- [x] 5.5 `SceneBuilder::commit_ref`; `World::replace_geometry` uses it. Test:
      `commit_ref_builds_the_scene_commit_builds`.
