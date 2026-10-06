## 1. Sphere precision (`crates/crust-rt/src/prim.rs`)

- [ ] 1.1 Rewrite `SpherePrim::hit` with the closest-approach discriminant and the stable
      root pair (design D1). Verify: a kernel test compares reported distances with an
      f64 reference for radius 0.05 at 8 units and radius 0.01 at 2 units, and every
      error is below `1e-6 · t`.
- [ ] 1.2 Add the restart test: each ray restarted at `t − 0.001 + 1e-5·t` finds the far
      side, never the near side. Verify: it fails on the old formula (about 1 in 3 rays
      re-hit) and passes on the new one.
- [ ] 1.3 Verify the kernel's bit-identity pins still hold:
      `scripts/test_simd_matrix.sh -p crust-rt` and the `Tri4` ↔ scalar tests.

## 2. One crossing per surface (`crates/crust-core/src/tracer/path.rs`)

- [ ] 2.1 Track the last accepted crossing `(geom_id, prim, side, t)` in `pass_cutouts`
      and `pass_walls`, and skip a same-primitive, same-side hit within `1e-3·t` of it
      (design D2). Verify with a unit test that feeds the walk a duplicated entry: the
      hidden light's emission is collected once.
- [ ] 2.2 Apply the same rule in `cutout_through` and the shadow-side thin-wall walk.
      Verify: the "two cards close together" test (`(1 − 0.5)²`) and
      `a_surface_just_behind_a_cutout_is_not_skipped` both pass.

## 3. Equivalence

- [ ] 3.1 Add the repro as an integration test (`tests/hidden_lights.rs`): a diffuse plane
      under one hidden sphere light at distance/radius 40, 160 and 200. Verify:
      BSDF-only and light-only agree within 3σ at the test's sample count (they differed
      by 4%, 34% and 64%).
- [ ] 3.2 Re-render veach_mis (portable) with LightTiny alone. Verify: plate0 is within
      noise of the pre-hidden-light binary (it was 16.6% brighter), and the four-light
      render still equals the sum of the single-light renders.
- [ ] 3.3 Run `scripts/check_images.sh check` against a recording from the branch head
      before this change. Verify: samples without spheres or pass-throughs are
      bit-identical. For three samples that move, the difference falls as 1/√N across
      spp.
- [ ] 3.4 Count instructions with callgrind (`RAYON_NUM_THREADS=1`, `-s 2`) on
      `samples/cornellbox.usda` and a sphere-heavy sample. Record the cost of the sphere
      test change in the `intersection-kernel` design record.

## 4. Documentation

- [ ] 4.1 Record the precision finding, the closest-approach form and the measurement in
      `openspec/specs/intersection-kernel/design.md`. Record the one-crossing rule, and
      why `resume_before`'s margin alone was not enough, in
      `openspec/specs/rendering/design.md`. Add the pass-through pair to
      `docs/architecture.md` § Invariants if its wording names one walk only.
- [ ] 4.2 Run the CI set: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`.
