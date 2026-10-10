## ADDED Requirements

### Requirement: EXRs record the exposure scale

Every EXR the render writes SHALL record the exposure scale its radiance channels were
multiplied by, as a float attribute `crust:exposureScale` in its header, `1` when no
exposure applied.

#### Scenario: An exposed render

- **WHEN** a stage whose camera authors `exposure = 2` is rendered
- **THEN** each EXR written records `crust:exposureScale = 4`

#### Scenario: No exposure

- **WHEN** a stage whose camera authors no exposure is rendered
- **THEN** each EXR written records `crust:exposureScale = 1`
