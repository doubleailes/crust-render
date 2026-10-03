# Tasks

## 1. Ptex requests carry a colour space

- [x] 1.1 Add a `ColorSpace` argument to `AssetLoader::load_ptex` (`crust-core/src/scene.rs`), including the default `NoAssets` impl. Change `material_ptex` to pass `ColorSpace::Gamma22` and to key its cache on `(resolved path, space)`. Verify with `cargo build --workspace`.
- [x] 1.2 In `crust-assets`, apply the curve per request on both the preloaded (`ptex_texture.rs`) and streamed (`ptex_stream.rs`) paths, with `Raw` skipping the `powf(2.2)`. Verify with a new unit test: a `u8` fixture texel of 128 reads `128/255` raw and `(128/255)^2.2` under `Gamma22`, on both paths.
- [x] 1.3 Extend the streamed ↔ preloaded `u8` bit-identity test to `Raw`, and verify it passes. Then verify `samples/ptex_quads.usda` renders bit-identical to before (`scripts/check_images.sh record` on `main`, `check` on the branch).
- [x] 1.4 Update the textures design record (§ Ptex, Known gaps) and the user-docs texture limitations page so that only colour Ptex is gamma-decoded. Verify with `zola build` in `site/`.

## 2. The displacement pass (pure, no USD)

- [x] 2.1 Add the `Displacement` enum (`Constant` / `Uv` / `Ptex` / `Mtlx`, plus `bound`) and `VertexCtx` in crust-core, with `eval`. Verify with unit tests for each variant against hand-computed values. `Mtlx` can be stubbed until group 6.
- [x] 2.2 Add a `HitRecord`-free scalar sampling entry point shared by `UvInput::sample` and `Displacement::Uv`: channel, scale and bias. Verify that `crust-core/tests/resolve.rs` and the preview-surface tests still pass unchanged.
- [x] 2.3 Write `scene/displace.rs` with `displace(source, charts, &Displacement)`. It:
  - moves each unique vertex once along its unit pre-displacement normal;
  - computes area-weighted direction normals for faceted sources;
  - takes the footprint as the longer chart edge at the owner corner;
  - recomputes `smooth_normals` afterwards, except on a faceted cage under `CRUST_SUBDIV=0`;
  - tracks the maximum `|d|`.

  Verify with unit tests:
  - a constant 0.1 on a smooth cube moves every vertex exactly 0.1 along its normal;
  - a ridge's flank normals tilt;
  - a zero displacement leaves positions bit-identical.
- [x] 2.4 Run the per-vertex evaluation in fixed rayon chunks. Verify with a test that the output is bit-identical under `RAYON_NUM_THREADS=1` and with the default pool.

## 3. Import integration, switch and stats

- [x] 3.1 Add `Config::displace` (`CRUST_DISPLACE`, default on) to `config.rs`, with a parse test. Add its row in `docs/architecture.md` § Environment switches and its section in `site/content/docs/reference/environment-variables.md`. Verify the config tests and `zola build`.
- [x] 3.2 Return `Option<Arc<Displacement>>` from material resolution beside the material, cached per `(epoch, path)`, and `None` when `CRUST_DISPLACE=0`. Verify that every existing sample renders bit-identical (`check_images.sh check`).
- [x] 3.3 Build the per-vertex owner chart table. Record `(ptex_face, uv)` and the chart UV for each new vertex in the per-face tessellator's `emit` closure, and only when the mesh is displaced. Fill it from first corners for uniform, level-0 and Loop sources (`UvSource.indices`, `SubdivFaces`, `SubFace`). Verify with a test that on `samples/subdivision.usda`'s textured cube every vertex's owner chart value equals the value of one of its corners.
- [x] 3.4 Call `displace` in `MeshArena::intern` on a key miss, before `triangulate` and the density table, and also on the non-invertible bake path. Verify with a test that two prims sharing a displaced cage produce one slot and one displaced-mesh count.
- [x] 3.5 Dice a displaced `subdivisionScheme = "none"` mesh as `bilinear` in `mesh_source`, under the uniform, per-mesh adaptive and per-face paths. Leave it faceted and cage-only under `CRUST_SUBDIV=0` or `CRUST_DISPLACE=0`. Verify the new "Scheme none with displacement" scenario as a test.
- [x] 3.6 Write the watertightness tests:
  - a face-varying UV cube displaced by a map that is discontinuous across its seams;
  - a Ptex mesh whose neighbouring faces differ along an edge;
  - a per-face adaptive mesh with mixed rates.

  Each test asserts that every interior edge is shared by exactly two triangles, and that rays aimed at seams hit. Reuse the per-face tessellation watertight harness. Verify all pass.
- [x] 3.7 Add `DisplacementCounters` to `stats.rs`: meshes, vertices, time, max `|d|`, at cage resolution, frustum skipped. Print them only when nonzero. Emit the one-per-stage cage-resolution warning at `WARN`, and per-mesh detail at `DEBUG` only. Verify with a stats test that a scene without displacement prints no displacement lines.

## 4. UsdPreviewSurface displacement

- [x] 4.1 Resolve `inputs:displacement` (a constant, or a `UsdUVTexture` with output channel, `scale` and `bias`) through the preview input resolver into `Displacement::Constant` / `Uv`, requesting the texture `Raw` unless `sourceColorSpace` says otherwise. If `ReadPreviewSurface` lacks the field, read the attribute by name. Verify with tests for the "A constant preview displacement" and "A textured preview displacement" scenarios.
- [x] 4.2 Create `samples/displacement.usda` with:
  - a one-quad `none` ground displaced by a small generated height PNG under `samples/textures/`;
  - a Catmull-Clark object with a constant displacement;
  - `crust:subdivisionLevel` set on its RenderSettings.

  Verify with an integration test in `crust-core/tests/` that it loads, and that `--stats` reports a nonzero displaced-mesh count and the expected max `|d|`.
- [x] 4.3 Remove "`displacement` not read" from the `preview_surface.rs` module docs and from the materials and textures design records. Document the inputs in `site/content/docs/usd/materials.md`. Verify with `zola build`.

## 5. RenderMan / Moana displacement

- [x] 5.1 Use an openusd probe on an island material to confirm how `outputs:ri:displacement`, `PxrDisplace` (`dispAmount`, `dispScalar`), `inputs:displacementMap` and any `displacementbound` attribute are authored. Record the findings in the usd-scene-import design record § Moana, then settle the design's open question.
- [x] 5.2 Read `PxrDisplace` the way `PxrDisneyBsdf` is read. Detect the child shader. Take `dispAmount` from the shader or the interface, and the Ptex (`Raw`) from `inputs:displacementMap` or the connected `PxrPtexture` / `PxrTexture` file. Refuse `dispVector` / `modelDispVector` with one warning. Verify with a test stage in `samples/` that uses the existing `f32` Ptex fixture, checking the "RenderMan displacement read through Ptex" scenario numerically: a vertex offset equals `dispAmount` times the texel.
- [x] 5.3 Update `docs/moana_profile.md` and the textures design record's § Ptex "not reproduced" note to reflect what is now read. Verify with a docs link check (`zola build`) where site pages change.

## 6. MaterialX displacement

- [x] 6.1 In `crust-mtlx`, follow `surfacematerial.displacementshader` to a `displacement` node. Expose its `float` input and `scale` as roots of `Compiled`. Refuse a `vector3` input with an error the loader reports once. Verify with a `crust-mtlx/tests/graph.rs` case for each.
- [x] 6.2 In `material/materialx.rs`, build the one-root displacement program with `Program::optimize`, JIT-compiled under `jit`, and evaluated from a vertex `ShadeCtx`: owner `uv` and `uv_width`, local `position` and `normal`, zero `tangent`, `view = normal`. Verify the interpreter ↔ JIT bit-identity test over the new root.
- [x] 6.3 Add a MaterialX displacement to `samples/displacement.usda` (a small `.mtlx` beside it) that uses the same map and scale as the preview-surface object. Verify with a test that the two meshes' displaced vertices match.
- [x] 6.4 Document the displacement node, its object-space semantics and the view-dependent caveat in the materials design record and in `site/content/docs/usd/materials.md`. Verify with `zola build`.

## 7. Adaptive dicing and the displacement bound

- [x] 7.1 Read `float crust:displacementBound` from the mesh prim, else from its material. Set the bound to `|constant|` for constants. Verify with an attribute-precedence unit test.
- [x] 7.2 Pad the per-mesh `Aabb` and the `segment_at` boxes by `bound · max_axis_scale`. Turn off the frustum term for a displaced mesh with no bound and count it. Verify with tests for the "Displaced into view" and "No bound known" scenarios, and check that undisplaced adaptive meshes keep identical edge rates.
- [x] 7.3 Warn once per mesh when the sampled max `|d|` exceeds an authored bound, without clamping. Verify with a test for "A bound that is too small".
- [x] 7.4 Document `crust:displacementBound` and the bilinear rule for `none` meshes in `site/content/docs/usd/geometry.md`. Add the displacement section, its known gaps (hard edges soften, rates ignore displacement, seam step, no vector displacement) and the bound rules to the usd-scene-import design record. Verify with `zola build`.

## 8. Integration checks

- [x] 8.1 Verify that every pre-existing sample is bit-identical with displacement on, and that `samples/displacement.usda` with `CRUST_DISPLACE=0` is bit-identical to the same stage with its displacement inputs removed. Use `scripts/check_images.sh` at `-s 16 --indirect-clamp 0`.
- [x] 8.2 Verify that `samples/displacement.usda` imported streamed and with `CRUST_STREAM_IMPORT=0` renders bit-identical.
- [x] 8.3 Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast` and the pinned-nightly legs from CLAUDE.md, and verify all are green.
- [x] 8.4 Measure import time and memory with `--stats` on `samples/displacement.usda` and on ALab (no displacement, expecting no change). Measure the island at its documented 2 px / ceiling-3 setting with displacement on, if the data is available. Record the figures in the usd-scene-import design record.
