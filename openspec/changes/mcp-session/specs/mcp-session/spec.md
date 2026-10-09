# Spec Delta

## Purpose

`crust mcp` lets an agent in Claude Desktop keep a USD stage open, edit it through
composition, look at progressive renders and measure them. The session's result is
written to disk as an override layer that sublayers the original, plus the final
render made from that layer.

## ADDED Requirements

### Requirement: An MCP server over stdio

`crust mcp` SHALL run a Model Context Protocol server on stdin/stdout until stdin
closes. stdout SHALL carry only protocol messages. The log SHALL go to stderr, and no
progress bar SHALL be drawn.

#### Scenario: Listing tools

- **WHEN** an MCP client initialises the server and asks for its tools
- **THEN** it receives `open_session`, `query`, `check`, `set_attribute`,
  `set_variant`, `set_active`, `bind_material`, `author_usda`, `undo`, `render`,
  `snapshot`, `cancel`, `probe`, `diff` and `render_final`, each with a JSON input
  schema

#### Scenario: Nothing but protocol on stdout

- **WHEN** a session imports a stage that raises warnings
- **THEN** every stdout line parses as a protocol message, and the warnings appear
  on stderr

### Requirement: Opening a session creates an override layer

`open_session` SHALL take an input stage and an output path. When `output` does not
exist, it SHALL write a USD layer there whose only composition arc is a sublayer on
the input, open the stage on that layer, and import it. It SHALL refuse an `output`
that is one of the input's own layers.

#### Scenario: A new session

- **WHEN** `open_session` is called with `input = shot.usda` and
  `output = shot_lookdev.usda`
- **THEN** `shot_lookdev.usda` exists, holds `subLayers = [@./shot.usda@]` and no
  prim opinion, and the result reports the camera, resolution and warning count

#### Scenario: Output is a source layer

- **WHEN** `output` names the input itself or a layer it composes
- **THEN** the call fails and no file is written

### Requirement: Resuming a session

When `output` already exists and is an override layer that sublayers `input`,
`open_session` SHALL open it and resume, keeping its opinions. When `output` exists
but does not sublayer `input`, the call SHALL fail without writing.

#### Scenario: Resuming yesterday's work

- **WHEN** `open_session` is called again with the same `input` and `output` after
  edits were made in an earlier session
- **THEN** the stage composes those edits, and the first `render` shows them

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
An attribute value SHALL take the attribute's declared type. A value that does not
convert to that type SHALL be refused without authoring anything.

#### Scenario: Dimming a light

- **WHEN** the agent calls `set_attribute` on `/lights/key` `inputs:exposure` with
  `-0.5`
- **THEN** the override layer holds `over "lights" { over "key" { float
  inputs:exposure = -0.5 } }` (in its own syntax), and the source layers are
  unchanged

#### Scenario: A value of the wrong type

- **WHEN** the agent sets `inputs:exposure` to `"bright"`
- **THEN** the call fails, naming the declared type, and the layer is unchanged

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

After each edit tool call, the session SHALL save the override layer, then import
the saved layer as `crust render <output>` would. A call's result SHALL report the
warning codes the import gained or lost, and the import time.

#### Scenario: An edit that introduces a warning

- **WHEN** the agent authors a `RectLight` with `width = 0`
- **THEN** the result lists `light.degenerate_shape` as gained, and the file on disk
  already holds the light

### Requirement: Undo

`undo` SHALL restore the override layer to its content before the most recent edit
batch, save it, and re-import it. Undo history SHALL last for the session. After a
resume, undo SHALL NOT reach edits made in earlier sessions.

#### Scenario: Undoing an exposure change

- **WHEN** the agent sets an exposure, then calls `undo`
- **THEN** the override layer's content is equal to its content before the set, and
  the next render equals the render before it, bit for bit

### Requirement: Time-bounded progressive renders

`render` SHALL start a progressive render of the current scene and return within
`budget_s` seconds. It SHALL return:

- a PNG of the latest snapshot, downscaled to at most 1024 pixels on its long side;
- the samples per pixel reached;
- whether the render is done;
- a render id.

The render SHALL keep refining after the call returns, until it completes or is
cancelled.

#### Scenario: A long render

- **WHEN** the agent calls `render` with `budget_s = 20` on a frame that needs
  minutes
- **THEN** the call returns after at most about 20 seconds with an image and
  `done = false`, and a later `snapshot` returns a newer image

#### Scenario: A render of a region

- **WHEN** `render` is given a region
- **THEN** only that region is rendered, and the image covers only it

### Requirement: Snapshot and cancel

`snapshot(render_id)` SHALL return the latest image and progress without waiting.
`cancel(render_id)` SHALL stop the render and keep what it has done. An edit, or a
new `render`, SHALL cancel any render still running.

#### Scenario: Editing during a render

- **WHEN** a render is refining and the agent calls `set_attribute`
- **THEN** that render is cancelled before the scene is re-imported, and its id
  reports `cancelled`

### Requirement: Renders are reproducible

A session render with the same layer content, region and sample count SHALL be
bit-identical to `crust render <output>` with the same region and samples.

#### Scenario: Agreeing with the CLI

- **WHEN** a session renders at 16 spp to completion, and the user runs
  `crust render -i shot_lookdev.usda -s 16`
- **THEN** `crust diff` reports the two images identical

### Requirement: Probing a pixel

`probe(render_id, x, y)` SHALL return the pixel's beauty and AOV values in the
render's linear working space. When the prim-path table is available, it SHALL also
return the prim the camera ray first hits there and the material bound to it.

#### Scenario: What is this grey thing?

- **WHEN** the agent probes a pixel on a mesh
- **THEN** the result names the mesh's prim path and its bound material

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
SHALL close the current one first, cancelling its render. Its override layer is
already saved, so nothing is lost.

#### Scenario: Switching stages

- **WHEN** the agent opens a second stage while a render is running on the first
- **THEN** the first render is cancelled, its layer stays on disk as last saved,
  and the second session opens
