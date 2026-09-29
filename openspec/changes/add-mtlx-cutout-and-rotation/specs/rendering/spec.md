## ADDED Requirements

### Requirement: Cutout surfaces are stochastic presence

A hit on a surface whose material reports an opacity below 1 SHALL be met with
probability equal to that opacity, and otherwise passed through. The path SHALL
continue along the same line to the next hit, spending no depth, adding no
emission and recording no vertex, while the carried medium, volume regions and
texture footprint keep measuring the segment from its origin. A shadow ray
SHALL be attenuated by `1 − opacity` at every cutout it crosses, and blocked by
any other surface. A world with no cutout material SHALL render exactly as it
did before cutouts existed.

#### Scenario: A half-present sphere in a furnace

- **WHEN** a black sphere of opacity 0.5 sits in a white furnace of radiance 1
- **THEN** a ray through it sees 0.25, the chance of passing both of its
  crossings

#### Scenario: Every strategy agrees through a cutout

- **WHEN** a black sheet of opacity 0.5 hangs between a diffuse floor and a
  sphere light
- **THEN** the power-MIS, light-only and BSDF-only estimates of the floor all
  agree with half of the unoccluded power-MIS estimate

#### Scenario: Opaque occluders are unaffected

- **WHEN** an opaque sheet hangs between a floor and its light, in a world with
  or without a cutout elsewhere
- **THEN** light sampling alone finds the floor unlit
