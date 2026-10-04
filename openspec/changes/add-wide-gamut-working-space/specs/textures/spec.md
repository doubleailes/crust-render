## MODIFIED Requirements

### Requirement: Colour space travels with the texture

Every texture request SHALL carry the colour space the binding declares and the
working space the render is in, and lookups SHALL return linear values in the
working space. A declared space SHALL be converted with its transfer curve and
its change of primaries, as the OCIO config defines them. For MaterialX an
absent `colorspace` (on the input, its node, its nodegraph and the document)
SHALL mean the values are already in the working space, and a non-colour image
SHALL never be converted; for `UsdUVTexture` a `colorSpace` metadatum on
`inputs:file` SHALL win over `sourceColorSpace`, and an absent
`sourceColorSpace` means `auto` (8-bit RGB / RGBA is sRGB, anything else
unconverted). A conversion the renderer cannot split into a per-channel curve
and a matrix SHALL be refused with a warning and the values used as stored.

#### Scenario: A greyscale roughness PNG under UsdUVTexture

- **WHEN** a single-channel 8-bit PNG is bound with no `sourceColorSpace`
- **THEN** it is read as raw data, with no transfer curve applied

#### Scenario: An sRGB albedo rendered in ACEScg

- **WHEN** an 8-bit texture tagged `srgb_texture` is bound in a render whose
  working space is ACEScg
- **THEN** a lookup returns the sRGB-decoded colour converted from Rec.709 to
  AP1 primaries, and a streamed `.tx` of it returns bit-identical values
