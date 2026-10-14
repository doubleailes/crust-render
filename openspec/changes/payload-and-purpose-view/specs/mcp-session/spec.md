# Spec Delta

## MODIFIED Requirements

### Requirement: An MCP server over stdio

`crust mcp` SHALL run a Model Context Protocol server on stdin/stdout until stdin
closes. stdout SHALL carry only protocol messages. The log SHALL go to stderr, and no
progress bar SHALL be drawn.

#### Scenario: Listing tools

- **WHEN** an MCP client initialises the server and asks for its tools
- **THEN** it receives `open_session`, `query`, `check`, `set_attribute`,
  `set_variant`, `set_active`, `bind_material`, `author_usda`, `undo`, `reload`,
  `load`, `unload`, `set_view`, `render`, `snapshot`, `cancel`, `probe`,
  `light_paths`, `diff` and `render_final`, each with a JSON input schema

#### Scenario: Nothing but protocol on stdout

- **WHEN** a session imports a stage that raises warnings
- **THEN** every stdout line parses as a protocol message, and the warnings appear
  on stderr

### Requirement: Opening a session creates an override layer

`open_session` SHALL take an input stage and an output path. When `output` does not
exist, it SHALL write a USD layer there whose only composition arc is a sublayer on
the input, open the stage on that layer, and import it under the session's view. It SHALL
refuse an `output` that is one of the input's own layers, and one that is not a `.usda`
text layer. An optional `view` argument SHALL name the view to open in, `decision` (the
default) or `final`.

#### Scenario: A new session

- **WHEN** `open_session` is called with `input = shot.usda` and
  `output = shot_lookdev.usda`
- **THEN** `shot_lookdev.usda` exists, holds `subLayers = [@./shot.usda@]` and no
  prim opinion, and the result reports the camera, resolution, warning count and view

#### Scenario: Output is a source layer

- **WHEN** `output` names the input itself or a layer it composes
- **THEN** the call fails and no file is written

#### Scenario: Output is not a text layer

- **WHEN** `output` is `shot_lookdev.usdc`
- **THEN** the call fails, saying the layer must be a `.usda`, and no file is
  written

#### Scenario: A heavy stage opens without its payloads

- **WHEN** `open_session` is called on a stage whose assets bring their geometry through
  payloads and publish proxies beside them
- **THEN** no payload layer is opened, the result reports the `decision` view, and the
  first `render` shows the proxies

#### Scenario: Opening in the final view

- **WHEN** `open_session` is called with `view = "final"`
- **THEN** every payload is loaded, the stage's own purposes and binding purposes apply,
  and the session renders what `crust render <output>` renders

### Requirement: Each edit batch is saved and imported

After each edit tool call, the session SHALL save the override layer, then bring the
scene up to date with the saved layer, either in place or by importing it as
`crust render <output>` with the view's flags would. A call's result SHALL report the
warning codes the scene gained or lost, the time taken (`import_s`), and how the edit
was applied (`applied`: `settings`, `lights`, `materials` or `full`).

#### Scenario: An edit that introduces a warning

- **WHEN** the agent authors a `RectLight` with `width = 0`
- **THEN** the result lists `light.degenerate_shape` as gained, the result reports
  `applied = full`, and the file on disk already holds the light

#### Scenario: Dimming a light is applied in place

- **WHEN** the agent sets `inputs:intensity` on an existing `RectLight`
- **THEN** the result reports `applied = lights`

#### Scenario: An edit under the decision view

- **WHEN** the session is in the `decision` view and the agent moves an asset's root
  prim
- **THEN** the re-import loads no payload the view leaves unloaded, and the next render
  shows the asset's proxy at its new place

### Requirement: Renders are reproducible

A session render with the same layer content, the same source layers on disk, the same
view, the same region and the same sample count SHALL be bit-identical to
`crust render <output>` with the view's flags, the same region and the same samples.

#### Scenario: Agreeing with the CLI

- **WHEN** a session in the `final` view renders at 16 spp to completion, and the user
  runs `crust render -i shot_lookdev.usda -s 16`
- **THEN** `crust diff` reports the two images identical

#### Scenario: Agreeing with the CLI after a source changed

- **WHEN** another program rewrites a layer the input composes, then a session in the
  `final` view renders at 16 spp to completion, and the user runs
  `crust render -i shot_lookdev.usda -s 16`
- **THEN** `crust diff` reports the two images identical

#### Scenario: Agreeing with the CLI in the decision view

- **WHEN** a session in the `decision` view, with `/World/hero` loaded, renders at 16
  spp to completion, and the user runs `crust render -i shot_lookdev.usda -s 16`
  followed by the `flags` the render's result reported
- **THEN** `crust diff` reports the two images identical

### Requirement: The final render

`render_final` SHALL save the override layer and render it exactly as
`crust render <output>` would, whatever the session's view. Product paths SHALL resolve
against the override layer's directory, and a stage without products SHALL write
`<output stem>.exr` and its PNG beside the layer. The result SHALL list the files
written.

#### Scenario: Ending a session

- **WHEN** the agent calls `render_final` on `work/shot_lookdev.usda`, which
  authors no product
- **THEN** `work/shot_lookdev.exr` and `work/shot_lookdev.png` exist, and are
  identical to what `crust render -i work/shot_lookdev.usda -o
  work/shot_lookdev.exr` writes

#### Scenario: The view does not reach the final render

- **WHEN** the session is in the `decision` view with most payloads unloaded, and the
  agent calls `render_final`
- **THEN** every payload is loaded and the stage's own purposes and binding purposes
  apply, and the session's view is unchanged afterwards

## ADDED Requirements

### Requirement: The session holds a view

A session SHALL hold a view: which payloads are loaded, which purposes are drawn and
which binding purposes resolve materials. The `decision` view SHALL load no payload and
draw `render` and `proxy` with `preview`, then all-purpose, bindings. The `final` view
SHALL load every payload and use the stage's own purposes and binding purposes. The
view SHALL NOT be written to any layer, and `undo` SHALL NOT change it.

#### Scenario: The view is not an opinion

- **WHEN** the agent changes the view, then reads the override layer
- **THEN** the layer's bytes are unchanged by the view change

#### Scenario: Undo leaves the view alone

- **WHEN** the agent loads `/World/hero`, sets an exposure, then calls `undo`
- **THEN** the exposure is undone and `/World/hero` stays loaded

#### Scenario: Resuming starts from the opening view

- **WHEN** a session that had `/World/hero` loaded is closed, and `open_session` resumes
  its override layer without a `view` argument
- **THEN** the session is in the `decision` view with no payload loaded

### Requirement: Loading and unloading payloads

`load(prims)` and `unload(prims)` SHALL load or unload the payloads at and below each
prim, with USD load semantics, then import the saved layer under the new view, cancelling
any running render. A prim the stage does not compose SHALL be refused, and so SHALL an
import that fails: in both cases the view and the scene stay as they were.

#### Scenario: Judging the hero asset

- **WHEN** the session is in the `decision` view and the agent calls
  `load(["/World/hero"])`
- **THEN** `/World/hero`'s payload is loaded, the next render draws its render geometry
  instead of its proxy, and no other payload is loaded

#### Scenario: Putting it back

- **WHEN** the agent then calls `unload(["/World/hero"])`
- **THEN** the next render draws `/World/hero`'s proxy again, bit-identical to a render
  before the load

#### Scenario: Loading what a probe hit

- **WHEN** the agent probes a pixel on an unloaded asset's proxy and calls `load` with
  the prim the probe named
- **THEN** the asset's payload, the one on the prim's ancestry, is loaded, and the next
  render draws the asset's render geometry there

#### Scenario: A mistyped prim

- **WHEN** the agent calls `load(["/World/heor"])`
- **THEN** the call fails, naming the prim, and the view and the scene are unchanged

### Requirement: Choosing what the session draws

`set_view` SHALL take a `preset` (`decision` or `final`), which sets the whole view, or
`purposes` and `binding_purposes`, which replace those parts of the view; `null` for
either SHALL mean the stage's own value. It SHALL import the saved layer under the new
view, cancelling any running render. An unknown purpose or binding purpose SHALL be
refused, leaving the view as it was.

#### Scenario: Judging a loaded asset's look

- **WHEN** `/World/hero` is loaded and the agent calls
  `set_view(binding_purposes = ["full", "all"])`
- **THEN** the next render shows `/World/hero` with its `full` material

#### Scenario: Back to the final view

- **WHEN** the agent calls `set_view(preset = "final")`
- **THEN** every payload is loaded, and the next render to completion is bit-identical
  to `crust render <output>` with the same region and samples

### Requirement: Results report the view

The results of `open_session`, `load`, `unload`, `set_view`, `render`, `snapshot` and
`check` SHALL carry the view: its preset when it is one, the payloads loaded and
unloaded, the purposes and binding purposes, and `flags`, the `crust render` arguments
that reproduce it. A `render` or `snapshot` SHALL report the view the render was made
under.

#### Scenario: Reproducing a session image

- **WHEN** the agent renders in the `decision` view after `load(["/World/hero"])`
- **THEN** the result's `flags` are `--unloaded --load /World/hero --purposes
  render,proxy --binding-purposes preview,all`

#### Scenario: The final view needs no flags

- **WHEN** the agent renders in the `final` view
- **THEN** the result's `flags` are empty

### Requirement: Authored purposes re-import

An edit to `includedPurposes` or `materialBindingPurposes` on the render settings prim
that changes what the session draws SHALL bring the scene up to date as a full import
would, never in place. Where the view sets its own purposes or binding purposes, the
edit SHALL NOT change the session's render, the result SHALL say the view overrides it,
and `render_final` SHALL use the edited value.

#### Scenario: Drawing proxies in the final render

- **WHEN** the session is in the `final` view and the agent sets the render settings'
  `includedPurposes` to `["default", "proxy"]`
- **THEN** the result reports `applied = full`, and the next render shows the proxies

#### Scenario: An authored purpose under the decision view

- **WHEN** the session is in the `decision` view and the agent sets the same attribute
- **THEN** the session's next render is unchanged, the result says the view overrides
  `includedPurposes`, and `render_final` shows the proxies
