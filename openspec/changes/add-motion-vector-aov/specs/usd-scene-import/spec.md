## ADDED Requirements

### Requirement: Render settings can disable motion blur

`disableMotionBlur = true` SHALL trace every camera ray at shutter open, so
the beauty and every AOV show moving geometry sharp at its authored position.
`instantaneousShutter = true` SHALL mean the same. Motion stays in the scene,
so motion vectors are still produced. The value is resolved like the
render's camera: the first product's value, else the settings prim's.
Neither attribute SHALL be warned about as not honoured.

#### Scenario: Sharp beauty with motion authored

- **WHEN** a sphere authors `crust:motion:translate = (1, 0, 0)` and the
  render settings author `disableMotionBlur = true`
- **THEN** the sphere renders sharp at its authored position, with no streak
  along X, and no warning about `disableMotionBlur` is logged

#### Scenario: instantaneousShutter is a synonym

- **WHEN** the render settings author `instantaneousShutter = true` and not
  `disableMotionBlur`
- **THEN** the render is the same as with `disableMotionBlur = true`

#### Scenario: Product overrides the settings

- **WHEN** the settings prim authors `disableMotionBlur = true` and the first
  product authors `disableMotionBlur = false`
- **THEN** moving geometry is motion blurred

#### Scenario: Blur stays on by default

- **WHEN** neither attribute is authored
- **THEN** a prim authoring `crust:motion:translate` is motion blurred, as
  before this change
