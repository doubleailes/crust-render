## MODIFIED Requirements

### Requirement: Area lights

Rect, sphere, disk and cylinder lights SHALL be one-sided `AreaLight`s whose
geometry is also attached to the world. Sphere lights SHALL sample the cone
they subtend and rect lights the spherical rectangle they subtend where that
density is well conditioned. Disk lights SHALL sample by area. Cylinder lights with a
one-sided emitter SHALL sample, from a shading point outside the tube, only the
part of their wall that faces it. By default (`CRUST_TUBE_SAMPLING = equiangular`)
they SHALL draw the axial position along the sampled wall line by a density that
follows its inverse-square falloff. They SHALL sample by area from inside the tube
or with a two-sided emitter. `CRUST_TUBE_SAMPLING = area` SHALL restore area
sampling for cylinders. A non-uniform scale SHALL be honoured through an
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
  every value of `CRUST_TUBE_SAMPLING`
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

#### Scenario: Area sampling restored

- **WHEN** a stage with disk and cylinder lights is rendered with
  `CRUST_TUBE_SAMPLING=area`
- **THEN** the image is bit-identical to the same render before this requirement

#### Scenario: Scenes without round lights are unchanged

- **WHEN** a stage with no disk or cylinder light is rendered at 16 spp
- **THEN** the image is bit-identical to the same render before this requirement


### Requirement: Light selection

NEE SHALL take `crust:lightSamples` / `--light-samples` light samples at the
camera vertex and `crust:lightSamplesIndirect` / `--light-samples-indirect` at
every later surface and volume vertex (each default 1, from 1 to 1024; a stage
value outside that range is clamped with a warning, a command-line one refused).
The light
of each sample SHALL be chosen by `crust:lightSelection` / `--light-selection`:
`power` (default; infinite lights keep a uniform share, the finite lights split
the rest half evenly and half by flux), `tree` (infinite lights keep a uniform
share, and the finite lights are chosen by walking a light BVH by an importance
bound at the shading point's position and normal, mixed with a defensive share
of the `power` probabilities), `uniform` (bit-identical to the
historical renderer), `learned` (a visibility-aware per-cell table trained by
a deterministic pre-pass, over the power table). The samples at one vertex SHALL
be stratified, so that each light is chosen close to the count times its
selection probability.

#### Scenario: Uniform selection

- **WHEN** a render runs with `--light-selection uniform`
- **THEN** each of N lights is picked with probability 1/N

#### Scenario: One sample per vertex is unchanged

- **WHEN** a render runs with both sample counts at 1, given or by default,
  under `power` or `uniform`
- **THEN** the image is bit-identical to the renderer before sample counts
  existed under the same selection

#### Scenario: Stratified picks

- **WHEN** a vertex takes 4 light samples among three lights whose selection
  probabilities are 0.5, 0.25 and 0.25
- **THEN** the first light is sampled twice and each other light once

#### Scenario: More samples, same image

- **WHEN** a scene is rendered with `--light-samples 4` and with the default,
  at increasing sample counts with the indirect clamp off
- **THEN** the difference between the two falls as 1/√N, and the direct-light
  noise per pixel sample is lower with 4

#### Scenario: Neural selection is no longer accepted

- **WHEN** a render runs with `--light-selection neural`, or its render settings
  author `crust:lightSelection = "neural"`
- **THEN** the command line rejects the value, and the render settings log one
  `WARN` and pick by `power`

#### Scenario: Tree selection is accepted

- **WHEN** a render runs with `--light-selection tree`, or its render settings
  author `crust:lightSelection = "tree"`
- **THEN** the render completes, and the `--light-selection` help lists `tree`
  with a one-line description, while `power` stays the default

#### Scenario: Existing modes are unchanged

- **WHEN** a checked-in sample renders at 16 spp under `power`, `uniform`,
  or `learned`, each given explicitly
- **THEN** the image is bit-identical to the one rendered under the same mode
  before `tree` existed


## REMOVED Requirements

### Requirement: Neural selection falls back to power

**Reason**: `neural` selection is removed; it lost at equal time on every scene measured.
**Migration**: use `learned` for visibility-aware selection.

### Requirement: Neural selection is unbiased

**Reason**: `neural` selection is removed.
**Migration**: none.

### Requirement: Neural selection weights both MIS sides alike

**Reason**: `neural` selection is removed.
**Migration**: none.

### Requirement: Neural selection is deterministic

**Reason**: `neural` selection is removed.
**Migration**: none.
