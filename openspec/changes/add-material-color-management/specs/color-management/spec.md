## Purpose

Define how the renderer interprets authored color values from every
color-valued USD input it reads — material shader attributes, Ptex textures,
light colors, and volume coefficients — so that every color reaches its use
(shading, lighting, or volume transport) already converted to linear light,
while scalar (non-color) inputs are never subjected to that conversion.

## ADDED Requirements

### Requirement: Material color inputs are decoded from a known source color space

Every color-valued material input (e.g. `baseColor`, `diffuseColor`,
`emissiveColor`, Ptex-sourced per-face colors) SHALL be converted from its
source's documented color space to linear before it is used in shading. The
conversion applied depends on the input's source, not a single global
default:

- `UsdPreviewSurface` color inputs SHALL be decoded from sRGB (piecewise
  EOTF) to linear.
- `PxrDisneyBsdf`'s `baseColor` and Ptex color textures SHALL be decoded with
  a flat gamma-2.2 power curve to linear (matching the specific authoring
  convention of content that uses these sources), not the piecewise sRGB
  EOTF.
- `crust:openpbr` native shader color inputs SHALL be treated as already
  linear, with no conversion applied.

#### Scenario: UsdPreviewSurface diffuseColor is converted from sRGB

- **WHEN** a `UsdPreviewSurface` shader authors `diffuseColor = (0.5, 0.5,
  0.5)`
- **THEN** the resulting material's linear base color is the sRGB-decoded
  value (≈0.214 per channel), not 0.5

#### Scenario: PxrDisneyBsdf baseColor is converted with flat gamma 2.2

- **WHEN** a `PxrDisneyBsdf` shader authors `baseColor = (0.5, 0.5, 0.5)`
- **THEN** the resulting material's linear base color is the flat
  gamma-2.2-decoded value (0.5^2.2 ≈ 0.218 per channel)

#### Scenario: crust:openpbr color inputs pass through unconverted

- **WHEN** a `crust:openpbr` shader authors `baseColor = (0.5, 0.5, 0.5)`
- **THEN** the resulting material's linear base color is exactly (0.5, 0.5,
  0.5), with no color-space conversion applied

#### Scenario: Ptex color texture is converted with flat gamma 2.2

- **WHEN** a material's per-face color comes from a Ptex texture
- **THEN** each texel is decoded with the same flat gamma-2.2 curve used for
  `PxrDisneyBsdf.baseColor`, before being used as the surface's base color

### Requirement: Light and volume color inputs are treated as already linear

Color-valued inputs that are not material shader inputs — USD light colors
(`UsdLuxDistantLight`, `UsdLuxDomeLight`, `UsdLuxSphereLight`,
`UsdLuxRectLight` `inputs:color`) and volume region coefficients
(`crust:volume:sigmaS`, `crust:volume:sigmaA`, `crust:volume:emission`) —
SHALL be treated as already linear, with no color-space conversion applied,
and that treatment SHALL be an explicit decision rather than an omission.

#### Scenario: Light color passes through unconverted

- **WHEN** a `UsdLuxDistantLight`, `UsdLuxDomeLight`, `UsdLuxSphereLight`, or
  `UsdLuxRectLight` prim authors `inputs:color = (0.5, 0.5, 0.5)`
- **THEN** the light's linear color is exactly (0.5, 0.5, 0.5), with no
  color-space conversion applied

#### Scenario: Volume coefficient color passes through unconverted

- **WHEN** a volume region prim authors `crust:volume:sigmaS`,
  `crust:volume:sigmaA`, or `crust:volume:emission` as `(0.5, 0.5, 0.5)`
- **THEN** the region's linear coefficient is exactly (0.5, 0.5, 0.5), with
  no color-space conversion applied

### Requirement: Scalar material inputs are never color-space converted

Scalar (non-color) material inputs — including but not limited to
roughness, metalness, IOR, weight, and anisotropy parameters — SHALL be used
at their authored numeric value with no color-space conversion applied,
regardless of which shader source they come from.

#### Scenario: Roughness is used unconverted

- **WHEN** any supported shader source authors a roughness-like scalar
  input
- **THEN** the value used in shading equals the authored value, with no
  gamma or sRGB decoding applied
