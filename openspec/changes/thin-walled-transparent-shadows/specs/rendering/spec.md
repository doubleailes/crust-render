## MODIFIED Requirements

### Requirement: Cutout surfaces are stochastic presence

A hit on a surface whose material reports an opacity below 1 SHALL be met with
probability equal to that opacity, and otherwise passed through. The path SHALL
continue along the same line to the next hit, spending no depth, adding no
emission and recording no vertex, while the carried medium, volume regions and
texture footprint keep measuring the segment from its origin. A shadow ray
SHALL be attenuated by `1 − opacity` at every cutout it crosses, by the factor
"Thin-walled transmission is a pass-through" defines at every thin-walled
transmissive surface it crosses, and blocked by any other surface. A world with
no cutout material and no thin-walled transmissive material SHALL render exactly
as it did before cutouts existed.

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

## ADDED Requirements

### Requirement: Thin-walled transmission is a pass-through

A material whose thin-walled transmission leaves a ray's direction unchanged
SHALL report, for each crossing direction ω, the RGB weight `T(ω)` its BSDF
gives that straight transmission. With opacity `α`, the fraction of a ray that
continues unscattered along the same line is `P(ω) = (1 − α) + α · T(ω)`.

A shadow ray SHALL be multiplied by `P(ω)` at every such surface it crosses.

A path SHALL pass such a surface with probability `q = max over channels of
P(ω)`, scaling its throughput by `P(ω) / q`, spending no depth, adding no
emission, recording no vertex, and keeping the previous vertex's MIS record.
Otherwise it SHALL meet the surface and scatter through the material without its
straight transmission, scaling its throughput by `α / (1 − q)`.

A pass SHALL be a specular transmission event (`TS`) for light path
expressions. `Material::resolve` SHALL report the same `T(ω)` and the same
reduced scattering as per-query shading. This requirement SHALL NOT change the
expected value of any pixel. It changes only which strategy finds the light,
and so the noise.

#### Scenario: Light through a window is found by every strategy

- **WHEN** a thin-walled sheet with `transmission_weight = 1` and a coloured
  `transmission_color` hangs between a diffuse floor and a sphere light
- **THEN** the power-MIS, light-only and BSDF-only estimates of the floor agree,
  and light sampling alone no longer finds the floor unlit

#### Scenario: The expectation does not move

- **WHEN** the scene above is rendered before and after this requirement, at
  increasing sample counts with the indirect clamp off
- **THEN** the difference between the two falls as 1/√N, with either sign pixel
  to pixel, and the relative error at equal time is lower after

#### Scenario: A sheet that is also a cutout

- **WHEN** a thin-walled transmissive sheet has an opacity of 0.5 and a straight
  transmittance `T`
- **THEN** shadow rays through it are multiplied by `0.5 + 0.5 · T`, and every
  strategy agrees on the floor beneath it

#### Scenario: Thick glass is unchanged

- **WHEN** a closed, thick (not thin-walled) dielectric stands between a floor
  and a light
- **THEN** shadow rays are blocked by it, as before this requirement
