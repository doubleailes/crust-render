# mcp-session Specification

## Purpose

`crust mcp` lets an agent in Claude Desktop keep a USD stage open, edit it through
composition, look at progressive renders and measure them. The session's result is
written to disk as an override layer that sublayers the original, plus the final
render made from that layer.

## Requirements

### Requirement: An MCP server over stdio

`crust mcp` SHALL run a Model Context Protocol server on stdin/stdout until stdin
closes. stdout SHALL carry only protocol messages. The log SHALL go to stderr, and no
progress bar SHALL be drawn.

#### Scenario: Listing tools

- **WHEN** an MCP client initialises the server and asks for its tools
- **THEN** it receives `open_session`, `query`, `check`, `set_attribute`,
  `set_variant`, `set_active`, `bind_material`, `author_usda`, `drop_to_surface`,
  `scatter_on_surface`, `undo`, `reload`, `render`, `snapshot`, `cancel`, `probe`,
  `light_paths`, `diff` and `render_final`, each with a JSON input schema

#### Scenario: Nothing but protocol on stdout

- **WHEN** a session imports a stage that raises warnings
- **THEN** every stdout line parses as a protocol message, and the warnings appear
  on stderr

### Requirement: Opening a session creates an override layer

`open_session` SHALL take an input stage and an output path. When `output` does not
exist, it SHALL write a USD layer there whose only composition arc is a sublayer on
the input, open the stage on that layer, and import it. The layer SHALL author the
input root layer's `metersPerUnit` and `upAxis` when the input authors them, so the
session's stage has the input's units and up axis. It SHALL refuse an `output`
that is one of the input's own layers, and one that is not a `.usda` text layer.

#### Scenario: A new session

- **WHEN** `open_session` is called with `input = shot.usda` and
  `output = shot_lookdev.usda`, and `shot.usda` authors no `metersPerUnit` and no
  `upAxis`
- **THEN** `shot_lookdev.usda` exists, holds `subLayers = [@./shot.usda@]` and no
  prim opinion, and the result reports the camera, resolution and warning count

#### Scenario: The input's units and up axis carry over

- **WHEN** `open_session` is called on an input that authors `metersPerUnit = 1`
  and `upAxis = "Z"`
- **THEN** the new override layer authors `metersPerUnit = 1` and `upAxis = "Z"`
  beside its `subLayers`, and a camera with `fStop = 2` renders as
  `crust render` of the input does

#### Scenario: Output is a source layer

- **WHEN** `output` names the input itself or a layer it composes
- **THEN** the call fails and no file is written

#### Scenario: Output is not a text layer

- **WHEN** `output` is `shot_lookdev.usdc`
- **THEN** the call fails, saying the layer must be a `.usda`, and no file is
  written

### Requirement: Resuming a session

When `output` already exists and is an override layer that sublayers `input`,
`open_session` SHALL open it and resume, keeping its opinions. If the layer does not
author `metersPerUnit` or `upAxis` and the input's root layer does, the session SHALL
add the input's value to the layer before importing it. A value the layer authors is
kept. When `output` exists but does not sublayer `input`, the call SHALL fail without
writing.

#### Scenario: Resuming yesterday's work

- **WHEN** `open_session` is called again with the same `input` and `output` after
  edits were made in an earlier session
- **THEN** the stage composes those edits, and the first `render` shows them

#### Scenario: Resuming a layer written without the input's units

- **WHEN** `output` is an override layer that authors no `metersPerUnit`, over an
  input that authors `metersPerUnit = 1`
- **THEN** after `open_session` the layer authors `metersPerUnit = 1`, and its
  earlier opinions are unchanged

### Requirement: Source layers are never written

A session SHALL write only:

- its override layer;
- the final render's files;
- the `.tx` files `--auto-tx` creates.

No tool SHALL modify, save or create any other layer.

#### Scenario: Editing an opinion that lives in a referenced asset

- **WHEN** the agent sets an attribute whose current value is authored in a
  referenced asset
- **THEN** the new opinion is written to the override layer, and the asset file is
  byte-for-byte unchanged

### Requirement: Edits are opinions in the override layer

`set_attribute`, `set_variant`, `set_active` and `bind_material` SHALL author their
opinion in the override layer, as `over` specs where the prim is defined elsewhere.
An attribute value SHALL take the attribute's declared type: the type a schema of
the prim declares, else the type the strongest layer authors. `set_attribute` SHALL
need an explicit `type` only for an attribute neither declares, and SHALL refuse one
that differs from the declared type. A value that does not convert to that type
SHALL be refused without authoring anything.

#### Scenario: Dimming a light

- **WHEN** the agent calls `set_attribute` on `/lights/key` `inputs:exposure` with
  `-0.5`
- **THEN** the override layer holds `over "lights" { over "key" { float
  inputs:exposure = -0.5 } }` (in its own syntax), and the source layers are
  unchanged

#### Scenario: A schema attribute nothing authors yet

- **WHEN** no layer authors `/scene/Sky.inputs:exposure` on a DomeLight and the agent
  calls `set_attribute` on it with `-1` and no `type`
- **THEN** the override layer holds `float inputs:exposure = -1` on `/scene/Sky`

#### Scenario: A value of the wrong type

- **WHEN** the agent sets `inputs:exposure` to `"bright"`
- **THEN** the call fails, naming the declared type, and the layer is unchanged

#### Scenario: A type that contradicts the schema

- **WHEN** the agent sets a DomeLight's `inputs:exposure` with `type` `"double"`
- **THEN** the call fails, naming `float` as the declared type, and the layer is
  unchanged

#### Scenario: Choosing a variant

- **WHEN** the agent calls `set_variant` with `/asset`, `lod` and `high`
- **THEN** the override layer authors that variant selection, and the next render
  composes the `high` variant

### Requirement: Authoring USDA snippets

`author_usda` SHALL accept USDA text holding prim specs (`over`, `def`, `class`) and
merge them into the override layer as one edit batch. Text that does not parse, or
that changes the layer's sublayers, SHALL be refused without authoring anything.

#### Scenario: Several edits at once

- **WHEN** the agent submits an `over` for a material that sets two inputs and
  rebinds it on a mesh
- **THEN** all three opinions are authored, and the scene is imported once

#### Scenario: Adding a sublayer

- **WHEN** the snippet authors `subLayers` metadata
- **THEN** the call fails and the layer is unchanged

### Requirement: Paths are anchored to the override layer

The sublayer path, and every relative asset path an edit authors, SHALL be written
relative to the override layer's directory. Each path SHALL resolve to the same
file it named when the edit was made.

#### Scenario: Output in another directory

- **WHEN** `input = shots/a/shot.usda`, `output = work/shot_lookdev.usda`, and the
  agent authors a texture path that resolves to `shots/a/tex/wood.png`
- **THEN** opening `work/shot_lookdev.usda` alone in any USD tool resolves the
  texture to `shots/a/tex/wood.png`

### Requirement: Each edit batch is saved and imported

After each edit tool call, the session SHALL save the override layer, then bring the
scene up to date with the saved layer, either in place or by importing it as
`crust render <output>` would. A call's result SHALL report the warning codes the
scene gained or lost, the time taken (`import_s`), and how the edit was applied
(`applied`: `settings`, `lights`, `materials` or `full`).

#### Scenario: An edit that introduces a warning

- **WHEN** the agent authors a `RectLight` with `width = 0`
- **THEN** the result lists `light.degenerate_shape` as gained, the result reports
  `applied = full`, and the file on disk already holds the light

#### Scenario: Dimming a light is applied in place

- **WHEN** the agent sets `inputs:intensity` on an existing `RectLight`
- **THEN** the result reports `applied = lights`

### Requirement: Undo

`undo` SHALL restore the override layer to its content before the most recent edit
batch, save it, and bring the scene up to date with it as an edit would, in place
when the restored change allows it. Undo history SHALL last for the session. After a
resume, undo SHALL NOT reach edits made in earlier sessions. An `undo` whose import
fails SHALL NOT be applied: the layer and the scene stay as they were, and the step
stays in the history.

#### Scenario: Undoing an exposure change

- **WHEN** the agent sets an exposure, then calls `undo`
- **THEN** the override layer's content is equal to its content before the set, the
  undo reports `applied = settings`, and the next render equals the render before the
  set, bit for bit

### Requirement: Source layers changed on disk are picked up

The session SHALL notice when a USD layer the session's import composed (the input, its
sublayers, and the layers behind references and payloads) changes length or
modification time, appears or vanishes. The next tool call that reads the stage or the
scene SHALL then compose and import the sources as they are on disk. An edit SHALL never
be applied in place over a scene imported from a changed source.

#### Scenario: A sublayer rewritten by another program, then an edit

- **WHEN** another program rewrites a layer the input sublayers, and the agent then
  dims a light with `set_attribute`
- **THEN** the result reports `applied` `full`, names the changed layer, and the
  next render is bit-identical to a fresh session's render of the same override layer

#### Scenario: Querying after a source changed

- **WHEN** another program rewrites a referenced layer to change an attribute's
  value, and the agent queries that attribute
- **THEN** the answer is the new value

#### Scenario: Rendering after a source changed

- **WHEN** another program rewrites a layer the input composes, and the agent calls
  `render` with no edit in between
- **THEN** the render shows the rewritten layer, and its result names the changed
  layer

#### Scenario: A changed source that no longer composes

- **WHEN** another program rewrites a source layer so that it no longer parses, and
  the agent calls `render`
- **THEN** the call fails, naming the changed layer and the composition error, nothing
  is rendered, and the next tool call that reads the stage or the scene tries again

#### Scenario: A layer behind a payload

- **WHEN** another program rewrites a layer that a payload arc in the input brings in
- **THEN** the next render shows the rewritten layer

### Requirement: Reloading

`reload` SHALL compose the stage again and import the saved override layer as
`crust render <output>` would, whether or not a source changed. Its result SHALL list
the source layers that changed since the previous import, and the warning codes gained
or lost, as an edit's result does. The undo history SHALL survive a reload.

#### Scenario: Reloading after an external change

- **WHEN** another program rewrites a source layer, and the agent calls `reload`
- **THEN** the result names that layer, and the next render shows it

#### Scenario: Reloading when nothing changed

- **WHEN** the agent calls `reload` and no source changed
- **THEN** the result lists no changed layer, the scene is imported again, and the
  next render is bit-identical to the one before

#### Scenario: Undo after a reload

- **WHEN** the agent sets an attribute, another program rewrites a source layer,
  the agent calls `reload`, then `undo`
- **THEN** the override layer's content is equal to its content before the set, and
  the scene composes it over the rewritten source

#### Scenario: Reloading cancels a running render

- **WHEN** a render is refining and the agent calls `reload`
- **THEN** that render is cancelled before the scene is imported again, and its id
  reports `cancelled`

### Requirement: Placement is computed against the scene the session renders

`drop_to_surface` and `scatter_on_surface` SHALL compute placement against the
geometry the session's current import renders: subdivided, displaced and instanced as
it renders, at the session's frame and at shutter open, counting only geometry visible
to the camera. When a source layer changed on disk, the session SHALL import it before
computing. "Up" SHALL be the stage's `upAxis` (`Y` when none is authored), and "down"
its opposite. Each call that places anything SHALL be one edit batch: saved, brought up
to date as every edit is, one undo step, its result reporting `applied`, `import_s`
and the warning codes gained or lost. A call that places nothing SHALL author nothing
and add no undo step.

#### Scenario: Placing after a source changed

- **WHEN** another program moves a table in a layer the input composes, and the agent
  drops a vase onto it
- **THEN** the vase rests on the table where the rewritten layer puts it

#### Scenario: A Z-up stage

- **WHEN** the stage authors `upAxis = "Z"` and the agent drops a prim
- **THEN** the prim moves along Z only

#### Scenario: Undoing a placement

- **WHEN** the agent drops a prim, then calls `undo`
- **THEN** the override layer's content is equal to its content before the drop, and
  the next render equals the render before the drop, bit for bit

### Requirement: Dropping prims onto the surface below

`drop_to_surface` SHALL take one or more prim paths and, for each, move the prim along
the down axis until its geometry rests on the highest surface beneath it: after the
move no point of the prim's geometry lies below that surface, and at least one rests on
it. The surface SHALL be every other geometry of the scene, or only the geometry of the
`target` prims (and their descendants) when `target` is given. A prim's own geometry
SHALL never count as a surface. A prim already sunk into the surface SHALL be raised.
Each prim SHALL be placed against the scene as it was before the call, so prims in one
call do not land on each other's new positions.

The move SHALL be authored in the override layer as one transform op on the prim,
first in its `xformOpOrder` (after a `!resetXformStack!`), keeping every op the stage
already composes in their order. Dropping a prim again SHALL fold the new move into
that same op. The prim's other opinions and its descendants SHALL be unchanged.

A prim that has no geometry in the scene, or that nothing lies beneath, SHALL be
reported with the reason and left unchanged. A path that is not a prim of the stage,
lies inside an instance, or names a prim that cannot hold a transform, and a call that
names a prim together with one of its ancestors, SHALL be refused with the reason, and
nothing SHALL be authored for any prim of the call.

#### Scenario: A vase onto a table

- **WHEN** the agent calls `drop_to_surface` with a vase prim hanging above a table
- **THEN** the vase's lowest vertex lies on the table top within 1e-4 stage units, no
  vertex lies below it, and its position along the other two axes is unchanged

#### Scenario: A rock on uneven ground

- **WHEN** the agent drops a rock over a terrain mesh that is not flat beneath it
- **THEN** no vertex of the rock lies below the terrain, and at least one lies on it
  within 1e-4 stage units

#### Scenario: Raising a sunken prim

- **WHEN** a box's lower half is below a floor and the agent drops it
- **THEN** the box moves up until its lowest vertex lies on the floor

#### Scenario: Only onto the target

- **WHEN** a cup hangs above a shelf that is above a table, and the agent drops it with
  `target` the table
- **THEN** the cup rests on the table, below the shelf

#### Scenario: The authored transform keeps the prim's own

- **WHEN** a prim composes `xformOpOrder = ["xformOp:translate", "xformOp:rotateY"]`
  from a payload, and the agent drops it twice
- **THEN** the override layer authors one placement op, first in an `xformOpOrder`
  that then lists `xformOp:translate` and `xformOp:rotateY`, and the prim's rotation
  is unchanged

#### Scenario: Nothing below

- **WHEN** the agent drops two prims and nothing lies beneath the second
- **THEN** the first is placed, the result reports the second with `nothing below`,
  and the second's opinions are unchanged

#### Scenario: A prim inside an instance

- **WHEN** one of the paths lies inside an instance
- **THEN** the call fails naming that path, and the layer is unchanged

#### Scenario: A prim with its ancestor

- **WHEN** the agent drops `/World/vase` and `/World/vase/geo` in one call
- **THEN** the call fails naming both, and the layer is unchanged

### Requirement: Aligning a dropped prim to the surface

With `align_to_normal`, `drop_to_surface` SHALL also turn each prim so that its up axis
follows the geometric normal of the surface beneath the centre of the prim's footprint
(the face's normal, whatever vertex normals the mesh authors),
pivoting about the bottom of that footprint, and then drop it as without alignment. A
prim with nothing beneath the centre of its footprint SHALL be reported and left
unchanged.

#### Scenario: A box on a slope

- **WHEN** the agent drops a box with `align_to_normal` onto a plane tilted 30° about
  the X axis
- **THEN** the box's up axis is the plane's normal within 1e-4, and its lowest vertex
  lies on the plane within 1e-4 stage units

#### Scenario: A slope whose vertex normals point elsewhere

- **WHEN** the tilted plane authors vertex normals that all point straight up, and the
  agent drops a box onto it with `align_to_normal`
- **THEN** the box's up axis is the plane's face normal, not the authored normals

### Requirement: Scattering instances over a surface

`scatter_on_surface` SHALL take the `target` prims, the `prototypes` (prim paths of the
stage or asset paths), a new prim path for the instancer, a `seed`, and either a
`count` or a `density` (instances per square stage unit of eligible surface). It SHALL
define one `PointInstancer` at that path in the override layer whose instances lie on
the target's triangles, distributed in proportion to area, each with its prototype's
origin on the surface. Each prototype SHALL be a child of the instancer referencing the
given prim or asset, placed at its own origin: its own transform in the stage is not
carried. The instancer SHALL author `positions`, `orientations`, `scales` and
`protoIndices` (prototypes chosen with equal probability) in its parent's space, and
render as `crust render` reads any `PointInstancer`.

Optional inputs SHALL be:
- `max_slope`: only triangles whose geometric normal is within this many degrees of up
  are eligible;
- `random_rotation`: each instance turned about the up axis by an angle drawn from the
  seed (otherwise none);
- `scale`: a `[min, max]` range each instance's uniform scale is drawn from (otherwise
  1).

The call SHALL be refused, authoring nothing, when:
- the instancer path names a prim the stage already composes, or its parent does not
  exist, is an instance, or lies inside one (USD reads nothing authored there);
- the target has no eligible triangle;
- both or neither of `count` and `density` are given, or more than 100 000 instances
  would be authored;
- a prototype is neither a prim of the stage nor an existing `.usd`, `.usda`, `.usdc`
  or `.usdz` file (an absolute path that names a file is a file);
- `scale` is not `0 < min <= max`, or `max_slope` is not in `(0, 180]`.

Geometry of the target that is not a triangle mesh (curves, implicit shapes) SHALL be
skipped and named in the result.

#### Scenario: Stones over terrain

- **WHEN** the agent scatters 500 instances of two stone assets over a terrain mesh
- **THEN** the override layer defines one `PointInstancer` with 500 positions, each on
  a terrain triangle within 1e-4 stage units, and `protoIndices` naming both stones

#### Scenario: A density

- **WHEN** the agent scatters with `density` 2 over a target whose eligible area is 50
  square units
- **THEN** the instancer holds 100 instances

#### Scenario: Steep faces left bare

- **WHEN** the agent scatters over a mesh with flat and vertical faces, with
  `max_slope` 30
- **THEN** no instance lies on a vertical face

#### Scenario: A prototype from the stage

- **WHEN** a prototype is `/World/rock`, defined at `(10, 0, 0)` in the stage
- **THEN** each instance places the rock's geometry at the instance's position, not 10
  units away, and `/World/rock` itself still renders where it was

#### Scenario: Rendering the scatter

- **WHEN** the agent scatters, then renders at 16 spp to completion
- **THEN** the render is bit-identical to a fresh session's 16 spp render of the same
  saved override layer

#### Scenario: An existing path

- **WHEN** the instancer path names a prim the stage already composes
- **THEN** the call fails naming it, and the layer is unchanged

#### Scenario: A prototype that is not USD

- **WHEN** a prototype is `rock.obj`
- **THEN** the call fails naming it, and the layer is unchanged

#### Scenario: Under an instance

- **WHEN** the instancer path's parent is an instance, or lies inside one
- **THEN** the call fails naming it, and the layer is unchanged

### Requirement: A scatter is reproducible

Two `scatter_on_surface` calls with the same inputs over the same scene SHALL author
byte-identical instancers, whatever the render's sampler setting and the number of
threads. A different `seed` SHALL give a different distribution. Every `seed` from 0 to
4 294 967 295 SHALL scatter up to the most instances a scatter places.

#### Scenario: Scattering again with the same seed

- **WHEN** the agent scatters with seed 7, calls `undo`, and scatters again with the
  same inputs
- **THEN** the override layer's bytes are identical after the two scatters

#### Scenario: Another seed

- **WHEN** the agent scatters with seed 7, undoes it, and scatters with seed 8
- **THEN** the two instancers' `positions` differ

### Requirement: Edits apply in place when they are confined

The session SHALL compare the override layer before and after each edit batch or undo,
and apply the change without a re-import when every changed field belongs to one of
these classes:

- `settings`: attribute values on the `RenderSettings` prim the render uses, or on
  the render camera;
- `lights`: `inputs:*` values on existing UsdLux light prims;
- `materials`: `inputs:*` values on existing shaders, or interface inputs, of
  materials the scene already uses, when the change does not move the material's
  opacity, transmission or emission class.

Any other change SHALL be applied as `full`, a re-import of the saved layer. This
includes a spec added or removed, a relationship, connection, transform, visibility,
primvar, variant selection, activation or composition arc. A batch with changes in
several classes SHALL be applied as the most expensive of them, in the order
`settings` < `materials` < `lights` < `full`. A render after an edit applied in place
SHALL be bit-identical to a render after a `full` application of the same layer
content. With `CRUST_SCENE_DELTAS=0`, every edit SHALL be applied as `full`, with no
assets kept from earlier imports.

Three properties of a production stage SHALL NOT, on their own, make a confined edit
`full`:

- **Tessellation that reads the camera.** When the scene uses adaptive subdivision or
  implicit shapes sized on screen, a `settings` edit SHALL stay in place unless the
  camera projection that tessellation reads changes: the camera's position,
  orientation, focal length, aperture, or the image resolution, or the
  adaptive-subdivision target or level ceiling. Exposure, focus distance, f-stop,
  shutter, sampling settings and render products do not move it.
- **Light linking.** When the scene authors `collection:lightLink` or
  `collection:shadowLink`, a `lights` edit SHALL stay in place, and the resolved links
  SHALL be kept. An edit to a light that the links leave illuminating nothing (a light
  the scene no longer lists as a light) SHALL be `full`.
- **Looks delivered by payloads.** A `materials` edit SHALL classify the same whether
  the material's network is composed directly or through a payload.

#### Scenario: A shader input

- **WHEN** the agent sets `inputs:base_color` on an existing `standard_surface` shader
- **THEN** the result reports `applied = materials`, and a 16 spp render is
  bit-identical to the same render after `crust render <output>` imports the layer

#### Scenario: A transform is not confined

- **WHEN** the agent sets `xformOp:translate` on a mesh
- **THEN** the result reports `applied = full`

#### Scenario: Making a material emissive

- **WHEN** the agent sets `inputs:emission` above 0 on a shader whose material
  emitted nothing
- **THEN** the result reports `applied = full`

#### Scenario: The switch turns it off

- **WHEN** the session runs with `CRUST_SCENE_DELTAS=0` and the agent sets
  `inputs:intensity` on a light
- **THEN** the result reports `applied = full`

#### Scenario: Exposure on an adaptively subdivided scene

- **WHEN** the scene sets `crust:subdivisionEdgeLength` and the agent sets
  `exposure` on the render camera
- **THEN** the result reports `applied = settings`, and a 16 spp render is
  bit-identical to the same render after a `full` application of the layer

#### Scenario: Focal length on an adaptively subdivided scene

- **WHEN** the scene sets `crust:subdivisionEdgeLength` and the agent sets
  `focalLength` on the render camera
- **THEN** the result reports `applied = full`

#### Scenario: Dimming a linked light

- **WHEN** the scene's lights author `collection:lightLink` and
  `collection:shadowLink`, and the agent sets `inputs:intensity` on a linked light
- **THEN** the result reports `applied = lights`, and a 16 spp render is
  bit-identical to the same render after a `full` application of the layer

#### Scenario: A light that illuminates nothing

- **WHEN** a light's `collection:lightLink` includes no receiver, and the agent sets
  its `inputs:intensity`
- **THEN** the result reports `applied = full`

#### Scenario: A shader behind a payload

- **WHEN** a mesh's material is defined in a layer the stage brings in through a
  payload, and the agent sets an `inputs:roughness` on that material's shader
- **THEN** the result reports `applied = materials`, and a 16 spp render is
  bit-identical to the same render after a `full` application of the layer

### Requirement: Assets persist across edits

The session SHALL keep the decoded assets (images, texture tiles, Ptex) of earlier
imports for its lifetime. Before each import, it SHALL discard any kept asset whose
file's length or modification time changed on disk.

#### Scenario: A re-exported texture

- **WHEN** a texture the stage uses is overwritten on disk with different content
  during a session, and the agent then makes any edit
- **THEN** the next render uses the new texture

#### Scenario: Unchanged textures are not decoded again

- **WHEN** the agent makes a `full` edit on a textured stage whose texture files did
  not change
- **THEN** the import decodes no texture file a second time

### Requirement: Time-bounded progressive renders

`render` SHALL start a progressive render of the current scene and return within
`budget_s` seconds. It SHALL return:

- a PNG of the latest snapshot, downscaled to at most 1024 pixels on its long side;
- the samples per pixel reached;
- whether the render is done;
- a render id.

By default (`wait = "image"`), `render` SHALL return as soon as every pixel has taken 4
samples, the render has returned, or `budget_s` has passed, whichever comes first. A
render of 4 samples per pixel or fewer SHALL return when the render does, or at
`budget_s`. With `wait = "done"`, `render` SHALL return when the render does, or at
`budget_s`.

The render SHALL keep refining after the call returns, until it completes or is
cancelled.

#### Scenario: The first image

- **WHEN** the agent calls `render` with `spp = 128` and `budget_s = 60` on a frame
  whose 4-spp stage completes in under a second
- **THEN** the call returns within a few seconds with an image, `done = false` and
  `spp_reached` of at least 4, and a later `snapshot` returns a newer image

#### Scenario: Waiting for completion

- **WHEN** the agent calls `render` with `spp = 16`, `wait = "done"` and a budget the
  render fits in
- **THEN** the call returns with `done = true` and status `done`

#### Scenario: A long render

- **WHEN** the agent calls `render` with `wait = "done"` and `budget_s = 20` on a
  frame that needs minutes
- **THEN** the call returns after at most about 20 seconds with an image and
  `done = false`, and a later `snapshot` returns a newer image

#### Scenario: A render of a region

- **WHEN** `render` is given a region
- **THEN** only that region is rendered, and the image covers only it

#### Scenario: Changing only the samples keeps the light selection

- **WHEN** the stage selects lights by `learned`, and a render names `spp` or a region
  but nothing else differs from the imported settings
- **THEN** the light selection is not trained again, and the render is bit-identical
  to one of a freshly built renderer with the same settings

### Requirement: Snapshot and cancel

`snapshot(render_id, budget_s?)` SHALL return the latest image and progress. With a
`budget_s`, it SHALL first wait for the render to return, for at most that long.
Without one, it SHALL not wait. `cancel(render_id)` SHALL stop the render and keep what
it has done. An edit, or a new `render`, SHALL cancel any render still running.

#### Scenario: Waiting for the converged image

- **WHEN** `render` answered with `done = false`, and the agent calls `snapshot` with
  the render's id and a budget the render fits in
- **THEN** the call returns with `done = true` and the render's final image

#### Scenario: Editing during a render

- **WHEN** a render is refining and the agent calls `set_attribute`
- **THEN** that render is cancelled before the scene is re-imported, and its id
  reports `cancelled`

### Requirement: Renders are reproducible

A session render with the same layer content, the same source layers on disk, the same
region and the same sample count SHALL be bit-identical to `crust render <output>` with
the same region and samples.

#### Scenario: Agreeing with the CLI

- **WHEN** a session renders at 16 spp to completion, and the user runs
  `crust render -i shot_lookdev.usda -s 16`
- **THEN** `crust diff` reports the two images identical

#### Scenario: Agreeing with the CLI after a source changed

- **WHEN** another program rewrites a layer the input composes, then the session
  renders at 16 spp to completion, and the user runs
  `crust render -i shot_lookdev.usda -s 16`
- **THEN** `crust diff` reports the two images identical

### Requirement: Probing a pixel

`probe(render_id, x, y)` SHALL return the pixel's beauty and AOV values in the
render's linear working space. It SHALL also return the prim whose geometry the camera
ray through the pixel's centre first hits, as the import's geometry records it, and the
Material prim bound to it: for a hit inside a native instance or a PointInstancer, the
instance or the instancer prim. `prim` and `material` SHALL be null, with a note saying
why, when the ray hits nothing or the scene has been imported again since the render.

#### Scenario: What is this grey thing?

- **WHEN** the agent probes a pixel on a mesh
- **THEN** the result names the mesh's prim path and its bound material

#### Scenario: A hit inside an instance

- **WHEN** the agent probes a pixel on a prototype's mesh placed by a PointInstancer
- **THEN** the result names the PointInstancer prim, and says so

#### Scenario: A render from before a re-import

- **WHEN** the agent probes a render made before an edit that imported the stage again
- **THEN** `prim` and `material` are null, and the note says to render again

### Requirement: Inspecting the light paths of a session render

`light_paths(render_id, region, group_by, top)` SHALL return the
`crust-light-paths/1` report for `region` of a completed session render, at that
render's settings and sample count. It SHALL refuse a render that has not
completed, a session whose layer differs from the one that render used, and a
region outside the render's region or over 100 pixels.

#### Scenario: From a probe to its paths

- **WHEN** the agent probes a bright pixel at (623, 336) of completed render 3, then
  calls `light_paths(3, [618, 331, 628, 341])`
- **THEN** it receives the report for those 100 pixels, whose beauty values equal
  render 3's

#### Scenario: The session changed

- **WHEN** the agent edits an attribute after render 3, then calls
  `light_paths(3, ...)`
- **THEN** the call is refused with a message saying the session changed since
  render 3 and to render again

#### Scenario: An edit undone

- **WHEN** the agent edits an attribute after render 3, undoes it, then calls
  `light_paths(3, ...)`
- **THEN** the layer is the one render 3 used, and the report is returned

#### Scenario: A render still running

- **WHEN** the agent calls `light_paths` on a render that has not completed
- **THEN** the call is refused with a message naming the render's status

### Requirement: Comparing renders

`diff(render_a, render_b)` SHALL compare two session renders as `crust diff` would
(bitwise identity, plus error metrics on the beauty). It SHALL refuse renders whose
sample counts or regions differ.

#### Scenario: Did the edit matter?

- **WHEN** two completed renders at 16 spp straddle an edit that changes nothing
  visible
- **THEN** `diff` reports them identical

### Requirement: The final render

`render_final` SHALL save the override layer and render it exactly as
`crust render <output>` would. Product paths SHALL resolve against the override
layer's directory, and a stage without products SHALL write `<output stem>.exr` and
its PNG beside the layer. The result SHALL list the files written.

#### Scenario: Ending a session

- **WHEN** the agent calls `render_final` on `work/shot_lookdev.usda`, which
  authors no product
- **THEN** `work/shot_lookdev.exr` and `work/shot_lookdev.png` exist, and are
  identical to what `crust render -i work/shot_lookdev.usda -o
  work/shot_lookdev.exr` writes

### Requirement: One session at a time

The server SHALL hold at most one session. `open_session` while a session is open
SHALL cancel the current session's render, and SHALL close the current session once
the new one is open. Its override layer is already saved, so nothing is lost. An
`open_session` that is refused or fails SHALL leave the current session open, its
undo history included.

#### Scenario: Switching stages

- **WHEN** the agent opens a second stage while a render is running on the first
- **THEN** the first render is cancelled, its layer stays on disk as last saved,
  and the second session opens

#### Scenario: A mistyped stage

- **WHEN** the agent calls `open_session` with an input that does not exist while
  a session is open
- **THEN** the call fails, and the open session still answers its tools, `undo`
  included

### Requirement: Querying an attribute nothing authors

`query` on an attribute that a schema of its prim declares and that no layer authors
SHALL answer the declared type, the schema's fallback value, and `source`
`"fallback"`. `query` on an attribute that no schema declares and no layer authors
SHALL fail, saying so and naming `set_attribute` with an explicit `type` as the way to
author it.

#### Scenario: A light's unauthored exposure

- **WHEN** no layer authors `/scene/Sky.inputs:exposure` on a DomeLight and the agent
  queries it
- **THEN** the answer has `type` `"float"`, `value` `0` and `source` `"fallback"`

#### Scenario: A name no schema declares

- **WHEN** the agent queries `/scene/Sky.inputs:nonsense`
- **THEN** the call fails, naming `set_attribute` with an explicit `type`
