# Spec Delta

## ADDED Requirements

### Requirement: The camera's aperture is conformed to the image's aspect

The importer SHALL read `aspectRatioConformPolicy` from the first `RenderProduct`,
else from the `RenderSettings` prim, as it resolves `resolution`; unauthored it SHALL
be `expandAperture`. Before the field of view is derived, the render camera's
apertures SHALL be conformed to the image's aspect as OpenUSD's
`CameraUtilConformWindow` does, with square pixels. An unauthored `verticalAperture`
SHALL still follow the image's aspect, so it needs no conform.

#### Scenario: expandAperture keeps a narrow aperture's height

- **WHEN** the camera authors `horizontalAperture = 36`, `verticalAperture = 24`, and
  the settings author `resolution = (200, 100)` and no policy
- **THEN** the render frames a 48 × 24 aperture: the vertical field of view is the
  authored one, and the image is identical to the render before this change

#### Scenario: expandAperture keeps a wide aperture's width

- **WHEN** the camera authors `horizontalAperture = 36`, `verticalAperture = 24`, and
  the settings author `resolution = (100, 100)` and no policy
- **THEN** the render frames a 36 × 36 aperture: the whole authored width is in frame

#### Scenario: adjustApertureHeight keeps the width

- **WHEN** the camera authors `horizontalAperture = 36`, `verticalAperture = 24`, and
  the settings author `resolution = (200, 100)` and
  `aspectRatioConformPolicy = "adjustApertureHeight"`
- **THEN** the render frames a 36 × 18 aperture, and the image differs from the one
  rendered without the policy

#### Scenario: cropAperture keeps the smaller extent

- **WHEN** the camera authors `horizontalAperture = 36`, `verticalAperture = 24`, and
  the settings author `resolution = (100, 100)` and
  `aspectRatioConformPolicy = "cropAperture"`
- **THEN** the render frames a 24 × 24 aperture

#### Scenario: adjustApertureWidth keeps the height

- **WHEN** the camera authors `horizontalAperture = 36`, `verticalAperture = 24`, and
  the settings author `resolution = (100, 100)` and
  `aspectRatioConformPolicy = "adjustApertureWidth"`
- **THEN** the render frames a 24 × 24 aperture

#### Scenario: A product's policy overrides the settings'

- **WHEN** the settings author `aspectRatioConformPolicy = "adjustApertureWidth"` and
  the first product authors `aspectRatioConformPolicy = "adjustApertureHeight"`
- **THEN** the product's policy frames the render

#### Scenario: An aperture that matches the image is unchanged

- **WHEN** the camera's authored aperture has the image's aspect, or authors no
  `verticalAperture`
- **THEN** every policy renders the same image as before this change

#### Scenario: The policy on the camera is not read

- **WHEN** the camera prim, not the settings or a product, authors
  `aspectRatioConformPolicy`
- **THEN** it has no effect, as in OpenUSD, and the settings' policy (or
  `expandAperture`) frames the render

### Requirement: Everything that reads the camera's framing reads the conformed one

Adaptive subdivision, on-screen implicit-shape sizing and the MCP session's
in-place settings edits SHALL read the same conformed apertures as the render
camera, so a policy that reframes the image also re-sizes what was tessellated for
the old frame.

#### Scenario: Adaptive subdivision follows the policy

- **WHEN** a stage with adaptive subdivision changes only its
  `aspectRatioConformPolicy` so that the conformed vertical aperture changes
- **THEN** the tessellation is computed for the conformed field of view, and an MCP
  edit making that change re-imports rather than applying in place

#### Scenario: An in-place policy edit matches a re-import

- **WHEN** an MCP edit changes `aspectRatioConformPolicy` on a stage whose geometry
  does not read the camera
- **THEN** the edit applies in place, and its render is bit-identical to a full
  import of the same layer

### Requirement: adjustPixelAspectRatio falls back to expandAperture

Crust SHALL render square pixels only. A resolved policy of
`adjustPixelAspectRatio` whose aperture aspect differs from the image's SHALL be
conformed as `expandAperture`, with one `product.unhonoured_attribute` warning naming
the pixel aspect ratio it would have needed.

#### Scenario: A pixel-aspect policy on a mismatched aperture

- **WHEN** the camera authors a 36 × 24 aperture, and the settings author
  `resolution = (200, 100)` and
  `aspectRatioConformPolicy = "adjustPixelAspectRatio"`
- **THEN** one `product.unhonoured_attribute` warning names a pixel aspect ratio of
  0.75, and the render is the `expandAperture` one

#### Scenario: A pixel-aspect policy on a matching aperture

- **WHEN** the policy is `adjustPixelAspectRatio` and the aperture already has the
  image's aspect
- **THEN** no warning is raised and the framing is unchanged

### Requirement: An unknown conform policy is refused

An authored `aspectRatioConformPolicy` that is not one of the five schema tokens
SHALL be refused with one `product.invalid_conform_policy` warning naming the prim
and the token, and read as `expandAperture`.

#### Scenario: A misspelt policy

- **WHEN** the settings author `aspectRatioConformPolicy = "adjustApertureHieght"`
- **THEN** one `product.invalid_conform_policy` warning is raised, and the render is
  framed as `expandAperture`

### Requirement: Products that differ in conform policy share the first's framing

Crust SHALL render one framing per render. A product after the first whose
resolved `aspectRatioConformPolicy` differs from the first product's SHALL still be
written, framed by the first's policy, with one `product.framing_mismatch` warning.

#### Scenario: A second product with another policy

- **WHEN** two products share the camera and resolution, the first resolves
  `expandAperture` and the second `adjustApertureHeight`
- **THEN** both files are written with the `expandAperture` framing, and one
  `product.framing_mismatch` warning names the second product
