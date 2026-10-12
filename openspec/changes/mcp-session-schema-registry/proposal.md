## Why

An agent in a `crust mcp` session cannot see the type or the fallback of a schema
attribute that no layer authors. Querying a DomeLight's `inputs:exposure` is refused
("no layer authors …"), and `set_attribute` needs an explicit `type` the first time it
sets one. This was a known gap while openusd-schemas shipped no schema data
(`openspec/changes/archive/2026-10-09-mcp-session/design.md`, "No schema data reaches
the authoring stage"). openusd 0.8 (#280) ships it, and the render's import already
registers it (`usd_import::stage_builder()`). Only the session's authoring stage
(`open_stage` in `crates/crust-render/src/mcp/session.rs`) is still opened without it.

## What Changes

- The session opens its authoring stage with the same schema registry as the render's
  import, so an attribute that a prim's schemas declare has its declared type and
  fallback even when no layer authors it.
- `query` on such an attribute answers its type, its fallback value and
  `"source": "fallback"` instead of an error. Only an attribute that no schema declares
  and no layer authors is still refused.
- `set_attribute` on a schema-declared attribute takes its type from the schema, so
  `type` can be left out. A `type` that differs from the schema's is refused, as one
  that differs from an authored declaration already is.
- `query` on a prim keeps listing only the authored attributes (see design D3).
- The limitation goes away from the user docs (`help/claude-desktop.md`,
  `architecture/limitations.md`).

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `mcp-session`: unauthored schema attributes have a type and a fallback in `query` and
  `set_attribute`.

## Impact

- `crust-render`: `mcp/session.rs` (`open_stage`), `mcp/query.rs` (the error text that
  remains), `tests/mcp.rs`.
- `crust-core`: makes its schema registry public (design D1), so the session uses the
  same families as the import.
- No new dependency and no change to renders: the import already registers the
  registry, so no image changes. Depends on #280 (openusd 0.8) landing first.
