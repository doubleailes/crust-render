## 1. Sphere precision (`crates/crust-rt/src/prim.rs`)

- [x] 1.1 Rewrite `SpherePrim::hit` with the closest-approach discriminant and the stable
      root pair (design D1). Verify: a kernel test compares reported distances with an
      f64 reference for radius 0.05 at 8 units and radius 0.01 at 2 units, and every
      error is below `1e-6 · t`.
- [x] 1.1b Rewrite `CylinderPrim::hit`'s wall test the same way (design D1, as revised).
      Verify: the same kernel test on a cylinder of radius 0.05 at 8 units.
- [x] 1.2 Add the restart test: each ray restarted at `t − 0.001 + 1e-5·t` finds the far
      side, never the near side. Verify: it fails on the old formula (about 1 in 3 rays
      re-hit) and passes on the new one.
- [x] 1.3 Verify the kernel's bit-identity pins still hold:
      `scripts/test_simd_matrix.sh -p crust-rt` and the `Tri4` ↔ scalar tests.

## 2. One crossing per surface (`crates/crust-core/src/tracer/path.rs`)

- [x] 2.1 Track the last accepted crossing `(geom_id, prim, side, t)` in `pass_cutouts`
      and `pass_walls`, and skip a same-primitive, same-side hit within `1e-4·t` of it
      (design D2). Verify with a unit test that feeds the walk a duplicated entry: the
      hidden light's emission is collected once.
- [x] 2.2 Apply the same rule in `cutout_through` and the shadow-side thin-wall walk.
      Verify: the "two cards close together" test (`(1 − 0.5)²`) and
      `a_surface_just_behind_a_cutout_is_not_skipped` both pass.

## 3. Equivalence

- [x] 3.1 Add the repro as an integration test (`tests/hidden_lights.rs`): a diffuse plane
      under one hidden sphere light at distance/radius 40, 160 and 200. Verify:
      BSDF-only and light-only agree within 3σ at the test's sample count (they differed
      by 4%, 34% and 64%).
- [x] 3.2 Re-render veach_mis (portable) with LightTiny alone. Verify: plate0 is within
      noise of the pre-hidden-light binary (it was 16.6% brighter), and the four-light
      render still equals the sum of the single-light renders. *(Plate0 band, rows
      230–250, mean R under LightTiny alone at 144 spp: pre-hidden-light `b00c86a`
      5.6355, branch head 6.5572 (+16.4%), this change 5.6327 (−0.05%); plate1 3.8313
      / 4.0004 / 3.8324. Four-light ÷ sum of singles on the four plate bands:
      0.9974–1.0002 at 144 spp, 0.9996–1.0000 at 576 spp — tightening as noise.)*
- [x] 3.3 Run `scripts/check_images.sh check` against a recording from the branch head
      before this change. Verify: samples without spheres or pass-throughs are
      bit-identical. For three samples that move, the difference falls as 1/√N across
      spp. *(19 of 38 bit-identical; the 19 that move all have a sphere or cylinder,
      except `materialx_showcase` (32 px at 1e-6: triangle re-hits of its hidden rect
      lights, caught by the guard). The difference is not pure noise — it carries the
      corrected sphere positions — so it does not fall as 1/√N: `light_linking`
      relMSE 5.4e-10 / 6.5e-10 / 5.4e-10 at 16 / 64 / 256 spp (flat, max abs 2e-4);
      `openpbr_showcase` 2.09e-6 / 1.57e-6 / 1.03e-6 onto a 3.1e-7 trimmed floor (the
      bubble's transmittance no longer squared); `materialx_cutout` 2.5e-5 / 5.1e-6 /
      2.8e-6. Renders are deterministic run to run (0 px). A kernel-fix-only binary
      renders `light_linking` bit-identically to the full change.)*
- [x] 3.4 Count instructions with callgrind (`RAYON_NUM_THREADS=1`, `-s 2`) on
      `samples/cornellbox.usda` and a sphere-heavy sample. Record the cost of the sphere
      test change in the `intersection-kernel` design record. *(cornellbox 4 236.88 M →
      4 236.77 M, −0.003 %; openpbr_showcase 2 361.70 M → 2 369.56 M, +0.33 %:
      `scalar_hit` +16.2 M, `pass_walls` −18.9 M.)*

## 4. Documentation

- [x] 4.1 Record the precision finding, the closest-approach form and the measurement in
      `openspec/specs/intersection-kernel/design.md`. Record the one-crossing rule, and
      why `resume_before`'s margin alone was not enough, in
      `openspec/specs/rendering/design.md`, with the nested-instance shared-id exposure
      as a known gap. Add the pass-through pair to `docs/architecture.md` § Invariants
      if its wording names one walk only.
- [x] 4.2 Run the CI set: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`.
