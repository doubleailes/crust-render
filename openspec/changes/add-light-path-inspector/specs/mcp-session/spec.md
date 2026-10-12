# Spec Delta

## MODIFIED Requirements

### Requirement: An MCP server over stdio

`crust mcp` SHALL run a Model Context Protocol server on stdin/stdout until stdin
closes. stdout SHALL carry only protocol messages. The log SHALL go to stderr, and no
progress bar SHALL be drawn.

#### Scenario: Listing tools

- **WHEN** an MCP client initialises the server and asks for its tools
- **THEN** it receives `open_session`, `query`, `check`, `set_attribute`,
  `set_variant`, `set_active`, `bind_material`, `author_usda`, `undo`, `render`,
  `snapshot`, `cancel`, `probe`, `light_paths`, `diff` and `render_final`, each
  with a JSON input schema

#### Scenario: Nothing but protocol on stdout

- **WHEN** a session imports a stage that raises warnings
- **THEN** every stdout line parses as a protocol message, and the warnings appear
  on stderr

## ADDED Requirements

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
