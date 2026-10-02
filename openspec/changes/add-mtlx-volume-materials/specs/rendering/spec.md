## ADDED Requirements

### Requirement: Paths cross medium boundaries without a vertex

A segment that hits a medium boundary SHALL continue past it without a vertex,
spending no path depth and keeping the previous vertex's MIS record, in the
medium the crossing leaves it in: a boundary's front face met in vacuum enters
its medium; that boundary's own back face leaves it; any other crossing changes
nothing. While a path is inside a boundary's medium, every ray it traces SHALL
travel in that medium.

#### Scenario: An absorbing boundary dims by its chord

- **WHEN** a unit sphere bound to a volume-only absorber with σₐ = (0.25, 0.5,
  1) is seen through its centre against a white sky
- **THEN** the radiance is e^{−2σₐ} within the 0.001 restart epsilon, whatever
  the length of the camera ray's direction

### Requirement: Scatters inside a medium boundary sample lights

A scatter in a boundary's medium SHALL run light sampling with the medium's
phase function, MIS-weighted against phase sampling, and its shadow rays SHALL
cross boundaries under the same rules as paths, attenuated by Beer–Lambert
through each medium they travel in.

#### Scenario: A white furnace stays white

- **WHEN** a non-absorbing scattering volume-only sphere, optionally with a
  white Lambertian sphere inside it, sits under a uniform white sky
- **THEN** the mean radiance through it is 1 within 0.025, under power MIS,
  light sampling alone and phase sampling alone

### Requirement: A world without medium boundaries renders as before

A world in which no material is a medium boundary SHALL render bit for bit as
it did before medium boundaries existed.

#### Scenario: Existing samples are unchanged

- **WHEN** every sample scene without a medium boundary or a volume region is
  rendered at 16 spp before and after
- **THEN** the images are bit-identical

### Requirement: Volume regions measure distance

Free flights and transmittance through `crust:volume` regions SHALL be measured
in distance, whatever the length of the ray's direction.

#### Scenario: Region fog does not depend on the focus distance

- **WHEN** a pinhole camera renders a region of fog at focus distance 1 and at
  focus distance 10
- **THEN** the two images are the same within noise
