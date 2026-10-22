# Tasks

## 1. Tables from upstream OSL

- [ ] 1.1 Pin the OSL commit (candidate `cc6f9b1`) in `scripts/tables/bsdl_luts_to_rust.py`, make it fetch `src/libbsdl` from upstream instead of Typhoon's copy, and verify that regenerating `DielectricReflFront` reproduces `closure/bsdl_tables.rs` bit for bit, or record the explained diff in `materials/design.md`
- [ ] 1.2 Extend the script to emit `DielectricBothFront`, `DielectricBothBack`, `MiniMicrofacetGGX`, `ContyKullaSheen` and `ZeltnerBurleySheen`, and verify each generated table's dimensions and a few spot values against the `genluts` headers
- [ ] 1.3 Compare `ZeltnerBurleySheen` with `material/ltc_sheen_table.rs` (design open question 1); keep one table if they are identical, else add BSDL's, and record the answer in `materials/design.md`
- [ ] 1.4 Update `THIRD-PARTY.md`'s BSDL entry (upstream source, pinned commit, the new tables) and verify `cargo deny --locked check` and `cargo build` pass

## 2. BSDL oracle

- [ ] 2.1 Write `scripts/bsdl_oracle/probe.cpp` and `scripts/bsdl_oracle.py` (modelled on `adobe_oracle`): build against the pinned `libbsdl` and Imath, and emit value toward fixed `wi`, 32 × 32 directional albedo, `filter_o` and `albedo_impl` per case. Verify the script runs end to end and writes `crates/crust-core/tests/data/bsdl_oracle.txt` with the pin in its header
- [ ] 2.2 Choose cases covering every leaf and mode in the spec's "Every leaf is replayed" scenario (dielectric R/T/RT front and back, IOR 1.001–5, roughness 0–1 incl. anisotropic; conductor; Schlick R/RT; both sheens; ON with and without EC; Burley; translucent; tinted cases). Verify that the fixture contains each one
- [ ] 2.3 Write `crates/crust-core/tests/bsdl_oracle.rs` with the `Deviation` mechanism from `adobe_oracle.rs` (stale rules fail) and temporary per-leaf "genglsl leaf" rules bounded by today's measured error. Verify `cargo test -p crust-core --test bsdl_oracle` passes and fails if any rule is removed
- [ ] 2.4 Document the oracle (regeneration recipe, toolchain, pin) in `materials/design.md` and add its commands to CLAUDE.md's command list beside `adobe_oracle`

## 3. Layer opacity

- [ ] 3.1 Change `walk` in `closure/mod.rs` to return testrender's linear opacity (design decision 1): leaf `own·(1 − filter)` or `own`, mix blend, add sum, multiply scale, layer base weight `1 − clamp(O_top)` and `O = O_top + O_base`. Verify the existing closure tests that encode GLSL throughput fail where expected, then update them to the spec's scenarios
- [ ] 3.2 Add the testrender `evaluate_layer_opacity` transcription and fixed trees to `closure/tests.rs` (design decision 5), including a weighted opaque top, a multiplied top, a refracting top, a nested layer in a top and opacities summing above 1. Verify the test passes
- [ ] 3.3 Keep the DPEL "transmission dummy" pin (pruned against connected-zero dummy, bit for bit) passing unchanged, and verify that `cargo test -p crust-core closure` is green
- [ ] 3.4 Verify that the shipped surfaces are unaffected by the rule alone: `scripts/check_images.sh check` on scenes using only `open_pbr_surface` / `standard_surface` / `gltf_pbr` reports no change after 3.1 (leaves still genglsl)
- [ ] 3.5 Rewrite the "trap" paragraph of `openspec/specs/materials/design.md` (§ closure tree) for the OSL opacity rule and its sum over nested layers, and update `site/content/docs/usd/materials.md`; verify `zola build` in `site/` passes

## 4. Leaves ported from BSDL

- [ ] 4.1 Add `closure/bsdl.rs` with `GGXDist` (BSDL's roughness/anisotropy parameterisation), `TabulatedEnergyCurve`, `DielectricFresnel`, `ConductorFresnel` and `eval_turquin_microms_reflection`, with unit tests against hand-computed values
- [ ] 4.2 Port `conductor_bsdf` (Turquin with the GGX table) and switch `prepare` to it; delete its "genglsl leaf" rule and verify the conductor oracle cases pass
- [ ] 4.3 Port `dielectric_bsdf` R-only (Turquin with ReflFront, filter `E_ms`) and verify the oracle cases pass
- [ ] 4.4 Port refracting `dielectric_bsdf` (T and RT, both faces, BothFront/BothBack renormalisation, tints per hemisphere) keeping crust's VNDF sampler and a matching pdf; verify the oracle cases, the sampling-vs-eval histogram test and the spec's "rough clear dielectric conserves energy" scenario
- [ ] 4.5 Port `generalized_schlick_bsdf` (R and RT, dielectric-table compensation) and verify its oracle cases
- [ ] 4.6 Port `sheen_bsdf` in `conty_kulla` and `zeltner` modes and verify their oracle cases
- [ ] 4.7 Port `oren_nayar_diffuse_bsdf` (with and without energy compensation), `burley_diffuse_bsdf` and `translucent_bsdf`, and verify their oracle cases
- [ ] 4.8 Feed leaf selection from the new albedos (design decision 6) and verify `tests/resolve.rs`, the MIS / sampling tests and the white furnace in `tests/mtlx_surfaces.rs` pass
- [ ] 4.9 Keep the Airy thin film on the new dielectric and conductor leaves, add its oracle deviation rule with measured bound, and verify the spec's "A thin film still interferes" scenario
- [ ] 4.10 Delete the genglsl lobes and fits from `mx.rs` (keep thin film and hair), verify that no "genglsl leaf" deviation rule remains and that `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] 4.11 Update `materials/design.md` § Leaves and § throughput tables (sources, parameterisation, measured gaps) and its known gaps (thin film, sampling, path regularisation, subsurface and hair references)

## 5. Measurement and goldens

- [ ] 5.1 Measure callgrind instructions (`-s 2`) on `cornellbox` (must be unchanged), `materialx_basic` and a `standard_surface` scene, and `scripts/bench_ab.sh -n 7` old vs new binary; record min and mean in `materials/design.md`
- [ ] 5.2 Re-record MaterialX goldens with `scripts/check_images.sh record` at `-s 16 --indirect-clamp 0`, and verify that non-MaterialX scenes are bit-identical
- [ ] 5.3 Re-run the Material Fidelity suite scored against `materialx-osl` and update `docs/material_fidelity.md` with before/after per group; verify that the mean PSNR against `materialx-osl` does not fall for any non-subsurface group, or record why
- [ ] 5.4 Run the full CI set locally (`cargo fmt --all -- --check`, clippy, `cargo test --workspace --no-fail-fast`, `cargo deny --locked check`) and verify it is green

## Workflow follow-up

- Sync the delta into `openspec/specs/materials/spec.md` and archive the change after review.
