# Spec Delta

## ADDED Requirements

### Requirement: The camera shutter defines the motion interval

The importer SHALL read the render camera's `shutter:open` and `shutter:close`, which are
offsets in time codes from the frame given by `-f`, with USD's fallback of 0 for each. With
frame `f`, motion SHALL be measured from `f + open` (shutter open) to `f + close` (shutter
close). The shutter SHALL be instantaneous when `close <= open`, or when no frame is given.
The same interval SHALL drive the beauty's motion blur and the `motionvector` AOV.

#### Scenario: Shutter authored on the camera

- **WHEN** the render camera authors `shutter:open = 0` and `shutter:close = 0.5`, and the
  render is made with `-f 5`
- **THEN** motion is measured from time code 5 to time code 5.5

#### Scenario: Shutter opening before the frame

- **WHEN** the camera authors `shutter:open = -0.25` and `shutter:close = 0.25`, and the
  render is made with `-f 5`
- **THEN** motion is measured from time code 4.75 to time code 5.25

#### Scenario: No shutter authored

- **WHEN** the camera authors neither attribute
- **THEN** the shutter is instantaneous, and a stage with time-sampled transforms renders
  bitwise as it did before this change, with or without `-f`

#### Scenario: Shutter of a named camera

- **WHEN** `--camera` or `RenderSettings.camera` names a camera whose shutter is authored,
  and another camera with a different shutter comes first on the stage
- **THEN** the named camera's shutter is used

#### Scenario: Camera settled on differs from the one whose shutter was read

- **WHEN** no camera is named, and the camera the render goes through is not the first
  camera on the stage with payloads unloaded, and the two cameras' shutters differ
- **THEN** one warning names both cameras and the shutter the motion was measured over

### Requirement: Time-sampled transforms move geometry over the shutter

When the shutter is not instantaneous, each mesh, sphere, implicit shape, native
instance and `PointInstancer` placement whose world transform varies over time (its own
ops or an ancestor's) SHALL move from its shutter-open world transform to its
shutter-close one, interpolated linearly. Unless motion blur is disabled, the beauty SHALL
be blurred along that motion.

#### Scenario: Animated translate is blurred

- **WHEN** a sphere's `xformOp:translate` has time samples (-2, 0.6, 0) at frame 1 and
  (2, 0.6, 0) at frame 10, the camera authors `shutter:close = 1`, and the render is made
  with `-f 5`
- **THEN** the sphere is streaked along +X over the distance it covers from frame 5 to
  frame 6, about 0.44 units

#### Scenario: Animated ancestor

- **WHEN** a static mesh sits under an `Xform` whose `xformOp:rotateY` is time-sampled,
  and the shutter is not instantaneous
- **THEN** the mesh moves with its ancestor's rotation over the shutter

#### Scenario: Animated instanceable prim

- **WHEN** an instanceable prim whose prototype holds two meshes has a time-sampled
  transform, and the shutter is not instantaneous
- **THEN** both meshes of that instance move over the shutter, and other instances of the
  same prototype with static transforms do not

#### Scenario: Animated PointInstancer

- **WHEN** a `PointInstancer`'s own `xformOp:translate` is time-sampled, and the shutter
  is not instantaneous
- **THEN** every instance it places moves with that translation over the shutter

#### Scenario: Static prims are unchanged

- **WHEN** the shutter is not instantaneous and a prim's world transform has no time
  samples on itself or any ancestor
- **THEN** that prim renders exactly as it would with an instantaneous shutter

#### Scenario: Other attributes stay at the frame

- **WHEN** a light's intensity and a moving mesh's transform are both time-sampled, and
  the shutter opens before the frame
- **THEN** the light's intensity is evaluated at the frame, and only the mesh's transform
  is evaluated at shutter open and shutter close

### Requirement: Authored translation adds to animated motion

`crust:motion:translate` SHALL remain a world-space displacement over the shutter,
whatever the shutter's length, including an instantaneous one. On a prim whose world
transform also varies over the shutter, the authored translation SHALL be added to that
motion: the prim's shutter-close placement SHALL be its shutter-close world transform,
moved by the authored translation.

#### Scenario: Translation without a shutter

- **WHEN** a sphere authors `crust:motion:translate = (1, 0, 0)` and the camera authors no
  shutter
- **THEN** the sphere is blurred over one unit along X, as before this change

#### Scenario: Translation on an animated prim

- **WHEN** a mesh whose `xformOp:translate` moves it by (0, 1, 0) over the shutter also
  authors `crust:motion:translate = (1, 0, 0)`
- **THEN** each of its points moves by (1, 1, 0) over the shutter

### Requirement: Time-sampled motion that is not produced is reported

The importer SHALL warn, once per cause with a count of the prims involved, when a drawn
prim's world transform varies over time but produces no motion:

- the shutter is not instantaneous but no frame was given;
- the prim is a curve, a light or a volume;
- it lies inside a prototype;
- it is a mesh baked for a non-invertible transform.

Static stages and stages with an instantaneous shutter SHALL raise none of these warnings.

#### Scenario: No frame given

- **WHEN** the camera authors `shutter:close = 0.5`, a mesh's transform is time-sampled,
  and the render is made without `-f`
- **THEN** one warning says that time samples are not evaluated without a frame, and the
  mesh renders sharp

#### Scenario: Animated curves

- **WHEN** a `BasisCurves` prim's transform is time-sampled and the shutter is not
  instantaneous
- **THEN** the curves render sharp at their shutter-open placement, and one warning
  counts them

#### Scenario: Instantaneous shutter is not warned about here

- **WHEN** the camera authors no shutter and a mesh's transform is time-sampled
- **THEN** none of these warnings is raised

## MODIFIED Requirements

### Requirement: Render settings can disable motion blur

`disableMotionBlur = true` SHALL trace every camera ray at shutter open, so
the beauty and every AOV show moving geometry sharp at its shutter-open position.
That is its authored position for `crust:motion:translate`, and its transform at
`f + shutter:open` for a time-sampled transform.
`instantaneousShutter = true` SHALL mean the same. Motion stays in the scene,
so motion vectors are still produced. The value is resolved like the
render's camera: the first product's value, else the settings prim's.
Neither attribute SHALL be warned about as not honoured.

#### Scenario: Sharp beauty with motion authored

- **WHEN** a sphere authors `crust:motion:translate = (1, 0, 0)` and the
  render settings author `disableMotionBlur = true`
- **THEN** the sphere renders sharp at its authored position, with no streak
  along X, and no warning about `disableMotionBlur` is logged

#### Scenario: Sharp beauty at shutter open for an animated transform

- **WHEN** a sphere's transform is time-sampled, the camera authors
  `shutter:open = -0.25` and `shutter:close = 0.25`, the render is made with `-f 5`, and the
  render settings author `disableMotionBlur = true`
- **THEN** the sphere renders sharp where its transform places it at time code 4.75

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
