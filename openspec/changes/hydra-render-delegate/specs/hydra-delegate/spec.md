## Purpose

Crust as a Hydra 2.0 renderer: a host such as usdview selects "Crust" and sees
the scene path traced progressively, the same image `crust render` gives for the
same scene, while the camera moves and the stage is edited.

## ADDED Requirements

### Requirement: Crust registers as a Hydra renderer

The `hdCrust` plugin SHALL register a Hydra renderer plugin named "Crust" with
OpenUSD 26, found through `PXR_PLUGINPATH_NAME`. It SHALL consume the host's
terminal scene index, not the scene delegate path. A host that loads no `hdCrust`
SHALL be unaffected.

#### Scenario: Picking Crust in usdview

- **WHEN** usdview is started with `PXR_PLUGINPATH_NAME` pointing at the installed
  plugin and a stage is opened
- **THEN** "Crust" appears in the renderer menu, and choosing it shows a path-traced
  image of the stage

#### Scenario: No deprecation notice

- **WHEN** usdview renders with Crust on OpenUSD 26
- **THEN** no warning about the scene delegate code path is printed

### Requirement: The delegate renders a layer crust can render

The delegate SHALL write what it renders as USD layers on disk, a flattened scene
layer and an override layer over it, in its session directory. A render of those
layers by the delegate SHALL be bit-identical to `crust render` of the override
layer, at the same resolution, camera and samples.

#### Scenario: Agreeing with the CLI

- **WHEN** usdview renders a stage with Crust at 16 spp to completion, with the
  session directory set, and the user runs `crust render -i <dir>/edits.usda -s 16`
  at the viewport's resolution
- **THEN** `crust diff` reports the two images identical

#### Scenario: The session directory

- **WHEN** the `crust:sessionDirectory` render setting is set
- **THEN** the layers are written there and kept when the delegate is destroyed;
  unset, they go to a temporary directory removed with the delegate

### Requirement: The flattened layer carries the scene Hydra presents

The scene layer SHALL hold one prim per Hydra prim crust reads, at the same path, with its world transform: meshes with their subdivision scheme, tags and primvars; `BasisCurves`; points; implicit shapes; `Material` networks with direct bindings; UsdLux lights; cameras; instancers as `PointInstancer`s. Asset paths SHALL be written resolved. Only prims Hydra presents as visible, with a render tag the host draws, SHALL be written.

#### Scenario: Composition is already resolved

- **WHEN** a stage composes a mesh through a reference, a variant and an inherited
  material binding
- **THEN** the scene layer holds the mesh with no composition arc, a direct
  `material:binding` to the material's path, and the mesh renders as in
  `crust render` of the original stage

#### Scenario: A MaterialX network

- **WHEN** a material's Hydra network is made of `ND_*` nodes
- **THEN** the layer holds it as `Shader` prims with those `info:id`s, and it renders
  through the inline-network path

#### Scenario: Native instances

- **WHEN** a stage places one prototype through three `instanceable` prims
- **THEN** the layer holds one `PointInstancer` with three instances of one
  prototype, and the render shows three copies

#### Scenario: Hidden and proxy prims

- **WHEN** a prim is invisible, or has purpose `proxy` while usdview shows only
  `render` purpose
- **THEN** it is not written and does not render

### Requirement: Data the delegate does not carry is reported

Hydra data that the flattened layer cannot carry yet SHALL be reported once per
kind and stage, as a host warning naming the first prim: light linking, volume
fields, motion over the shutter, and any prim type crust does not read. The rest of
the scene SHALL still render.

#### Scenario: Light linking

- **WHEN** a stage authors `collection:lightLink` on a light
- **THEN** one warning says light linking is not carried, and the light illuminates
  everything

#### Scenario: An unread prim type

- **WHEN** the scene index holds a prim type crust does not read
- **THEN** one warning names the type and the first prim, and the other prims render

### Requirement: The viewport camera is the render camera

The host's viewing camera and viewport size SHALL be the render camera and
resolution. Its movement SHALL restart the render from its first sample. The stage's
render camera SHALL be the dicing camera, and the viewing camera at first population
when the stage names none.

#### Scenario: Tumbling does not re-import

- **WHEN** the user tumbles the camera on a stage with adaptive subdivision and a
  render camera
- **THEN** each move restarts the render without importing the scene again, and the
  tessellation stays the one diced for the stage's camera

#### Scenario: A stage with no camera

- **WHEN** a stage authors no camera and sets `crust:subdivisionEdgeLength`
- **THEN** geometry is diced for the viewing camera at first population, and keeps
  that tessellation while the camera moves

#### Scenario: Resizing the viewport

- **WHEN** the viewport is resized
- **THEN** the render restarts at the new resolution

### Requirement: Edits follow the session's classes

A change the scene index reports SHALL be written to the override layer and applied
as the MCP session applies an edit: in place for its `settings`, `lights` and
`materials` classes, by a re-import otherwise. A running render SHALL be cancelled
before the change is applied, and restarted after it.

#### Scenario: Dimming a light

- **WHEN** the user changes a light's `inputs:intensity` in usdview's attribute editor
- **THEN** the render restarts without a re-import, and its 16 spp result equals a
  `full` application of the same layers

#### Scenario: Moving a mesh

- **WHEN** a mesh's transform changes
- **THEN** the scene is re-imported, and the render restarts on the new scene

### Requirement: A time change renders the new frame

A change of the host's current time SHALL rebuild the scene layer at the new time and
re-import it. The scene SHALL hold the values at that time only, with no motion over
the shutter.

#### Scenario: Scrubbing

- **WHEN** the user moves usdview's time slider from frame 1 to frame 10 on an
  animated stage
- **THEN** the render shows frame 10, equal to `crust render` of the layers written
  for frame 10

### Requirement: The render is shown while it refines

The beauty SHALL be shown in the host's color buffer as it refines, each new image
at least as many samples per pixel as the last, until the render completes. Other
AOVs the host binds SHALL be filled when the render completes. The render SHALL
report converged exactly when it has completed.

#### Scenario: Progressive refinement

- **WHEN** a 256 spp render starts in usdview
- **THEN** the viewport shows a noisy image within the time of the first sample
  stages, which then refines until the render completes and reports converged

#### Scenario: A depth AOV

- **WHEN** usdview displays the `depth` AOV
- **THEN** after the render completes, the buffer holds crust's `depth` AOV for the
  frame, in Hydra's convention: projected to normalized device depth in [0, 1],
  1 where nothing was hit, which is what the host composites the image with

#### Scenario: Other AOVs

- **WHEN** the host binds `normal`, `Neye` or `Peye`
- **THEN** after the render completes, the buffer holds the values of crust's
  matching AOV (`N`, `Neye`, `Peye`) exactly as `crust render` of the layers writes
  them

### Requirement: Render settings are exposed to the host

The delegate SHALL list as host render settings the samples per pixel, minimum
samples per pixel, variance threshold, maximum depth, indirect clamp, sampling
strategy and the session directory, with crust's defaults. A setting changed in the
host SHALL be written as the `crust:*` attribute on the render settings prim and
applied as a `settings` edit.

#### Scenario: Fewer samples

- **WHEN** the user sets samples per pixel to 16 in usdview's renderer settings
- **THEN** the render restarts without a re-import and completes at 16 spp

### Requirement: Failures never take the host down

An import error, a refused layer or a panic inside crust SHALL be reported as a host
error and SHALL NOT end the host process. The delegate SHALL keep the last image
and accept further changes; the next one SHALL re-import the scene.

#### Scenario: A panic during a render

- **WHEN** crust panics on a render thread
- **THEN** usdview keeps running, an error naming the panic is reported, and the next
  edit or camera move renders again

#### Scenario: A layer crust cannot open

- **WHEN** the scene layer the delegate wrote cannot be opened by crust's import
- **THEN** the import's error is reported, the viewport keeps its last image, and the
  next change that yields an openable layer renders it

### Requirement: Crust's warnings reach the host

Each warning the import raises SHALL be reported to the host once per import, with
its code and prim. Log lines at `WARN` or above SHALL be forwarded as host warnings;
nothing below `WARN` SHALL be forwarded unless the host asks for debug output.

#### Scenario: An import warning

- **WHEN** an imported mesh raises `mesh.invalid_uvs`
- **THEN** usdview's console shows one warning with that code and the mesh's path
