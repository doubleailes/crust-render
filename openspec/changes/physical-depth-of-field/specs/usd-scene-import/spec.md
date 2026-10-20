## ADDED Requirements

### Requirement: Camera depth of field

The render camera's lens SHALL have diameter `focalLength / fStop × 0.001 /
metersPerUnit` scene units: `focalLength` read in millimetres, `metersPerUnit`
the stage's. It SHALL focus at `focusDistance` scene units. Fallbacks:
`focalLength` 50, `focusDistance` 10, `metersPerUnit` 0.01. An `fStop` of 0 or
less, or unauthored, SHALL be a pinhole. The field of view SHALL not depend on
`fStop`, `focusDistance` or `metersPerUnit`.

#### Scenario: A centimetre stage

- **WHEN** a stage authors `metersPerUnit = 0.01` and the render camera
  authors `focalLength = 50`, `fStop = 2` and `focusDistance = 500`
- **THEN** the lens diameter is 2.5 scene units (25 mm), and rays from every
  lens sample through the frame centre meet 500 units down the view axis

#### Scenario: A metre stage

- **WHEN** a stage authors `metersPerUnit = 1` and the render camera authors
  `focalLength = 50` and `fStop = 2`
- **THEN** the lens diameter is 0.025 scene units (25 mm)

#### Scenario: metersPerUnit unauthored

- **WHEN** a stage does not author `metersPerUnit` and the render camera
  authors `focalLength = 50` and `fStop = 2`
- **THEN** the lens diameter is 2.5 scene units, as on a centimetre stage

#### Scenario: No fStop is a pinhole

- **WHEN** the render camera authors no `fStop`, or `fStop = 0`
- **THEN** every camera ray through a pixel starts at the camera's position,
  whatever the lens sample

#### Scenario: The field of view does not change

- **WHEN** the same camera is rendered on a stage with `metersPerUnit = 0.01`
  and on one with `metersPerUnit = 1`
- **THEN** the ray through each pixel's centre from the lens centre has the
  same direction on both

### Requirement: An unusable metersPerUnit is refused

A stage `metersPerUnit` that is not finite or not positive SHALL be refused with
one `stage.invalid_meters_per_unit` warning naming the value, and SHALL read as
0.01. The check SHALL apply only when the render camera has a lens, so a
pinhole render raises no warning.

#### Scenario: A zero metersPerUnit

- **WHEN** a stage authors `metersPerUnit = 0` and the render camera authors
  `focalLength = 50` and `fStop = 2`
- **THEN** one `stage.invalid_meters_per_unit` warning is raised and the lens
  diameter is 2.5 scene units

#### Scenario: A pinhole does not check

- **WHEN** a stage authors `metersPerUnit = 0` and the render camera authors
  no `fStop`
- **THEN** no `stage.invalid_meters_per_unit` warning is raised

### Requirement: Render settings can disable depth of field

`disableDepthOfField = true` SHALL render through a pinhole at the camera's
position, whatever its `fStop`. The field of view and the focus plane SHALL not
change. The value SHALL be resolved like `disableMotionBlur`: the first
product's authored value, else the settings prim's, default `false`. It SHALL
not be warned about as not honoured. An MCP edit of it SHALL take effect as a
full re-import would.

#### Scenario: Sharp render with a lens authored

- **WHEN** the render camera authors `fStop = 2` and the render settings author
  `disableDepthOfField = true`
- **THEN** every camera ray starts at the camera's position, and no
  `product.unhonoured_attribute` warning names `disableDepthOfField`

#### Scenario: Product overrides the settings

- **WHEN** the settings prim authors `disableDepthOfField = true` and the first
  product authors `disableDepthOfField = false`, and the camera authors
  `fStop = 2`
- **THEN** the render has depth of field

#### Scenario: Depth of field stays on by default

- **WHEN** `disableDepthOfField` is not authored and the camera authors
  `fStop = 2`
- **THEN** the render has depth of field

#### Scenario: Products that disagree

- **WHEN** the first product authors `disableDepthOfField = true` and a second
  product with the same camera and resolution authors
  `disableDepthOfField = false`
- **THEN** both products are written without depth of field and one
  `product.depth_of_field_mismatch` warning names the second product
