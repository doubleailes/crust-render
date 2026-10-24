# Tasks

## 1. Uniform emission (materials)

- [x] 1.1 In `crust-mtlx`, after the closure tree is built, fold each emission term's `weight` / `color` with `Compiler::fold` and accept a `generalized_schlick_edf` only when absent or with equal folded `color0` / `color90`; record `constant_emission: Option<[f32; 3]>` on the program with `MtlxMaterial::emission`'s per-channel sanitising. Verify with `crust-mtlx` unit tests: a literal `open_pbr_surface` emission folds; an `image`-driven `emission_color` and a `coat_weight = 1` surface do not.
- [x] 1.2 Add `Material::uniform_emission() -> Option<Vec3A>` (default `None`) and implement it for `OpenPBR` (no coat, no fuzz), pattern materials (`constant_emission`, same condition), `Emissive` (unshaped, untextured, two-sided) and `MtlxMaterial` (the folded constant); `MtlxMaterial::emitted` returns the constant. Verify with a bitwise test in `crust-core/tests/resolve.rs` (or beside it) that `emitted_at` equals `uniform_emission` at random hits and angles for every material answering `Some`, with and without `--features jit`.
- [x] 1.3 Check that a stage with a constant-emission MaterialX surface renders bit-identically: `scripts/check_images.sh check` against goldens recorded before 1.1 on `samples/materialx_emissive.usda` and the default scene set.
- [x] 1.4 Update `openspec/specs/materials/design.md` (the emission paragraphs that say an emissive MaterialX surface reports zero hit-free emission) to describe the fold and the `uniform_emission` contract.

## 2. The light density takes the hit's primitive (no behaviour change)

- [x] 2.1 Add the primitive index to `Light::pdf_at_point` and `LightShape::normal_at` (ignored by sphere, affine and rect shapes); pass `WorldHit::prim_id` from `bounce_emission_weight_at` and every other caller (`found_along`, light-path inspection, LPE routing). Verify with `cargo test --workspace` and `scripts/check_images.sh check` (bit-identical on every scene).
- [x] 2.2 Count instructions before and after 2.1 with callgrind on `samples/cornellbox.usda -s 2` (`RAYON_NUM_THREADS=1`), and verify the difference is within the noise of the existing zero-AOV pin (report both totals in the commit message).

## 3. Mesh shape and the mesh-light material (lighting)

- [x] 3.1 Add `MeshShape` (world triangles, area CDF, `kind() = "mesh"`) as `AreaShape::Mesh`: `sample_point` picks a triangle by `u` and rescales it, `normal_at(p, prim)` recomputes triangle `prim`'s geometric normal, `area()` the total, `extent()` the triangles' box, `solid_angle_sampler` `None`. Verify with unit tests in `light/tests.rs`: the sample density integrates to 1 over a two-triangle mesh of unequal areas, `sample_li`'s pdf equals `pdf_at_point` for the sampled point and triangle, and `power` equals `2πAL` for a two-sided mesh.
- [x] 3.2 Add `MeshLightMaterial { inner, emission }`, forwarding everything but emission to `inner`, with `emitted`, `emitted_directional`, `emitted_at` and its `Resolution`'s emission all taken from the shared `Emissive`. Verify with the `resolve.rs` pin (resolve equals per-query shading) extended to the wrapper, and a test that its emission equals the `AreaLight`'s `radiance_toward` from both sides.
- [x] 3.3 Verify unbiasedness with a test scene (a diffuse floor under a non-planar mesh light): light-only, BSDF-only and power-MIS estimates of the floor agree within noise that falls as 1/√N, under `power`, `uniform` and `tree` selection.
- [x] 3.4 Update `openspec/specs/lighting/design.md`: a "Mesh lights" section (decisions 1, 2, 5, 7, 8 of this change's design) and the `materialSyncMode` reading with its reason; `docs/architecture.md` § Invariants for the new `prim` argument.

## 4. Import (usd-scene-import)

- [x] 4.1 Read `MeshLightAPI` on `UsdGeomMesh` prims during traversal: `light_inputs` / `lux_params`, `light:materialSyncMode` (fallback `materialGlowTintsLight`, an unknown token refused with `light.non_finite_input`'s sibling rule as the other token readers do), linking collections; compute the radiance per design decision 5 and bind the wrapper (or the bare `Emissive` under `noMaterialResponse`). Verify with `crust-core/tests/usd_inline.rs` tests for each scenario of the "Mesh light radiance" requirement (camera-ray radiance read back per mode, `normalize` halving under a doubled area).
- [x] 4.2 At `flush_meshes`, build each mesh-light placement's `MeshShape` from its baked or transformed triangles and add the `AreaLight` with its `geom_id`, before light-link resolution; refuse moving and prototype placements. Verify with `usd_inline.rs` tests: a subdivided mesh light's NEE samples lie on the refined surface; a native-instance prototype and a moving mesh warn and stay out of the light list.
- [x] 4.3 Add the six `light.mesh_light_*` `WarningCode`s (all `skipped`) and their rows in `site/content/docs/reference/warnings.md`. Verify with the existing test that keeps the codes and the page in step, plus one `usd_inline.rs` test per cause.
- [x] 4.4 Apply the camera visibility rules (`crust:rayMask`, `crust:light:cameraVisible`, the RenderMan primvar) to the mesh's geometry, never registering it as a transparent emitter. Verify with `usd_inline.rs` tests: a default mesh light is camera-visible; a hidden one is invisible to camera rays and still shadows another light.
- [x] 4.5 List `MeshLightAPI` meshes in `crust ls light` with kind `mesh`, and in `crust check`'s light section. Verify with the listing tests and a `crust check --json -` run on the sample of 4.6.
- [x] 4.6 Add `samples/mesh_light.usda` (a flat MaterialX emissive card with `MeshLightAPI` lighting a diffuse floor and a sphere) and a twin without the API. Verify that at `-s 16 --indirect-clamp 0` the two agree within noise falling as 1/√N (`crust diff` at 16, 64, 256 spp), and that `crust diagnostic` reports a lower baseline MRSE with the API.
- [x] 4.7 Update `site/content/docs/usd/lights.md` (mesh lights supported, the sync modes as crust reads them and why, visibility, what is refused, the phase-1 limits) and the `usd-scene-import` design record. Verify with `zola build` in `site/` (Zola 0.21).

## 5. Diagnostic (diagnostics)

- [x] 5.1 Make `unlit_emitters`' reason name `MeshLightAPI` and up to five emissive prims outside the light list, each refused mesh light with its warning code, and add the numeric `prims` evidence. Verify with `diagnostic/checks.rs` tests for the three scenarios of "Unlit emitters name their remedy", and that the JSON report still validates as `crust-diagnostic/1`.
- [x] 5.2 Update `site/content/docs/help/diagnosing-a-render.md`, `.claude/skills/debug-noise/SKILL.md` and `openspec/specs/diagnostics/design.md` where they say crust has no remedy for unlit emitters. Verify with `zola build`.

## 6. Integration

- [x] 6.1 Measure the mesh-light quad against an equal `RectLight` (same area, radiance and placement) for relMSE at equal time, and the production-style card (4.6) for spp to 5% noise with and without the API; record both in `docs/light_sampling.md` and replace its "Crust has no triangle-mesh lights today" notes.
- [x] 6.2 Run `scripts/bench_ab.sh` on the default scene set (main vs this branch) and verify no throughput regression beyond noise for scenes without mesh lights.
- [x] 6.3 Run the CI set locally: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`, `cargo deny --locked check`.

## Workflow follow-up

- Archive the change once merged (`/opsx:archive`), syncing the four delta specs into `openspec/specs/`.
