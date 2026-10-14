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
  `render`, `snapshot`, `cancel`, `probe`, `diff` and `render_final`, each with a
  JSON input schema

#### Scenario: Nothing but protocol on stdout

- **WHEN** a session imports a stage that raises warnings
- **THEN** every stdout line parses as a protocol message, and the warnings appear
  on stderr

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

## ADDED Requirements

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

#### Scenario: A changed source that no longer imports

- **WHEN** another program rewrites a source layer so that it no longer parses, and
  the agent calls `render`
- **THEN** the call fails, saying the import failed and naming the layer, and
  the next tool call that reads the scene tries the import again

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
