# Spec Delta

## ADDED Requirements

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
`crust ls pass` lists, one after another in that order. Each pass SHALL render
as the active pass, with its own settings and products. A stage with no
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
  user runs `crust render -i samples/render_passes.usda --all-passes --stats`
- **THEN** the stats report one import phase and two renders

#### Scenario: A pass that dices differently

- **WHEN** one pass's settings author `crust:subdiv:level = 3` and the other's
  author none
- **THEN** the stage is imported twice, once for each pass

## MODIFIED Requirements

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

#### Scenario: Gaps are documented

- **WHEN** a user reads the render-passes page of the user documentation
- **THEN** each gap above is listed there
