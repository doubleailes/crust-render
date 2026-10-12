## Context

See `proposal.md` for why. What shapes the approach:

- **Two stages per session.** The session authors through its own `Stage`
  (`open_stage`, payloads unloaded, no resolver override) and renders through
  crust-core's import, which opens its own stage with `usd_import::stage_builder()`.
  Only the import's stage registers `openusd_schemas::schema_registry()` today.
- **openusd resolves a declared attribute through the registry.** With a registry on
  the stage, `Attribute::type_name` takes the schema's `typeName` first and the
  strongest composed opinion second (openusd `usd/attribute.rs`,
  `declared_type_token`), and `Attribute::get` falls back to the schema's fallback
  value (ROADMAP §12.2.8, since 0.8.0). Both are what `query` and `set_attribute`
  already call; neither needs new code to use the registry.
- **The families are chosen by Cargo features.** crust-core enables openusd-schemas'
  `geom`, `lux`, `shade`, `render`, `skel`, `vol`, `proc` and `media`. crust-render
  names openusd-schemas only as a dev-dependency, with `geom`.
- **`set_attribute` already handles a declared type.** It takes the declared type,
  refuses a different explicit `type`, and asks for one only when nothing declares the
  attribute (`session.rs`, `set_attribute`).

## Goals / Non-Goals

**Goals:**

- The session's stage answers "what type is this attribute, and what does it read as"
  the same way the render's stage does.
- No change to any render, and no new dependency.

**Non-Goals:**

- Listing schema-declared attributes in a prim `query` (D3).
- Changing what the import reads or its own defaults.
- Validating an authored value against a schema's `allowedTokens`.

## Decisions

### D1. crust-core exposes the registry; the session does not name openusd-schemas

crust-core adds a public `crust_core::usd_schema_registry()` (a thin wrapper over
`openusd_schemas::schema_registry()`, which `stage_builder()` then calls too).
`open_stage` passes it to `Stage::builder().schema_registry(..)`.

*Alternative rejected:* making openusd-schemas a regular dependency of crust-render's
`mcp` feature. It would build, since Cargo unifies features with crust-core's, but the
session's families would then depend on that unification instead of being the import's
by construction. The two lists would also have to be kept in step by hand.

### D2. Only the registry, not the import's resolver

The session keeps its own resolver. The import's memory-mapped resolver
(`CRUST_USD_MMAP`) is a read-throughput choice. The session's stage is small, payloads
unloaded, and it writes its override layer by rename (`session.rs`, `replace_file`).
Mapping that layer adds nothing and only gives the rename something to reason about.

### D3. A prim `query` keeps listing authored attributes only

The archived design expected `query` to move from `authored_attributes()` to
`attributes()` once the registry arrived. That is not taken: a light's schemas declare
some 30 properties, and a material many more. Listing them all would bury the handful
the scene authors in every prim answer an agent reads. An agent that wants a declared
attribute queries it by path, which now works. Revisit if agents are seen guessing
attribute names.

### D4. The error that remains names the real cause

With the registry, `query` refuses an attribute only when no schema of the prim
declares it and no layer authors it. The error says that and still points at
`set_attribute` with an explicit `type`, which is the way to author a custom attribute.

## Risks / Trade-offs

- **The schema's declared type now wins over an authored one.** A source layer that
  authors `double inputs:exposure` reads as `float` with the registry, as in C++ USD.
  `set_attribute` then authors a `float`, and an explicit `type="double"` is refused.
  This matches USD; the tests should pin it so it is not mistaken for a regression.
- **The fallback `query` shows must be the one the render uses.** The import reads
  attributes through the same `get`, on a stage with the same registry, so it sees the
  same fallback wherever it does not substitute its own default (`lights.rs`
  `light_inputs`: `intensity` 1, `exposure` 0). Task 2.3 checks a light whose UsdLux
  fallback could differ from crust's default (DistantLight `inputs:intensity`). Any
  difference is recorded in the `mcp-session` design record rather than left
  surprising.
- **Cost.** `schema_registry()` is built once per process and shared; the import
  already pays for it. Opening the session's stage adds a pointer copy.
