# Spec Delta

## MODIFIED Requirements

### Requirement: Listing metadata as JSON

`crust ls <kind> --json PATH|-` SHALL write the `crust-ls/1` report: the
listed prims in the text listing's order, each with its `path` and the values
a render reads for it. Values SHALL be read as the render reads them, not by
reimplementing the import. An unauthored value SHALL be reported as the value
the render falls back to.

#### Scenario: Camera records

- **WHEN** the user runs `crust ls camera -i samples/cornellbox.usda --json -`
- **THEN** each record has `focal_length_mm`, `aperture_mm` (horizontal,
  vertical), `conformed_aperture_mm` (horizontal, vertical), `f_stop`,
  `focus_distance`, `is_render_camera` and `hidden`

#### Scenario: The conformed aperture

- **WHEN** a camera authors a 36 × 24 aperture, and the settings author
  `resolution = (200, 100)` and
  `aspectRatioConformPolicy = "adjustApertureHeight"`
- **THEN** its record has `aperture_mm = [36, 24]` and
  `conformed_aperture_mm = [36, 18]`, the aperture the render frames

#### Scenario: The camera a render would use

- **WHEN** the first RenderProduct names `/cams/shot`, `RenderSettings.camera`
  names `/cams/layout`, and both are on the stage
- **THEN** only `/cams/shot` has `is_render_camera = true`, as
  `crust render` without `--camera` renders through it

#### Scenario: A settings camera that is missing

- **WHEN** `RenderSettings.camera` names a prim that is not a camera
- **THEN** the camera the render falls back to is the one marked
  `is_render_camera`

#### Scenario: Light records

- **WHEN** the user lists lights as JSON
- **THEN** each record has `type`, `intensity`, `exposure`, `color` and
  `normalize`, as authored on the light (no transform applied)

#### Scenario: Material records

- **WHEN** the user lists materials as JSON
- **THEN** each record has `surface`, the authored surface shader's
  identifier (`null` when the material has none), and `bound`, whether the
  render's binding resolution resolves at least one geometry prim to it

#### Scenario: A binding that never takes effect

- **WHEN** a mesh's own `material:binding` targets `/mtl/a`, and a parent
  binds `/mtl/b` with `bindMaterialAs = strongerThanDescendants`
- **THEN** `/mtl/b` has `bound = true` and `/mtl/a` has `bound = false`,
  unless another prim resolves to it
