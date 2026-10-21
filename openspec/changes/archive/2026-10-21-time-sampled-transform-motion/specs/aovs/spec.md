# Spec Delta

## ADDED Requirements

### Requirement: Motion vectors over an instantaneous shutter are reported

When a `motionvector` var is requested and the shutter is instantaneous, while a drawn
prim's world transform varies over time, the render SHALL warn once. The warning SHALL
say that the time-sampled motion reads zero, and that authoring the camera's
`shutter:open` / `shutter:close` measures it. The vector SHALL still be produced for
`crust:motion:translate`.

#### Scenario: Animated stage with no shutter authored

- **WHEN** `samples/animation.usda` is rendered with `-f 5` and a `motionvector` var,
  and its camera authors no shutter
- **THEN** one warning names the instantaneous shutter, and the vector is `(0, 0)` on the
  moving sphere

#### Scenario: Static stage with no shutter authored

- **WHEN** a stage with no time-sampled transforms requests `motionvector` and its camera
  authors no shutter
- **THEN** no warning about the shutter is raised

## MODIFIED Requirements

### Requirement: Motion vector AOV

The `motionvector` source SHALL be the first hit's forward 2D screen-space
displacement from shutter open to shutter close, in pixels of the rendered
image. `u` SHALL be positive to the right and `v` positive upwards, as Nuke
expects. A hit on geometry that moves over the shutter SHALL report
where that motion takes the hit's shutter-open point on screen. That motion is an
authored `crust:motion:translate` or a time-sampled transform, including rotation and
scale. Units
SHALL be per shutter interval, the camera's `shutter:open` to `shutter:close`. The
source SHALL have two floating-point
components (float or half) and a clear value of 0.

#### Scenario: Object moving right

- **WHEN** a sphere authors `crust:motion:translate` along the camera's
  right axis, and a `motionvector` var is requested
- **THEN** pixels on the sphere hold a positive `u`, equal to the screen
  distance in pixels between the projections of the hit's shutter-open
  point and that point plus the translation, and a `v` of 0 within
  tolerance where the sphere is centred vertically

#### Scenario: Object moving up

- **WHEN** a sphere authors `crust:motion:translate` along the camera's up
  axis
- **THEN** pixels on the sphere hold a positive `v`

#### Scenario: Time-sampled translate

- **WHEN** `samples/animation.usda`'s `Mover` (whose `xformOp:translate` goes from
  (-2, 0.6, 0) at frame 1 to (2, 0.6, 0) at frame 10) is rendered with `-f 5`, a camera
  authoring `shutter:close = 1`, and a `motionvector` var
- **THEN** its centre pixel's `u` equals, within 0.01 px, the screen distance between the
  projections of the hit's shutter-open point and that point moved 4/9 units along +X,
  and its `v` is 0 within tolerance

#### Scenario: Time-sampled rotation

- **WHEN** a cube spinning about the camera's view axis through its centre by a
  time-sampled `xformOp:rotateZ` is rendered with a non-instantaneous shutter and a
  `motionvector` var
- **THEN** each pixel on the cube holds the screen displacement of its hit point rotated
  by the shutter's angle about that axis: tangential, longer away from the axis, and
  near zero at its centre

#### Scenario: Shutter length scales the vector

- **WHEN** the same animated stage is rendered once with `shutter:close = 0.5` and once
  with `shutter:close = 1`, both with `shutter:open = 0`, for a prim moving at constant
  speed parallel to the image plane at constant depth
- **THEN** the second render's vectors are twice the first's, within floating-point
  tolerance

#### Scenario: Static geometry and escapes

- **WHEN** a pixel's chosen sample hits geometry that does not move over the
  shutter, hits a volume, or escapes to a dome or the sky
- **THEN** the pixel holds `(0, 0)`

#### Scenario: Perspective varies the vector across an object

- **WHEN** a long plane, receding from the camera, translates parallel to the
  image plane
- **THEN** its near pixels hold longer vectors than its far pixels

#### Scenario: Wrong component count is refused

- **WHEN** a `motionvector` var authors `dataType = "float"`
- **THEN** a warning names the var, and the product has no channel for it

#### Scenario: Integer types are refused

- **WHEN** a `motionvector` var authors `dataType = "int2"` or `"uint2"`
- **THEN** a warning names the var, and the product has no channel for it,
  since unsigned samples cannot hold leftward or downward motion
