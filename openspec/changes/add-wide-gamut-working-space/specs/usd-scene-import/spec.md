## ADDED Requirements

### Requirement: Working colour space

The importer SHALL render in the scene-linear colour space named by the
`RenderSettings` prim's `renderingColorSpace`, resolved through the OCIO config,
unless the host names one, which SHALL win; with neither, `lin_rec709`. A
stage-authored space that is unknown or not scene-linear SHALL be refused with
a warning and `lin_rec709` used; a host-named one SHALL be an error. An
authored colour SHALL be converted into the working space from the
colour space `UsdColorSpaceAPI` resolves for it — its `colorSpace` metadatum,
else the `colorSpace:name` of its prim or nearest authoring ancestor — and
taken as already in it when none is authored.

#### Scenario: An ACEScg stage

- **WHEN** `renderingColorSpace = "acescg"` and a light's `inputs:color`
  carries `colorSpace = "lin_rec709"`
- **THEN** the scene's working space is ACEScg and the light's colour is the
  Rec.709 value converted to AP1, while a light colour with no metadata is
  used as authored

#### Scenario: An inherited colour space

- **WHEN** a scope authors `colorSpace:name = "lin_rec709_scene"` and a light
  two prims below it authors an untagged `inputs:color`, in an ACEScg render
- **THEN** the light's colour is converted from Rec.709 to AP1

### Requirement: Heuristics weigh colours by the working space's luminance

Every sampling heuristic that reduces a colour to one weight — light power,
lobe selection, environment importance, guiding training, adaptive sampling —
and the `variance` AOV SHALL use the luminance weights of the working space,
the `Y` row of its RGB → XYZ matrix. In linear Rec.709 they SHALL be
(0.2126, 0.7152, 0.0722), and the render bit-identical to before.

#### Scenario: An ACEScg stage

- **WHEN** a stage renders in ACEScg
- **THEN** its lights and materials weigh colours by ACEScg's luminance
