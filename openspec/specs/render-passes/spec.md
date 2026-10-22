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
once per collection. In a streamed import, an included collection authored in a
different top-level subtree from the chunk being imported is not composed there:
it SHALL contribute nothing to that chunk's membership and SHALL warn once per
collection (`render_pass.nested_not_composed`).

#### Scenario: A membership expression

- **WHEN** `collection:matte` sets `includeRoot = 0` and
  `membershipExpression = "/World/Chars//"`
- **THEN** every geometry under `/World/Chars` is a holdout and nothing else is

#### Scenario: The nearest path decides

- **WHEN** `matte` includes `/World/Props` and excludes `/World/Props/Hero`
- **THEN** `/World/Props/Hero/Body` is not a holdout and `/World/Props/Crate` is

#### Scenario: An included collection outside the streamed chunk

- **WHEN** a streamed import reads a pass whose `matte` includes
  `</World/Sets.collection:hero>`, and the hero's geometry is under `/World/Chars`
- **THEN** one `render_pass.nested_not_composed` warning names the collection, and
  the hero is not a holdout

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

When the active pass's `collection:prune` restricts anything, the render SHALL
warn once that it is not read and render as if it were unauthored. A volume
region in `matte`, or excluded from `cameraVisibility` or `renderVisibility`,
SHALL warn and render as without the pass.

#### Scenario: A pass that prunes

- **WHEN** the active pass's `collection:prune:includes` targets `/World/Set`
- **THEN** one warning names the pass and `prune`, and `/World/Set` renders

#### Scenario: A held-out volume

- **WHEN** `matte` includes a `Volume` prim
- **THEN** one warning names the volume, and the camera sees it as without a pass

#### Scenario: A render-hidden volume

- **WHEN** a `Volume` prim is outside the active pass's `renderVisibility`
- **THEN** one warning names the volume, and every ray sees it as without a pass

### Requirement: Known gaps

The following SHALL be documented as not supported:

- `prune` is not read, and volume regions are neither held out nor hidden by a
  pass.
- A holdout behind a thin wall that the camera ray passes straight through
  (glass at IOR 1) renders black with the wall's alpha of 1, not as a hole.
- There is no shadow catcher: a holdout records no shadow or reflection of the
  other objects into alpha.
- Geometry a pass hides from every ray still costs that pass traversal time: it
  stays in the acceleration structure the passes share.
- In a streamed import, a pass collection that includes another collection
  authored in a different top-level subtree cannot read it while that subtree's
  chunk is imported; it contributes nothing there.

#### Scenario: Gaps are documented

- **WHEN** a user reads the render-passes page of the user documentation
- **THEN** each gap above is listed there

### Requirement: The render-visibility collection hides from every ray

Geometry outside the active pass's `collection:renderVisibility` (includeRoot
fallback true) SHALL be invisible to every ray category (camera, shadow,
indirect and light-sampling), as `crust:rayMask = 0` makes it. The collection
SHALL be resolved with the same `UsdCollectionAPI` semantics, on the same prim
path, as `matte` and `cameraVisibility`.

#### Scenario: A background plate alone

- **WHEN** the active pass sets `collection:renderVisibility:includeRoot = 0` and
  includes only `/World/Set/PlateA`
- **THEN** the camera sees only `PlateA`, and no other geometry shadows it,
  reflects in it or shows through it

#### Scenario: A pass that authors no render visibility

- **WHEN** the active pass authors no `collection:renderVisibility` property
- **THEN** the image and every AOV are bit-identical to the same pass before this
  capability read `renderVisibility`

### Requirement: Lights outside render visibility are off

A light outside the active pass's `renderVisibility` SHALL emit nothing: it
lights no surface or volume, and no ray sees its emitter. An AOV that selects
only that light (a light group, a light path expression naming it) SHALL be zero.
Light linking SHALL be resolved over the lights the pass keeps.

#### Scenario: Plates with the lights off

- **WHEN** the active pass excludes `/World/Lights` from `renderVisibility`, and
  `/World/Lights` holds the shot's only `RectLight` and `DomeLight`
- **THEN** the render is lit by nothing, and escaping camera rays see no dome

#### Scenario: A hidden area light no longer occludes

- **WHEN** a `RectLight` authoring `crust:light:cameraVisible = true` is outside
  `renderVisibility`
- **THEN** no ray, shadow rays toward other lights included, hits its surface

### Requirement: Render visibility takes precedence over the other collections

A prim outside `renderVisibility` SHALL be invisible whatever `matte` and
`cameraVisibility` say of it: it is not a holdout, and it does not count as
held out or hidden from the camera in any report. A pass SHALL never make
visible what `crust:rayMask` hides.

#### Scenario: Held out and render-hidden

- **WHEN** `/World/Props/Crate` is in the active pass's `matte` and outside its
  `renderVisibility`
- **THEN** camera rays pass through where the crate stands, as if it were absent,
  and it casts no shadow

### Requirement: Render visibility is decided before light linking

The active pass's render visibility SHALL be applied before any light link is
resolved. A light linked only to geometry the pass hides from every ray SHALL be
treated as linked to nothing, under the rule light linking already applies.

#### Scenario: A light whose only target is hidden

- **WHEN** a `SphereLight`'s `collection:lightLink` includes only `/World/Hero`,
  and the active pass excludes `/World/Hero` from `renderVisibility`
- **THEN** the sphere light lights nothing in the pass, as a light linked to
  nothing does

### Requirement: The render source is read as composed

A pass's `renderSource` settings prim SHALL be read with its composition arcs
applied: `inherits`, `specializes` and `references`. A setting the prim does not
author SHALL be the value its arcs bring in. A pass can therefore override any
of its base settings, `disableDepthOfField` included, without restating the rest.

#### Scenario: A sharp pass from a shot with depth of field

- **WHEN** `/Render/settings` renders a camera with depth of field, and
  `/Render/Passes/util.renderSource` targets `/Render/sharp`, which
  `inherits = </Render/settings>` and authors only `disableDepthOfField = 1`
- **THEN** the util pass renders through a pinhole at `/Render/settings`'s
  resolution, camera and sample count

### Requirement: Every pass of a stage in one render

`crust render --all-passes` SHALL render every `RenderPass` prim that
`crust ls pass` lists, one after another in that order, except that passes
sharing an import render together: the imports in the order of their first
pass, each one's passes in `crust ls pass` order. Each pass SHALL render as
the active pass, with its own settings and products. A stage with no
`RenderPass` SHALL stop with an error before rendering.

#### Scenario: Two passes in one command

- **WHEN** a stage defines `/Render/Passes/char` (writing `char.exr`) and
  `/Render/Passes/props` (writing `props.exr`), and the user runs
  `crust render -i shot.usda --all-passes`
- **THEN** both files are written, `char.exr` first

#### Scenario: No passes

- **WHEN** the user runs `crust render -i samples/cornellbox.usda --all-passes`
- **THEN** an error says the stage defines no `RenderPass`, and nothing is
  written

### Requirement: Each pass renders as it does alone

Every image and AOV that `--all-passes` writes for a pass SHALL be bit-identical
to the one `crust render --pass <that pass>` writes with the same other flags.

#### Scenario: Bit-identical to the single pass

- **WHEN** `crust render -i samples/render_passes.usda --all-passes -s 16` and
  `crust render -i samples/render_passes.usda --pass /Render/Passes/char -s 16`
  each write `char.exr`
- **THEN** `crust diff` of the two reports them identical

### Requirement: Passes share an import

`--all-passes` SHALL import the stage once for every group of passes whose
settings agree on everything the import builds into the scene (dicing,
subdivision and displacement settings among them). Passes whose settings differ
only in what a render reads (camera, resolution, products, sample counts,
integrator settings) SHALL share one import.

#### Scenario: One import for two passes

- **WHEN** `render_passes.usda`'s two passes differ only in products, and the
  user runs `crust render -i samples/render_passes.usda --all-passes`
- **THEN** the log reports one import serving both passes, and two renders

#### Scenario: A pass that dices differently

- **WHEN** one pass's settings author `crust:subdivisionLevel = 3` and the other's
  author none
- **THEN** the log reports two imports, one for each pass
