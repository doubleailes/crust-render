# Tasks

## 1. Closure-tree IR in crust-mtlx

- [x] 1.1 Replace `Flattened { lobes, emission }` / `LobeKind` with `Closures { tree, emission }`: an arena of `Leaf` / `Layer` / `Mix` / `Add` / `Multiply` nodes, with MaterialX-named leaves and per-leaf `normal` / `tangent` slots (design D2). Rewrite `bsdf::flatten` to build it for standalone-BSDF graphs, keeping compile-time pruning of literal-zero branches. Verify `crust-mtlx/tests/graph.rs` cases: a mix of two dielectrics keeps two leaves with their own roughness, a layer keeps its top/base structure, and pruned branches are absent.
- [x] 1.2 Extend `Compiled::roots()` / `optimize()` to the tree's slots. Verify in `tests/optimize.rs` that an optimised tree's slot values are bit-identical to the unoptimised one at several shading points.
- [x] 1.3 Record a volume term beside the tree (`anisotropic_vdf` and a surface's transmission medium inputs). Verify with a test that a `vdf`-bearing `surface` compiles its absorption / scattering / anisotropy slots.

## 2. Directional-albedo tables (ported)

- [x] 2.1 Port BSDL's `DielectricReflFront` filter (BSDL `genluts.cpp`, BSD-3-Clause, as vendored in Typhoon @ `70c45e8`; regenerated output matches Typhoon's table bit for bit) to `crust-core/src/material/closure/bsdl_tables.rs` with a checked-in conversion script and BSDL's axis conventions. The transmission-albedo and coupled-compensation tables were not ported: MaterialX's layer throughput reads the reflection albedo alone, and its GLSL applies no compensation to transmission, so they would have no consumer. Verify with spot-value tests at grid points and against known values.
- [x] 2.2 Port MaterialX GLSL's `mx_ggx_dir_albedo_analytic`, `mx_ggx_energy_compensation` and `mx_imageworks_sheen_dir_albedo` fits (Apache-2.0). Verify with tests reproducing reference values evaluated from the GLSL formulas.
- [x] 2.3 Add `THIRD-PARTY.md` entries for BSDL and MaterialX. Verify each vendored file names its source, commit and licence.
- [ ] 2.4 Measure each table's `E` against crust's own integrated leaf albedo over the grid (a test-only Monte Carlo integrator), and record max / mean mismatch in `openspec/specs/materials/design.md`. Verify the recorded numbers come from the committed test.

## 3. Leaf evaluators and the resolved closure (crust-core)

- [x] 3.1 Implement leaf eval / sample / pdf over `brdf.rs` (design D5): `Dielectric` R/T/RT with thin film and thin-walled window, `Conductor` with a new complex-IOR Fresnel, `GeneralizedSchlick` with `color82`, EON and plain Oren–Nayar, Burley, Charlie `Sheen`, diffuse-like `Subsurface`, Lambertian `Translucent`, and GGX multiple-scattering compensation on microfacet leaves. Verify with a per-leaf white-furnace test and a per-leaf sample-histogram versus pdf test.
- [x] 3.2 Implement `ResolvedClosure`: one tree walk collapsing to an inline fixed-capacity `(rgb_weight, leaf, frame)` list with `layer = f_top + f_base · T_top(ωo)`, the mixture pdf with floored `pᵢ ∝ lum(wᵢ)·Êᵢ(ωo)`, and one-sample MIS sampling (design D3). Verify with the spec's "coat dims what lies beneath" and "sampling agrees with evaluation" scenarios as unit tests, plus refusal of a tree above capacity.
- [x] 3.3 Build per-leaf frames from normal / tangent slots under the existing normal rule. Verify with unit tests that a perturbed coat normal leaves the base leaves on the interpolated normal, and that an authored tangent rotates the anisotropic highlight.
- [x] 3.4 Map the recorded volume term to crust's `Medium` as the MaterialX volume graph does (design D8), and attach it in the closure's `make_ray` below a thick surface. Verify that a refracted ray carries the medium, that a thin-walled one does not, and that absorption grows with depth for the spec's glTF attenuation scenario.
- [x] 3.5 Add `Resolved::Closure` to `ShadingPoint` and a closure constructor on `Resolution` that reads emission before resolving (design D7). Make `MtlxMaterial` resolve through it, remove `reduce()` and the pooled path, and adapt `examples/light_occlusion.rs`. Verify `cargo test -p crust-core --test resolve` passes for every MaterialX fixture material and `cargo build --examples` succeeds.

## 4. Surface-shader builders (crust-mtlx)

- [x] 4.1 Vendor `ND_open_pbr_surface_surfaceshader` (1.1), `ND_standard_surface_surfaceshader` and `ND_gltf_pbr_surfaceshader` as test fixtures under `crates/crust-mtlx/tests/nodedefs/` (Apache-2.0 header kept), and add the per-model default tables. Verify with a test that each table matches its nodedef exactly.
- [x] 4.2 Add any program operator the builders need (e.g. select / compare, `copysign`) to the interpreter and `crust-jit` together. Verify each with a JIT ↔ interpreter bit-identity test in `crust-jit/tests/jit.rs`. *(None was needed: `ifgreater` is built as `mix(in2, in1, max(sign(v2 − v1), 0))` from existing ops. `jit.rs`'s file check now covers `samples/materialx_surfaces.mtlx`, so every builder's program is pinned JIT ↔ interpreter.)*
- [x] 4.3 Implement the `open_pbr_surface` builder node for node after `NG_open_pbr_surface_surfaceshader`, including the thin-walled subsurface branch and derived parameters as program ops. Verify with the spec's `open_pbr_surface` scenarios (metal honours `specular_weight`, coat broadens the base highlight, unauthored defaults) as probe-level unit tests.
- [x] 4.4 Implement the `standard_surface` builder after `NG_standard_surface_surfaceshader_100` (artistic-IOR conductor, coat attenuation with `coat_affect_color`). Verify with the spec's artistic-IOR scenario and a constant-material scenario.
- [x] 4.5 Implement the `gltf_pbr` builder after `IMPL_gltf_pbr_surfaceshader` (clearcoat, sheen, iridescence, attenuation medium). Verify with the spec's clearcoat and attenuation scenarios.
- [x] 4.6 Compute the static unrepresentable-input and live-approximated-closure lists and log them as one `WARN` per material in the importer, beside the unsupported-node warning; warn on a non-default nodedef `version`. Verify with log-capture tests for the spec's opacity, default-silent and fuzz scenarios. *(Verified on `Loaded::reported`, the list the one `WARN` line joins, in `tests/mtlx_surfaces.rs`, rather than by capturing the log.)*

## 5. Probe, fixtures and existing documents

- [x] 5.1 Rewrite `examples/mtlx_shade` to print the resolved leaf list (kind, RGB weight, roughness / IOR / colours, normal), emission and medium at a `(u, v)` and ωo (design D10). Verify it runs on `samples/materialx_basic.mtlx` and prints one line per live leaf.
- [x] 5.2 Add `samples/materialx_surfaces.mtlx` / `.usda`: one material per surface model, a `standard_surface` glass, a `gltf_pbr` with attenuation, and an `open_pbr_surface` with a coat normal. Verify it renders with no unsupported-node warnings and that the probe matches each spec scenario.
- [x] 5.3 Rewrite the MaterialX unit tests in `material/materialx.rs` and the MaterialX assertions in `tests/usd_scene.rs` for tree semantics, keeping the invariants that still hold: EDF sums, per-channel emission weight, no clamping above 1, and a pure-EDF surface not reflecting. Verify `cargo test -p crust-core` passes.
- [ ] 5.4 Run the furnace scenario (uniform white dome, `--indirect-clamp 0`) on every fixture material. Verify no non-emissive pixel exceeds the environment.
- [ ] 5.5 Re-verify `materialx_basic`, `materialx_emissive` and the DPEL Teapot / Lion (when available) by probe numbers against the values recorded in the materials `design.md`. Then re-record the affected `check_images.sh` goldens at 16 spp. Verify the recorded numbers still hold, or that each change is explained in the design record.

## 6. Performance

- [ ] 6.1 Verify that `scripts/bench_ab.sh` of the pre- and post-change binaries on `materialx_basic.usda` and `samples/materialx_surfaces.usda` (plus the DPEL assets, if present) runs, and report min and mean.
- [ ] 6.2 Record callgrind instruction counts at `-s 2` for the same scenes, split into `RunShader` versus the closure resolve / eval. Verify the numbers and the `ShadingPoint` size are recorded in the materials `design.md`.

## 7. Documentation

- [ ] 7.1 Rewrite `openspec/specs/materials/design.md` § MaterialX for the closure tree (D1–D10). Retire the pooled-reduction traps, since the record describes current behaviour, and rewrite § Known gaps: MaterialX (subsurface, Zeltner, opacity, rotation, table mismatch, `crust:openpbr` divergence, Typhoon deviations followed or not). Verify with a review that every spec approximation appears in Known gaps.
- [ ] 7.2 Update the README's MaterialX section and `docs/color_management.md` (inherited colour-space handling of surface inputs). Verify the README no longer describes the BSDF reduction as pooling onto OpenPBR.

## 8. Integration: the Material Fidelity suite

- [ ] 8.1 Re-run `scripts/material_fidelity/run.py` on the full suite and regenerate `docs/material_fidelity.md` with `summarize.py`. Verify that no surface node appears in the logged-unsupported list, and that no `surfaces/*` or `showcase/*` group's mean PSNR falls more than 3 dB below `blender-new`. Investigate any that does as a probable builder or table bug before archiving.
- [ ] 8.2 Run the CI trio (`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`) and the pinned-nightly clippy leg. Verify all pass.
