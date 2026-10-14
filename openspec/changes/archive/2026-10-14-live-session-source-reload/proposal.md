# Proposal

## Why

A `crust mcp` session should be able to render a stage that another tool keeps
rewriting. An example is a scene-assembly agent such as BowerBot, running as a second MCP
server in the same client. Today the session misses those changes:

- `FileAssets::revalidate()` stamps decoded textures, Ptex, light textures and IES
  profiles, but not the USD layers the stage composes.
- The edit classifier (`mcp/delta.rs`) compares only the override layer, so an edit
  made after a source layer changed on disk can be applied in place over a stale scene.
- The authoring stage keeps the composition it opened with, so `query` answers from
  the old layers.
- No tool re-imports without an edit.

## What Changes

- The session records the length and modification time of every layer the stage
  composes: the input, its sublayers, references and payloads, each tracked with a
  stamp, as kept decodes are.
- Before every edit, undo or render, a moved stamp reopens the authoring stage and
  makes the next import `full`. The in-place classes are never applied over a changed
  source.
- A new `reload` tool re-imports the saved override layer on demand. It reports which
  source layers changed, and the warning codes gained or lost, as an edit's result does.
- The undo history survives a reload. Undo restores the override layer's bytes and
  composes them over the sources as they are now.

The override layer remains the only layer the session writes.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `mcp-session`:
  - The tool list gains `reload`.
  - A new requirement: a source layer changed on disk is picked up by the next edit,
    undo, render or `reload`, never rendered stale.
  - The "Renders are reproducible" requirement then holds against the sources as they
    are on disk at render time.

## Impact

- `crates/crust-render/src/mcp/`:
  - `session.rs`: the layer stamps, and reopening the authoring stage;
  - `delta.rs`: a moved stamp forces `full`;
  - `mod.rs`: the `reload` tool;
  - `tests.rs`: an external rewrite of a sublayer, then an in-place-class edit, must
    match a fresh session bitwise.
- `crust-core`: the import may need to report the layers it composed, if the stage
  does not expose them.
- Docs:
  - `openspec/specs/mcp-session/design.md`: an "Edits applied in place" section on
    source stamps;
  - `site/content/docs/help/claude-desktop.md`: the `reload` tool, and using the session
    beside another authoring server.
- No change to `crust render` output: goldens unaffected.
- Out of scope: `metersPerUnit` handling. It changes renders and is its own change.
