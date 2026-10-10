## MODIFIED Requirements

### Requirement: Area lights

Rect, sphere, disk and cylinder lights SHALL be one-sided `AreaLight`s whose
geometry is also attached to the world. Sphere lights SHALL sample the cone
they subtend and rect lights the spherical rectangle they subtend where that
density is well conditioned. Disk lights SHALL sample the spherical ellipse they
subtend from a shading point strictly in front of their emitting side, where that
solid angle is well conditioned, and by area otherwise. Cylinder lights with a
one-sided emitter SHALL sample, from a shading point outside the tube, only the
part of their wall that faces it. They SHALL draw the axial position along the
sampled wall line by a density that follows its inverse-square falloff. They
SHALL sample by area from inside the tube or with a two-sided emitter.
`CRUST_DISK_SAMPLING = area` and `CRUST_TUBE_SAMPLING = area` SHALL restore area
sampling for the respective shape. A non-uniform scale SHALL be honoured through an
affine shape, by every strategy. A light's source geometry SHALL be invisible to
camera rays by default. An authored `crust:rayMask` SHALL decide its visibility
outright. Otherwise `crust:light:cameraVisible` SHALL decide it, and otherwise
`primvars:ri:attributes:visibility:camera` (non-zero means visible).

#### Scenario: A light's source geometry in frame

- **WHEN** a camera ray would hit a light's source geometry
- **THEN** the geometry is invisible to it unless the prim opts in with
  `crust:light:cameraVisible = 1`,
  `primvars:ri:attributes:visibility:camera = 1` or an authored `crust:rayMask`

#### Scenario: The crust attribute wins over the RenderMan primvar

- **WHEN** a rect light authors `crust:light:cameraVisible = 0` and
  `primvars:ri:attributes:visibility:camera = 1`
- **THEN** its source geometry is invisible to camera rays

#### Scenario: Disk and cylinder sampling is unbiased

- **WHEN** a diffuse floor lit by a disk light and a cylinder light, each under a
  non-uniform scale, is rendered light-only, BSDF-only and with power MIS, under
  every value of `CRUST_DISK_SAMPLING` and `CRUST_TUBE_SAMPLING`
- **THEN** all the estimates of the floor agree within noise that falls as 1/√N

#### Scenario: A tube's light samples face the shading point

- **WHEN** a one-sided cylinder light is sampled from a point outside the tube
- **THEN** every sample lies on the part of the wall that faces the point, and none
  carries zero radiance for facing away

#### Scenario: A two-sided tube seen through its open end

- **WHEN** a cylinder light with a two-sided emitter is seen from a point on its
  axis beyond one open end, so that its inner wall is visible
- **THEN** it is sampled by area, and its light-only and BSDF-only estimates agree
  within noise

#### Scenario: A disk seen from behind

- **WHEN** a disk light is sampled from a point behind its emitting side
- **THEN** it is sampled by area, as before this requirement

#### Scenario: Area sampling restored

- **WHEN** a stage with disk and cylinder lights is rendered with
  `CRUST_DISK_SAMPLING=area CRUST_TUBE_SAMPLING=area`
- **THEN** the image is bit-identical to the same render before this requirement

#### Scenario: Scenes without round lights are unchanged

- **WHEN** a stage with no disk or cylinder light is rendered at 16 spp
- **THEN** the image is bit-identical to the same render before this requirement
