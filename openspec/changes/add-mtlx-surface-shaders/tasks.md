# Tasks

## 1. Nodedef tables in crust-mtlx

- [ ] 1.1 Vendor `ND_standard_surface_surfaceshader`, `ND_open_pbr_surface_surfaceshader` (1.1) and `ND_gltf_pbr_surfaceshader` from MaterialX 1.39 as test fixtures under `crates/crust-mtlx/tests/nodedefs/`, keeping their Apache-2.0 header. Verify the files parse with `Doc::open`.
- [ ] 1.2 Add a per-model input table to `crust-mtlx` (name, type, default; `gltf_pbr`'s `attenuation_distance` as `+∞`), plus the per-model "unrepresentable" input set from the spec. Verify with a test that each table matches its vendored nodedef exactly: same input names, types and defaults, nothing extra or missing.

## 2. Compile surface-shader inputs (crust-mtlx)

- [ ] 2.1 In `bsdf::flatten`, recognise the three surface categories and record a `SurfaceShader { model, inputs, authored }` on `Compiled` in place of lobes. Compile each input through the existing input path (connection → value → table default), and stop listing these categories in `unsupported`. Verify with `tests/graph.rs` cases: a value input, a connected input, and an unauthored input reading its default.
- [ ] 2.2 Extend `Compiled::roots()` / `optimize()` to remap surface-shader slots. Verify in `tests/optimize.rs` that an optimised surface-shader program yields bit-identical slot values to the unoptimised one at several shading points, and that unconnected inputs become constants.
- [ ] 2.3 Compute the "authored away from default" list against the unrepresentable set: connected, or a value differing from the default. Verify with tests that `opacity = (0.3,0.3,0.3)` is listed and an explicit `alpha = 1` / `alpha_mode = 0` is not.
- [ ] 2.4 Warn on a non-default nodedef `version` and fall back to the default table. Verify with a test document carrying `version="1.0"` on `open_pbr_surface`.

## 3. Map to OpenPBR (crust-core)

- [ ] 3.1 Add the surface-shader mode to `MtlxMaterial` (`load()` picks it when `Compiled` carries a `SurfaceShader`; `run` evaluates the program and maps rather than calling `reduce()`), and implement `map_open_pbr` one to one with the authored `coat_darkening`. Verify with unit tests: the spec's "open_pbr_surface is one to one" scenario, and an all-defaults document equal to `OpenPBR::default()` field for field.
- [ ] 3.2 Implement `map_standard_surface` node for node after `standard_surface_to_open_pbr.mtlx`, each step commented with the graph's node name. Verify with hand-derived unit tests: the spec's metal `specular_weight`, coat-tint and default scenarios, the thin-film nm→µm step with its weight switch, `fuzz_roughness = sheen_roughness^0.4`, and the coated-metal `coat_weight = 0` case.
- [ ] 3.3 Implement `map_gltf_pbr` per design D5 (roughness unchanged; transmission tint versus attenuation, with non-finite distance meaning none; thin-walled at `thickness == 0`; metal `specular_color` mix; sheen weight/colour split; clearcoat; iridescence nm→µm; anisotropy ratio; emission; dispersion). Verify with unit tests for the spec's volume and iridescence scenarios, and one test per D5 trap.
- [ ] 3.4 Feed the surface's `normal` / `geometry_normal` slot through `run`'s existing shading-normal rule, and skip it when unconnected. Verify with a test that a connected `normalmap` changes the probed normal and an unconnected one leaves `rec.normal` untouched.
- [ ] 3.5 Implement `may_transmit` and the transmissive `make_ray` of design D8. Verify that a `transmission = 1` material's refracted ray carries an interior medium, that a literal-zero one returns the plain `Ray::new` without running the graph (a counter or profile section in a test), and that `crust-core/tests/medium.rs`-style absorption grows with depth for the glTF attenuation scenario.
- [ ] 3.6 Log the unrepresentable-input list as one `WARN` per material in the importer, beside the unsupported-node warning. Verify with a `tests/usd_scene.rs`-style log capture on the `opacity` scenario and silence on the explicit-default one.

## 4. Fixture, pinned pairs and probe

- [ ] 4.1 Add `samples/materialx_surfaces.mtlx` / `.usda`: one constant material per model, a `standard_surface` glass, a `gltf_pbr` with volume attenuation, and a normal-mapped `open_pbr_surface`. Verify it renders with no unsupported-node warnings, and that `cargo run --release -p crust-render --example mtlx_shade -- samples/materialx_surfaces.mtlx` prints the expected parameters for each.
- [ ] 4.2 Add the fixture's materials to `crust-core/tests/resolve.rs`. Verify `cargo test -p crust-core --test resolve` passes, with `resolve` matching per-query shading for each.
- [ ] 4.3 Add a surface-shader program to `crust-jit/tests/jit.rs`. Verify it is bit-identical to the interpreter.
- [ ] 4.4 Verify the unchanged-render scenario: record `scripts/check_images.sh record` goldens before the change for `materialx_basic.usda` and `materialx_emissive.usda` (16 spp, `--indirect-clamp 0`), then `check` after; it exits zero.

## 5. Performance

- [ ] 5.1 Verify that `scripts/bench_ab.sh` of the pre-change and post-change binaries on `materialx_basic.usda` (and the DPEL teapot, if available) shows no change beyond noise, reporting min and mean.
- [ ] 5.2 Record a callgrind instruction count for `samples/materialx_surfaces.usda` at `-s 2` (per `CLAUDE.md`), and verify the mapping itself stays a small fraction of `RunShader`. Record the numbers in the materials `design.md`.

## 6. Documentation

- [ ] 6.1 Update `openspec/specs/materials/design.md` § MaterialX with the surface-shader path (D1–D8, including the gltf mapping table and the translation graph's approximations). In § Known gaps: MaterialX, replace the "no surface-shader nodes" entry with the remaining gaps (opacity, rotation, tangents, coat normal, glTF approximations, colour-space inheritance). Verify with a review that every spec approximation appears in Known gaps.
- [ ] 6.2 Add the colour-space note of the design's Risks to `docs/color_management.md`, and verify it names the surface inputs' inherited handling.

## 7. Integration: the Material Fidelity suite

- [ ] 7.1 Re-run `scripts/material_fidelity/run.py` on the full suite and regenerate the tables in `docs/material_fidelity.md` with `summarize.py`. Verify no `standard_surface` / `open_pbr_surface` / `gltf_pbr` appears in the logged-unsupported list, and that no `surfaces/*` or `showcase/*` group's mean PSNR falls more than 3 dB below `blender-new`. Investigate any group that does as a mapping bug before archiving.
- [ ] 7.2 Run the CI trio (`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`) on the pinned toolchain, and verify all three pass.
