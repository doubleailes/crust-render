## 1. Materials (`crates/crust-core/src/material/`)

- [ ] 1.1 Add `Material::straight_transmittance(ray, rec) -> Vec3A` (default
      zero) and `Material::has_straight_transmission()` (default false). Verify:
      `cargo test -p crust-core` passes unchanged.
- [ ] 1.2 Native OpenPBR: return the thin-walled window transmission (tint ×
      `(1 − R_window)` × transmission weight × the throughput of the layers
      above) and add a scatter variant without the thin delta lobe (design D2).
      Verify with a unit test: `straight_transmittance` plus the albedo of the
      reduced lobe set equals the full lobe set's albedo, per direction, to 1e-5.
- [ ] 1.3 MaterialX closure tree: `T` is the summed tree weight of the
      thin-walled delta-T leaves, and a meet drops those leaves (design D2).
      Verify with the same albedo-split test on `open_pbr_surface` with
      `geometry_thin_walled = true` and on a `standard_surface` thin-walled
      glass.
- [ ] 1.4 `Material::resolve` returns the same `T` and reduced lobes. Verify:
      `crust-core/tests/resolve.rs` gains the thin-walled cases and passes.

## 2. Integrator (`crates/crust-core/src/tracer/path.rs`)

- [ ] 2.1 Add `World::has_straight_transmission()`. Make `surface_visibility` /
      `cutout_through` return RGB `Π P(ω)` (design D1), keeping the scalar fast
      path when neither flag is set. Verify: cutout tests pass unchanged.
- [ ] 2.2 Extend `pass_cutouts` with the coloured pass: probability
      `q = max_c P_c`, throughput `P / q` on a pass, `1 / (1 − q)` on a meet
      with the reduced lobe set, and a new key `K_THIN` beside `K_CUTOUT`. Verify
      with a unit test on a clear tinted sheet: the mean pass throughput equals
      `T`.
- [ ] 2.3 Route a thin-wall pass as a `TS` event (design D3). Verify: the LPE
      test pinning `C.*[LO]` to the beauty passes on a scene with a window.
- [ ] 2.4 Learned light cache training uses the luminance of the RGB visibility
      (design D4). Verify: `--light-selection learned` tests pass.

## 3. Equivalence and cost

- [ ] 3.1 Count instructions with callgrind on `samples/cornellbox.usda` and on
      the zero-AOV render. Verify: unchanged, since neither has thin-walled
      transmission.
- [ ] 3.2 Prove the expectation is unchanged: render a sphere light behind a
      tinted thin-walled sheet over a diffuse floor, before and after, at
      16/64/256/1024 spp with `--indirect-clamp 0`. Verify: the difference falls
      as 1/√N with no plateau, light-only now agrees with power-MIS and BSDF-only
      (it was black), and record the relMSE drop at equal time in the design
      record.
- [ ] 3.3 Run `scripts/check_images.sh check` against a `main` recording. Verify:
      only samples with thin-walled transmissive materials change, and each
      changes by noise alone (per 3.2's test). Re-record them.

## 4. Documentation

- [ ] 4.1 Update `openspec/specs/rendering/design.md` (cutouts and pass-throughs)
      and `openspec/specs/materials/design.md` (thin-walled transmission) with
      the rule, the Typhoon reference and the measurement. Add the thick-glass
      straight shadow as a known gap. Update `docs/architecture.md` § Invariants
      (the pass-through pair) and the `CLAUDE.md` pairs list if its wording
      names cutouts alone.
- [ ] 4.2 Run the CI set: fmt, clippy `-D warnings`, `cargo test --workspace`.
