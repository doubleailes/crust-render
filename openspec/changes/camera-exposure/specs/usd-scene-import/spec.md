## ADDED Requirements

### Requirement: Camera exposure

The importer SHALL read the render camera's exposure as USD's linear exposure scale
(C++ `UsdGeomCamera::ComputeLinearExposureScale`):
`exposure:responsivity × exposure:time × (exposure:iso / 100) × 2^exposure /
exposure:fStop²`, each attribute read at the import's evaluation time, an unauthored
one at its schema fallback (`exposure:responsivity` 1, `exposure:time` 1,
`exposure:iso` 100, `exposure` 0, `exposure:fStop` 1). The camera's depth-of-field
`fStop` SHALL NOT enter the scale. A scale that is not finite or not positive SHALL be
refused with a `camera.invalid_exposure` warning naming the camera, and SHALL read 1.
Without a render camera prim (the procedural fallback scene), the scale SHALL be 1.

#### Scenario: Nothing authored

- **WHEN** the render camera authors none of the exposure attributes
- **THEN** the exposure scale is exactly 1

#### Scenario: Stops

- **WHEN** the render camera authors `exposure = 2`
- **THEN** the exposure scale is 4

#### Scenario: The full photometric set

- **WHEN** the render camera authors `exposure = 1`, `exposure:time = 0.5`,
  `exposure:iso = 400`, `exposure:fStop = 2` and `exposure:responsivity = 1.5`
- **THEN** the exposure scale is `1.5 × 0.5 × 4 × 2 / 4 = 1.5`

#### Scenario: The depth-of-field f-stop does not expose

- **WHEN** the render camera authors `fStop = 4` and no `exposure:fStop`
- **THEN** the exposure scale is 1

#### Scenario: Animated exposure

- **WHEN** the render camera's `exposure` is time-sampled 0 at frame 1 and 1 at frame 2,
  and the stage is rendered at `-f 2`
- **THEN** the exposure scale is 2

#### Scenario: A scale that cannot apply

- **WHEN** the render camera authors `exposure:fStop = 0`
- **THEN** a `camera.invalid_exposure` warning names the camera and the exposure scale
  is 1
