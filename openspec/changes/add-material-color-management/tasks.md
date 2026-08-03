## 1. Shared color-space primitive

- [ ] 1.1 Add `crates/crust-core/src/color.rs` with `ColorSpace { Linear,
      Srgb, Gamma(f32) }` and `ColorSpace::decode(self, Vec3A) -> Vec3A`,
      documenting on each variant why it exists (piecewise sRGB EOTF for
      DCC-authored content vs. flat gamma matching the island's
      `PxrColorCorrect`/`HwPtexTexture_1` authoring).
- [ ] 1.2 Re-export `ColorSpace` from `crust-core/src/lib.rs`.
- [ ] 1.3 Unit test: `ColorSpace::Srgb` and `ColorSpace::Gamma(2.2)` produce
      numerically distinct output at a representative input (e.g. 0.5), so
      an accidental future unification of the two curves fails a test.

## 2. Material importer enforcement (`usd_import.rs`)

- [ ] 2.1 Add a `RawColor3(Vec3A)` newtype with `RawColor3::decode(self,
      space: ColorSpace) -> Vec3A`; change `shader_input_vec3` to return
      `RawColor3` instead of a bare `Vec3A`.
- [ ] 2.2 Update `preview_surface_openpbr`: decode `diffuseColor` and
      `emissiveColor` with `ColorSpace::Srgb` (the bug fix). Leave scalar
      fields (`metallic`, `roughness`, `opacity`, `ior`, `clearcoat`,
      `clearcoatRoughness`) untouched.
- [ ] 2.3 Update `disney_to_openpbr`: decode `baseColor` with
      `ColorSpace::Gamma(2.2)` via the shared function; delete the
      function's private `srgb_to_linear` helper it used to call.
- [ ] 2.4 Update `decode_crust_openpbr`: decode every color field
      (`baseColor`, `specularColor`, `transmissionColor`, `subsurfaceColor`,
      `fuzzColor`, `coatColor`, `emissionColor`) explicitly with
      `ColorSpace::Linear`, making the "no conversion" decision explicit at
      each call site.

## 3. Light and volume importer enforcement (`usd_import.rs`)

- [ ] 3.1 Change `attr_color3f` and `custom_color3` to return `RawColor3`
      (same newtype as task 2.1), so light and volume color reads go through
      the same enforcement as material color reads.
- [ ] 3.2 Update `lux_emission` (feeding `emit_distant_light`,
      `emit_sphere_light`, `emit_rect_light`, and the dome-light path) to
      decode `inputs:color` with `ColorSpace::Linear`, with a doc comment
      citing `UsdLuxLightAPI`'s "in the rendering color space" wording.
- [ ] 3.3 Update the volume-region attribute reads (`crust:volume:sigmaS`,
      `sigmaA`, `emission`) to decode with `ColorSpace::Linear`, with a doc
      comment noting these are project-custom, linear-authored coefficients
      like `crust:openpbr`.

## 4. Ptex texel decode (`crust-render/src/main.rs`)

- [ ] 4.1 Update `PtexColor::open`'s texel decode to call
      `ColorSpace::Gamma(2.2).decode(...)` from `crust-core` instead of its
      own inline `powf(2.2)`.

## 5. Regression coverage

- [ ] 5.1 Add an integration test (or extend an existing USD-import test in
      `crust-core/tests/usd_scene.rs`) asserting a `UsdPreviewSurface`
      `diffuseColor` of (0.5, 0.5, 0.5) yields a linear `base_color` of
      ≈0.214 per channel, not 0.5.
- [ ] 5.2 Add an integration test asserting a `PxrDisneyBsdf` `baseColor` of
      (0.5, 0.5, 0.5) yields a linear `base_color` of 0.5^2.2 ≈ 0.218 per
      channel (pinning today's already-correct behavior against
      regression).
- [ ] 5.3 Add an integration test asserting a `crust:openpbr` `baseColor`
      passes through unconverted.
- [ ] 5.4 Add an integration test asserting a lux light's `inputs:color` and
      a volume region's `sigmaS`/`sigmaA`/`emission` each pass through
      `ColorSpace::Linear` unchanged.

## 6. Golden-image reconciliation

- [ ] 6.1 Identify which checked-in sample scenes (`samples/*.usda`) bind
      `UsdPreviewSurface` materials with non-trivial colors.
- [ ] 6.2 Re-render those scenes, visually confirm the new (sRGB-decoded)
      output is correct, then re-record their golden images via
      `scripts/check_images.sh record`.
- [ ] 6.3 Run `scripts/check_images.sh check` across the full sample set
      (including light- and volume-driven scenes) to confirm no scene
      outside step 6.1's list changed — the light/volume enforcement in
      task group 3 is `ColorSpace::Linear` (identity), so it must produce
      zero pixel difference anywhere.

## 7. Validation

- [ ] 7.1 `cargo test` passes, including the new unit and integration
      tests.
- [ ] 7.2 `cargo build --verbose && cargo test --verbose` (the CI command)
      passes clean.
