## Why

Material color inputs (`baseColor`, `emissiveColor`, Ptex-sourced colors, etc.)
and scalar inputs (`roughness`, `ior`, `metallic`, etc.) are read through four
independent, ad-hoc code paths today, and only two of them decode sRGB→linear:
`PxrDisneyBsdf.baseColor` and Ptex color textures each apply a flat `powf(2.2)`
gamma decode; `UsdPreviewSurface.diffuseColor`/`emissiveColor` are read raw with
**no** decode at all, which is a correctness bug (`UsdPreviewSurface` colors are
conventionally authored in a display-referred space); `crust:openpbr` colors are
also read raw, which is currently intentional (crust's own lossless native
mirror is documented as linear-authored) but that assumption is implicit and
unenforced. There is no shared `ColorSpace` concept, so nothing stops a future
shader-mapping function from decoding a scalar as if it were a color, or a
color as if it were a scalar — the `OpenPBR` struct represents both as bare
`Vec3A`/`f32` with no distinguishing type. Fixing the `UsdPreviewSurface` gap
and preventing this class of bug needs one principled place to decide, per
input, whether and how it is color-space-converted before reaching `OpenPBR`.

The same gap exists beyond materials: every mathematical operation in the
integrator is done in linear light, so every color-valued input has to be
linearized before use, not just the three material shaders above. USD light
colors (`UsdLuxDistantLight`/`DomeLight`/`SphereLight`/`RectLight`
`inputs:color`, read through a shared `lux_emission` helper) and volume
region coefficients (`crust:volume:sigmaS`/`sigmaA`/`emission`, read through
`custom_color3`) are read exactly as raw today, with no color-space decision
made at all — the same class of implicit gap, just not yet inventoried
against the same mechanism.

## What Changes

- Introduce a `ColorSpace` concept (e.g. `Linear`, `Srgb` piecewise EOTF,
  `Gamma(2.2)` flat power curve) and route every material-input decode in the
  USD importer through it, replacing today's two separate inline
  implementations (`usd_import.rs`'s `srgb_to_linear` and `main.rs`'s Ptex
  `powf(2.2)`) with one shared conversion.
- **Fix**: `UsdPreviewSurface`'s `diffuseColor` and `emissiveColor` are decoded
  sRGB→linear like every other portable color input, instead of read raw.
  **BREAKING**: any scene currently relying on `UsdPreviewSurface` colors being
  passed through unconverted will render differently (darker, correctly).
- Enforce the color/scalar distinction at the **importer boundary**, not on the
  `OpenPBR` struct: the three attribute-reading primitives
  (`shader_input_vec3`, `attr_color3f`, `custom_color3`) return a `RawColor3`
  newtype whose only accessor is `RawColor3::decode(space: ColorSpace) -> Vec3A`.
  Every color-valued call site must therefore name a color space — including the
  ones whose answer is `Linear` — so a conversion can no longer be omitted from a
  color input by accident. Which inputs count as colors follows the OpenPBR
  parameter reference's own `color3` vs `float` typing
  (https://academysoftwarefoundation.github.io/OpenPBR/#parameterreference).
  `OpenPBR`'s field types are **unchanged**: scalars are already `f32` and so
  cannot reach a `Vec3A` decode, meaning only the "color never decoded at all"
  direction needs new enforcement.
- Document, per material-input source, which color space is assumed and why:
  `UsdPreviewSurface` → sRGB (fixed by this change); `PxrDisneyBsdf.baseColor`
  → flat gamma 2.2 (preserved — matches a specific island `PxrColorCorrect`
  node, not a general default); `crust:openpbr` → linear, no conversion
  (preserved, native lossless format); Ptex color textures → flat gamma 2.2
  (preserved, same island-specific reasoning as Disney).
- No change to environment-map decoding (`main.rs`'s LDR piecewise sRGB EOTF /
  HDR-and-EXR-as-linear) — already correct per format, out of scope here
  except as a candidate consumer of the new shared `ColorSpace` conversion.
- Extend the same explicit-decision mechanism to light colors and volume
  coefficients: `UsdLuxLightAPI:inputs:color` is documented upstream as being
  "in the rendering color space" — i.e. already linear for a linear-light
  path tracer — and `crust:volume:*` coefficients are project-custom
  attributes with the same linear-authored convention as `crust:openpbr`. In
  both cases the correct conversion is `ColorSpace::Linear` (identity): this
  is **not** a numeric/behavioral change like the `UsdPreviewSurface` fix, it
  makes "linear, no conversion" an explicit, enforced call site instead of an
  accident nobody has had reason to touch.

## Capabilities

### New Capabilities

- `color-management`: defines the `ColorSpace` conversions available to the
  importer and texture loaders, and which inputs are color-valued versus scalar,
  so a conversion is never applied to a scalar or omitted from a color.

### Modified Capabilities

- `usd-scene-import`: the "Material resolution by shader id" requirement gains
  explicit, per-shader-source color-space decoding behavior, including the
  `UsdPreviewSurface` bug fix described above. The "Light schema mapping" and
  "Volume region import" requirements each gain an explicit statement that
  their color-valued inputs are treated as linear (`ColorSpace::Linear`,
  identity) rather than left undecided.

## Impact

- `crates/crust-core/src/scene/usd_import.rs`: `preview_surface_openpbr`,
  `disney_to_openpbr`, `decode_crust_openpbr`, and the existing `srgb_to_linear`
  helper.
- `crates/crust-core/src/material/openpbr.rs`: **not** affected — the `OpenPBR`
  struct's field types stay as they are. Enforcement lives at the importer
  boundary instead, keeping this change out of the `materials` capability and
  away from every BRDF read site.
- `crates/crust-render/src/main.rs`: `PtexColor::open`'s texel decode, as a
  candidate to route through the shared conversion instead of its own inline
  `powf(2.2)`.
- `crates/crust-core/src/scene/usd_import.rs`: `lux_emission` (feeding
  `emit_distant_light`/`emit_sphere_light`/`emit_rect_light`/the dome-light
  path) and the volume-region attribute reads (`custom_color3` for
  `sigmaS`/`sigmaA`/`emission`).
- Golden-image regression scenes/tests that use `UsdPreviewSurface` colors will
  need their reference images regenerated (`scripts/check_images.sh record`)
  since output changes for that path. Light and volume scenes need **no**
  golden-image changes: their values are already effectively linear, so
  making that explicit changes no rendered pixel.
- No render-throughput impact expected: color-space conversion happens once at
  import/load time, never per-sample in the integrator.
