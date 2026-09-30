## 1. Regression test

- [ ] 1.1 Add a test to `crates/crust-core/tests/render_smoke.rs`: an empty
      world with no lights, adaptive sampling on (threshold > 0, minimum below
      the budget). Assert `early_stopped == 0`, `spp_min == spp_max == spp`
      and an all-black buffer.
- [ ] 1.2 Run it on the current code and confirm it fails, because every
      pixel stops at the minimum.

## 2. Fix

- [ ] 2.1 In `render_pixel` (`crates/crust-core/src/tracer/mod.rs`), add
      `lum_sq > 0.0` to the early-stop condition, with a comment explaining
      why an all-zero history is not convergence and why the gate is on
      `lum_sq` rather than `lum_sum`.
- [ ] 2.2 Confirm the new test passes and
      `adaptive_sampling_takes_fewer_camera_rays_on_a_flat_image` still
      passes.

## 3. Verify

- [ ] 3.1 `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`.
- [ ] 3.2 Re-run the ALab probe (adaptive, minimum 8, 256 spp, frame 1004)
      and confirm that exact-zero pixels drop from about 3.6k to a few
      dozen, at similar render time.

## 4. Docs

- [ ] 4.1 In `openspec/specs/rendering/design.md`, under adaptive sampling,
      record the trap (all-zero history reads as converged), the ALab
      numbers and the cost to black regions.
