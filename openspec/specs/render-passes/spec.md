# render-passes Specification

## Purpose

Renders one pass of a multi-pass `UsdRenderPass` setup from a single stage: the
application names the active pass, and that pass's `matte` and `cameraVisibility`
collections decide which geometry is held out and which the camera cannot see.

## Requirements

### Requirement: The application names the active pass

A render SHALL have an active pass only when the host names a `RenderPass` prim
(the CLI's `--pass`, the MCP `open_session`'s `pass`). A stage that defines
`RenderPass` prims but is rendered with none named SHALL render with no active
pass. A named path that is not a `RenderPass` prim on the stage SHALL stop the
render with an error naming the path.

#### Scenario: A named pass is active

- **WHEN** a stage defines `/Render/Passes/char` and `/Render/Passes/bg`, and the
  user runs `crust render -i shot.usda --pass /Render/Passes/char`
- **THEN** the render uses `/Render/Passes/char`'s collections and ignores
  `/Render/Passes/bg`

#### Scenario: Passes on the stage but none named

- **WHEN** the same stage is rendered with no `--pass`
- **THEN** no geometry is a holdout and no geometry is hidden from the camera by
  either pass

#### Scenario: A path that is not a pass

- **WHEN** `--pass /World/Chars` names an `Xform`
- **THEN** the render stops before importing geometry, with an error naming
  `/World/Chars` as not a `RenderPass`

### Requirement: No active pass leaves renders unchanged

With no active pass, the rendered image and every AOV SHALL be bit-identical to
the renderer without this capability, and the render SHALL execute the same
instructions per camera ray up to one branch.

#### Scenario: Golden images hold

- **WHEN** `scripts/check_images.sh check` runs over the checked-in samples
  with no `--pass`
- **THEN** it reports no difference

### Requirement: The active pass chooses its render settings

When the active pass's `renderSource` relationship targets a `RenderSettings`
prim, the render SHALL read its settings (resolution, camera, products and every
`crust:` render parameter) from that prim instead of the stage's
`renderSettingsPrimPath`. When `renderSource` is unauthored, the stage's settings
SHALL be read as without a pass. A target that is not a `RenderSettings` prim
SHALL warn and fall back to the stage's settings.

#### Scenario: Each pass renders its own products

- **WHEN** `/Render/Passes/char.renderSource` targets `/Render/charSettings`,
  whose products write `char.exr`, and the stage's `renderSettingsPrimPath` names
  `/Render/settings`, whose products write `beauty.exr`
- **THEN** `crust render --pass /Render/Passes/char` writes `char.exr` and not
  `beauty.exr`

#### Scenario: A pass with no render source

- **WHEN** the active pass authors no `renderSource`
- **THEN** the render reads `/Render/settings` exactly as with no pass

#### Scenario: A render source that is not settings

- **WHEN** `renderSource` targets a `Camera` prim
- **THEN** a warning names the pass and the target, and the stage's settings are
  used

### Requirement: Matte collection members are holdouts

Every geometry brought in by a member of the active pass's `collection:matte`
SHALL be a holdout. The collection's `includeRoot` fallback SHALL be false, so a
pass that authors nothing on `matte` holds out nothing. A holdout's camera-ray
behaviour is specified by the `rendering` and `aovs` capabilities; every other ray
category SHALL see it as ordinary geometry.

#### Scenario: A character pass holds out the props

- **WHEN** the active pass's `collection:matte:includes` targets `/World/Props`,
  and the camera sees a character standing behind a crate under `/World/Props`
- **THEN** the pixels where the crate covers the character have zero colour and
  zero alpha, and the crate's shadow still darkens the character

#### Scenario: A pass that authors no matte

- **WHEN** the active pass authors no `collection:matte` property
- **THEN** no geometry is a holdout

### Requirement: The camera-visibility collection hides from camera rays only

Geometry and lights outside the active pass's `collection:cameraVisibility`
(includeRoot fallback true) SHALL be invisible to camera rays. Shadow, indirect
and light-sampling rays SHALL see them exactly as without a pass. A pass SHALL
only remove camera visibility: a member that `crust:rayMask` or the hidden-light
rule already hides from the camera SHALL stay hidden.

#### Scenario: A blocker hidden in one pass only

- **WHEN** the active pass's `collection:cameraVisibility:excludes` targets
  `/World/Blocker`
- **THEN** camera rays pass `/World/Blocker` to what lies behind it, and it still
  shadows the surfaces it shadowed without the pass

#### Scenario: A pass cannot reveal a hidden light

- **WHEN** a `SphereLight` that authors no camera visibility (hidden by default)
  is a member of the active pass's `cameraVisibility`
- **THEN** the camera still does not see the light's surface

#### Scenario: An area light hidden by the pass stays solid

- **WHEN** a `RectLight` authoring `crust:light:cameraVisible = true` is excluded
  from the active pass's `cameraVisibility`
- **THEN** the camera does not see it, and it still occludes shadow rays toward
  other lights as it does without the pass

### Requirement: Lights at infinity follow the pass

A `DomeLight` or `DistantLight` outside the active pass's `cameraVisibility`, or
inside its `matte`, SHALL be invisible to escaping camera rays, as
`domeLightCameraVisibility = false` makes it, for that light only. Its
illumination SHALL be unchanged.

#### Scenario: The dome is held out of a character pass

- **WHEN** the active pass's `matte` includes `/World/Sky`, a `DomeLight`
- **THEN** escaping camera rays show neither colour nor alpha from the dome, and
  surfaces are lit by it exactly as without the pass

### Requirement: Pass collections use UsdCollectionAPI membership

`matte` and `cameraVisibility` SHALL be resolved with `UsdCollectionAPI`
semantics: nearest include or exclude wins, `expansionRule`, included collections,
and `membershipExpression`. Membership SHALL be judged on the stage path of the
prim that brings the geometry in. For native and PointInstancer instances that
is the instance or instancer prim, and a target inside a prototype SHALL warn
once per collection.

#### Scenario: A membership expression

- **WHEN** `collection:matte` sets `includeRoot = 0` and
  `membershipExpression = "/World/Chars//"`
- **THEN** every geometry under `/World/Chars` is a holdout and nothing else is

#### Scenario: The nearest path decides

- **WHEN** `matte` includes `/World/Props` and excludes `/World/Props/Hero`
- **THEN** `/World/Props/Hero/Body` is not a holdout and `/World/Props/Crate` is

#### Scenario: A target inside an instance

- **WHEN** `matte` includes a prim under a native instance's prototype
- **THEN** one warning names the collection, and membership is decided on the
  instance prim

### Requirement: Camera visibility is decided before light linking

The active pass's camera visibility SHALL be applied before any light whose
`collection:lightLink` covers no geometry is demoted. Such a light at infinity
that the pass hides from the camera SHALL be dropped, not kept as a camera-only
backdrop.

#### Scenario: A backdrop dome hidden by the pass

- **WHEN** a `DomeLight` linked to nothing would be a camera-only backdrop, and
  the active pass excludes it from `cameraVisibility`
- **THEN** escaping camera rays show nothing of it

### Requirement: Pass collections this capability does not read

When the active pass's `collection:prune` or `collection:renderVisibility`
restricts anything, the render SHALL warn once per collection that it is not read
and render as if it were unauthored. A volume region in `matte` or excluded from
`cameraVisibility` SHALL warn and render as without the pass.

#### Scenario: A pass that prunes

- **WHEN** the active pass's `collection:prune:includes` targets `/World/Set`
- **THEN** one warning names the pass and `prune`, and `/World/Set` renders

#### Scenario: A held-out volume

- **WHEN** `matte` includes a `Volume` prim
- **THEN** one warning names the volume, and the camera sees it as without a pass

### Requirement: Known gaps

The following SHALL be documented as not supported:

- `prune` and `renderVisibility` are not read, and volume regions are neither
  held out nor hidden by a pass.
- A holdout behind a thin wall that the camera ray passes straight through
  (glass at IOR 1) renders black with the wall's alpha of 1, not as a hole.
- There is no shadow catcher: a holdout records no shadow or reflection of the
  other objects into alpha.
- One render renders one pass; several passes re-import the stage.

#### Scenario: Gaps are documented

- **WHEN** a user reads the render-passes page of the user documentation
- **THEN** each gap above is listed there
