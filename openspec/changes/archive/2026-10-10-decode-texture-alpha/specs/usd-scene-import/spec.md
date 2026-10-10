## MODIFIED Requirements

### Requirement: Imported opacity inputs are cutouts

`crust:openpbr`'s `geometryOpacity` and `PxrDisneyBsdf`'s `alpha` SHALL be
the material's opacity, a cutout the integrator honours (see the rendering
capability). A `UsdPreviewSurface` whose `opacityThreshold` is above 0 SHALL be
a cutout mask: a point is present where `opacity ≥ opacityThreshold` and absent
otherwise, from a constant or a texture-driven `opacity` alike, and SHALL NOT
refract. A texture-driven `opacity` connected to a `UsdUVTexture`'s `a` output
SHALL read the file's alpha (see the textures capability). Under an
`opacityThreshold` of 0 (the default) `opacity` below 1 SHALL remain
translucency (transmission at `ior`), not a cutout.

#### Scenario: A constant preview cutout

- **WHEN** a `UsdPreviewSurface` authors `opacity = 0` and `opacityThreshold = 0.5`
- **THEN** its material is a cutout of opacity 0, and its BSDF transmits nothing

#### Scenario: A textured preview mask

- **WHEN** a `UsdPreviewSurface` under `opacityThreshold = 0.5` reads `opacity`
  from a texture whose value at a hit is 0.2, and at another 0.8
- **THEN** the first hit has opacity 0 and the second opacity 1

#### Scenario: A cutout from a texture's alpha

- **WHEN** a `UsdPreviewSurface` under `opacityThreshold = 0.5` reads `opacity`
  from the `a` output of a `UsdUVTexture` whose PNG has alpha 0 on its left half
  and 255 on its right
- **THEN** a hit on the left half has opacity 0 and one on the right half
  opacity 1, preloaded and streamed alike, and the import raises no warning
  about the material

#### Scenario: Translucency is no cutout

- **WHEN** a `UsdPreviewSurface` authors `opacity = 0` with no threshold
- **THEN** its material has no cutout and refracts at `ior`
