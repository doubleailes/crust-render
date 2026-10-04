## ADDED Requirements

### Requirement: Working colour space

The importer SHALL render in the scene-linear colour space named by the
`RenderSettings` prim's `renderingColorSpace`, resolved through the OCIO config,
unless the host names one, which SHALL win; with neither, `lin_rec709`. A
stage-authored space that is unknown or not scene-linear SHALL be refused with
a warning and `lin_rec709` used; a host-named one SHALL be an error. An
authored colour SHALL be converted into the working space from the space its
`colorSpace` metadatum names, and taken as already in it when it names none.

#### Scenario: An ACEScg stage

- **WHEN** `renderingColorSpace = "acescg"` and a light's `inputs:color`
  carries `colorSpace = "lin_rec709"`
- **THEN** the scene's working space is ACEScg and the light's colour is the
  Rec.709 value converted to AP1, while a light colour with no metadata is
  used as authored
