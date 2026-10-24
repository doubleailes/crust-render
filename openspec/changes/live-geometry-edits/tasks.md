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
- [ ] 4.2 `scripts/check_images.sh` against goldens of the parent commit.
