## 1. Mapping (`crates/crust-core/src/environment.rs`)

- [x] 1.1 Change `direction_to_uv` to `u = ½ − atan2(x, z) / 2π` (wrapped) and
      `uv_to_direction` to `φ = 2π(½ − u)`, `d = (sin θ sin φ, cos θ, sin θ cos φ)`
      (design D2). Rewrite the module header's convention section, quoting the
      UsdLux/OpenEXR rule. Verify: the round-trip unit test passes unchanged.
- [x] 1.2 Replace `conventions_are_as_documented` with assertions of the spec:
      +Z at u = ½, +X at u = ¼, −X at u = ¾, −Z on the seam, +Y on row 0.
      Verify: `cargo test -p crust-core environment`.
- [x] 1.3 Replace `minus_z_is_the_image_centre_and_plus_z_the_seam` in
      `crates/crust-core/tests/environment.rs` with
      `plus_z_is_the_image_centre_and_plus_x_a_quarter_in`, on the same 2×1
      quad map. Verify: the test fails on the old mapping and passes on the new.
- [x] 1.4 Add a sampling consistency test: for a map with one bright texel, the
      directions `sample` returns land in that texel, and `pdf` of each returned
      direction equals the pdf `sample` reported. Verify:
      `cargo test -p crust-core environment`.

## 2. Cross-check against the reference

- [x] 2.1 Add a test that evaluates Typhoon's formula (design D2) at a grid of
      directions and asserts that crust's `direction_to_uv` matches it to 1e-6.
      Verify: `cargo test -p crust-core environment`.

## 3. Images and documentation

- [x] 3.1 Re-record the golden images (`scripts/check_images.sh record`) for the
      16 textured-dome samples listed in the proposal. Check that every other
      sample is bit-identical (`scripts/check_images.sh check` against a `main`
      recording). Verify: only those 16 differ.
- [x] 3.2 Review the samples that author a dome rotation (`grep -n "DomeLight" -A12
      samples/*.usda | grep rotate`) and confirm the new orientation is the
      intended look, or adjust the authored rotation and say why in the sample.
- [x] 3.3 Update the "Infinite lights" section of
      `openspec/specs/lighting/design.md` with the convention, the spec quote, the
      Typhoon reference and the OpenPBR Shader Playground example. Add the
      orientation sentence to `site/content/docs/usd/lights.md` (the `DomeLight`
      row). Verify: `zola build` in `site/` passes.
- [x] 3.4 Run the CI set: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`.
