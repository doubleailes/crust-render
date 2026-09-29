## 1. Baseline

- [x] 1.1 Record goldens for every sample scene with the current binary (`scripts/check_images.sh record <dir>`) and keep that binary for A/B; verified by one EXR per sample in `<dir>`
- [x] 1.2 Record the DPEL teapot's import and render phases (`--stats`) and `subdivision_memory_probe` numbers at the current commit; verified by the numbers noted for task 6.2

## 2. Refinement level plumbing

- [x] 2.1 `UsdImportOptions::subdivision_level: Option<u32>`, `crust:subdivisionLevel` read off the settings prim, resolved once in `load_scene` (override → settings → 2, clamp 6 with `WARN`, `CRUST_SUBDIV=0` → 0) and recorded in `RenderSettings`; verified by unit tests for each precedence step and the clamp
- [x] 2.2 Pass the resolved level into `mesh_source` on the single-stage, streamed and prototype paths; remove `attrs::subdiv_level`; verified by `cargo check` and a streamed-import test refining a scheme-authored mesh
- [x] 2.3 `--subdiv-level <n>` in `crust-render/src/main.rs`, forwarded through `UsdImportOptions`; verified by an args test beside the existing `-s` one and by `--help` listing it

## 3. Trigger on the authored scheme

- [x] 3.1 In `mesh_source`, refine only when `subdivisionScheme` has an authored value (`resolve_info().has_authored_value()`) other than `none`; unauthored → cage; verified by tests: unauthored cage, `none` cage, `catmullClark` / `bilinear` refined, non-triangle `loop` warns and stays a cage
- [x] 3.2 A mesh-prim `crust:subdivisionLevel` emits one `WARN` per load (flag in `ImportCaches`) and is otherwise ignored; verified by a test on a stage with two such prims counting exactly one warning

## 4. Refined UV chart

- [x] 4.1 `subdiv.rs`: an optional UV channel in `SubdivRequest` (face-varying values plus value indices, identity when `:indices` is unauthored), refined with `interpolate_face_varying` and `limit_face_varying`, returned as refined values plus per-face-vertex indices; invalid indices refuse the channel; verified by unit tests on a single quad (`all` → exact bilinear chart) and a two-quad UV seam (values stay on their own side)
- [x] 4.2 Map `faceVaryingLinearInterpolation` one to one onto `sdc::FVarLinearInterpolation`, with unauthored → `cornersPlus1`; verified by a test per token
- [x] 4.3 `vertex` / `varying` UVs are refined and limited like the points; verified by a unit test comparing them with a hand-refined quad
- [x] 4.4 Ptex channel invariance: refine the synthetic Ptex channel under all six modes and compare it bitwise with `All`; if any mode differs, give Ptex its own `All` refiner; verified by the test and the existing `ptex_quads` / subdivided-Ptex tests passing
- [x] 4.5 `mesh_source` keeps `uvs` for refined meshes (a `UvSource` over the refined chart) and drops the "texture coordinates are not refined" warning; verified by an inline-USD test binding a UV-textured material to a subdivided mesh and checking that the triangles carry UVs

## 5. Samples, scripts and tests

- [x] 5.1 Re-author `samples/subdivision.usda` by scheme: no scheme (cage), `none`, `bilinear`, `catmullClark`, creased `catmullClark` cube, UV-textured `catmullClark`; remove per-prim levels; doc string points at `--subdiv-level`; verified by a render at `--subdiv-level 0/1/3`
- [x] 5.2 Update `crust-core/tests/usd_inline.rs` and `usd_scene.rs` subdivision tests to authored schemes plus the import option; `scripts/gen_subdiv_stress.py` authors the scheme and the settings level; verified by `cargo test --workspace`
- [x] 5.3 Goldens: `check_images.sh check` against 1.1 shows every sample except `subdivision.usda` bit-identical; verified by the script's report

## 6. Measurement and records

- [x] 6.1 DPEL teapot at the default level renders smooth and textured with no refinement warning; verified by the render and by `mtlx_shade`-style numbers at a refined point matching the cage chart's UV there
- [x] 6.2 `subdivision_memory_probe` gains the UV-channel case and its ceiling; teapot `--stats` import time and memory measured against 1.2; verified by the probe passing and the numbers recorded in the design record
- [x] 6.3 Update `openspec/specs/usd-scene-import/design.md` (subdivision section: trigger, level, UV channel, D4 outcome, measurements), `textures/design.md` (drop the UV-on-subdivided-meshes gap), `cli/design.md` (`--subdiv-level`), the `CRUST_SUBDIV` row and README subdivision text; verified by `grep -rn "crust:subdivisionLevel"` finding only the settings-prim meaning and the legacy warning
- [x] 6.4 `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast` clean; `openspec validate usd-driven-subdivision --strict` passes; verified by their exit codes

## 7. Revision: follow the USD fallback scheme

- [x] 7.1 Probe ALab and Kitchen_set meshes through openusd (grep cannot read crate ≥ 0.4 token tables); verified: ALab's render meshes (foam hand, beakers, Kipp's apparatus, flask stand) and Kitchen_set's author neither a scheme nor normals, ALab's display proxies author `none` plus face-varying normals
- [x] 7.2 `mesh_source` decides on the resolved scheme (unauthored → `catmullClark`); level 0 renders the cage with `subdiv::smooth_cage_normals`; `CRUST_SUBDIV=0` stays the faceted cage (`SubdivPolicy::enabled`); verified by `an_unauthored_scheme_is_catmull_clark` and `level_zero_shades_the_cage_smooth`
- [x] 7.3 Author `subdivisionScheme = "none"` on every polygon mesh of `samples/*.usda` (63 meshes) and of the test stages; `subdivision.usda`'s first cube demonstrates the fallback; verified by `cargo test -p crust-core` and `check_images.sh check` against 1.1: every sample identical except `subdivision`, `materialx_teapot`, `materialx_showcase` and `cornellbox_guided` (the last differs from its golden with the *base* binary on the *unmodified* file too: pre-existing nondeterminism, not this change)
- [x] 7.4 ALab frame 1004 renders the foam hand and glassware smooth at `--subdiv-level 0` and at the default level; `--stats` memory recorded in the design record
- [x] 7.5 Specs, design records and docs describe the fallback rule; `cargo fmt`, `clippy -D warnings`, `cargo test --workspace` and `openspec validate --strict` clean

## 8. Revision: conservative default level

- [x] 8.1 `DEFAULT_SUBDIV_LEVEL = 0`: nothing is refined unless the stage's `crust:subdivisionLevel` or `--subdiv-level` asks; subdivision surfaces still render as smooth-shaded cages, `none` stays faceted; verified by `by_default_nothing_is_refined` and the level-driven tests moved to an explicit level (`load_at_level`)
- [x] 8.2 The DPEL teapot wrappers (`materialx_teapot.usda`, `materialx_showcase.usda`) set `crust:subdivisionLevel = 1` on their RenderSettings so they keep rendering refined and textured
- [x] 8.3 Specs, proposal, design, design records, CLI help and docs state the default of 0
- [x] 8.4 `cargo fmt`, `clippy -D warnings`, `cargo test --workspace`, `check_images.sh check` and `openspec validate --strict` clean

## 9. Review (Qodo, PR #178)

- [x] 9.1 `MeshKey` carries whether the mesh has smooth normals, so a level-0 subdivision cage and an identical `none` cage no longer share a slot; verified by `a_smooth_cage_and_a_faceted_cage_do_not_share_a_mesh` (fails without the fix, both authoring orders)
- [x] 9.2 A `loop` mesh whose material reads Ptex renders its smooth cage with a warning instead of refining into triangles read as cage face ids; verified by `a_loop_mesh_with_ptex_keeps_its_cage_face_ids` (8 triangles without the fix)
- [x] 9.3 `cornersPlus2` warns once per load that opensubdiv-rs omits its concave-corner sharpening; recorded as a known gap in the spec and the design record
- [x] 9.4 `subdivision_memory_probe` gets a resident ceiling per mode (no UVs 60, Ptex 105 as on `main`, chart 90 B/face); the 88.1 B/face Ptex resident is measured identical on `main`, so it predates this change (the 84 figure was stale)
- [x] 9.5 `cargo fmt`, `clippy -D warnings`, `cargo test --workspace`, `openspec validate --strict` clean
