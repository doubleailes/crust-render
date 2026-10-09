# Design

## Context

See proposal.md (Why). These facts about the current code shape the design:

- **The import opens its own stages from a path.** `load_scene` opens an index stage
  (payloads unloaded), then, on stages large enough to stream, one masked stage per
  top-level subtree, which it drops as it goes (`usd-scene-import` design §
  Streaming import). Edits held only in an in-memory session layer would therefore
  be invisible to every stage the import opens.
- **openusd 0.7 can author.** It has `set_edit_target`, `override_prim`,
  `Attribute::set`, a `NamespaceEditor`, `StageSink::after_commit` (resynced vs
  changed-info paths), and `Layer::save` / `export` / `export_to_string`. Its
  `Stage` uses `RefCell` and `Rc` internally: it is neither `Send` nor `Sync`.
- **One import can serve many renders.** `crust diagnostic` already renders one
  imported `Scene` dozens of times with different settings and crops.
- **Not built yet:**
  - progressive snapshots and cancellation (`progressive-cancellable-render`,
    0/20 tasks);
  - the geom id → prim path table (`add-identity-aovs-openexrid`, 0/8);
  - `crust check` (`add-crust-check`).
- **Release binaries.** `nightly.yml` builds release binaries for Linux, Windows
  (`x86_64-pc-windows-msvc`) and macOS (two targets).
- **stdout discipline.** The CLI already moves the log to stderr when stdout carries
  a report. `--stats-json -` is the precedent.

## Goals / Non-Goals

**Goals:**

- The file on disk *is* the session. What is rendered is always the saved override
  layer, imported by the unchanged `load_scene`. "Renders like `crust render
  <output>`" therefore holds by construction, and the result is pinned bitwise by a
  test.
- No new USD semantics. Every edit is a USD opinion, and crust interprets it through
  the same import as any other stage.
- Tools that never block Desktop for a whole render.
- A session API in crust-core that phase 3 (`SceneEdit`/`LiveScene`, and later
  Hydra) can extend without changing the tools.

**Non-Goals:**

- Incremental sync (phase 3). Every edit batch is a full re-import.
- Several concurrent sessions, network transports, or authentication. The client is
  Claude Desktop on the same machine.
- Moana-scale stages in a session (see Risks).
- A viewport or GUI.

## Decisions

### D1. The working layer is a real file, and the import reads that file

`open_session` writes `output` first: `#usda 1.0`, `subLayers = [@<input relative to
output's directory>@]`, nothing else. It then opens an authoring stage on `output`
with its root layer as the edit target. Each edit batch authors into that layer,
calls `Layer::save`, and calls `load_scene(output)`.

Because the import composes from the saved file:

- the streaming import's masked stages see every edit, with no special case;
- the file a user opens later is exactly what was rendered;
- a crash loses at most the batch in flight.

*Alternatives considered:*

- **An in-memory session layer, exported at the end.** Invisible to the streaming
  import's re-opened stages, and the export could diverge from what was rendered.
- **Teaching `load_scene` to accept an open `Stage`.** It would bypass streaming,
  which is the very thing that makes large stages fit in memory. It also creates a
  second import entry point that can drift from the first.

### D2. The session thread owns the authoring stage; tokio only carries the protocol

`Stage` is neither `Send` nor `Sync`, so one dedicated session thread owns:

- the authoring `Stage`;
- the undo history;
- the current `Arc<Scene>`.

The MCP SDK's async handlers send typed commands over a channel and await replies.
Renders run on the rayon pool against an `Arc<Scene>` clone, through the render
control handle that `progressive-cancellable-render` provides. Swapping the scene
after an edit:

1. cancel the running render;
2. join it;
3. import;
4. replace the `Arc`.

*Alternatives considered:*

- **A hand-rolled JSON-RPC loop instead of the SDK.** It needs no tokio, but MCP's
  protocol revisions (capabilities, content types) would become ours to track. The
  SDK lives behind the `mcp` cargo feature, so builds without it stay as they are.

### D3. Every edit is a USDA snippet merged into the override layer

`author_usda` takes a snippet as it is. The other edit tools write their own and go
through the same path (`mcp/edit.rs`):

- **`set_attribute`:** reads the composed attribute's `typeName` and variability, and
  writes the JSON value as a USDA literal of that type (number → `float`/`double`/`int`;
  array of 3 → `color3f`/`float3`/`point3f`, …), refusing a value of the wrong shape
  with the type's name. An undeclared attribute requires an explicit `type`.
- **`set_variant`, `set_active` and `bind_material`:** an `over` carrying the
  variant selection, `active`, or `MaterialBindingAPI` and `material:binding`.

The snippet is parsed by openusd's own USDA parser (`usda::parse`), which types every
value, so an opinion means in the session what it means in any USD tool. A snippet that
authors `subLayers` or `subLayerOffsets` is refused. The merge into the layer is one
`Stage::batch_edit`, so one undo step and one re-import, and works field by field, as a
stronger layer sits over a weaker one: a field the snippet authors replaces the layer's,
the namespace-children lists are unioned (authoring one prim or property keeps its
siblings), a variant selection or a dictionary merges key by key, a token or string list
op (`apiSchemas`, `variantSetNames`) merges item by item unless either side is explicit
(so `bind_material`'s `prepend apiSchemas = ["MaterialBindingAPI"]` keeps a schema an
earlier edit applied), and an `over` never downgrades a `def` or a `class` already
there. The other list ops (references, payloads, relationship targets) are replaced as
a whole: `bind_material` rebinds.

*Alternative rejected:* openusd's spec copy (`sdf/copy.rs`) **replaces** the destination
subtree, so a second edit of a prim would erase the first's opinions.

All edits author `over`s unless the snippet says `def` or `class`. Asset-valued
inputs are re-anchored per D5. A prim inside an instance (an instance proxy) is refused:
USD reads its opinions from the prototype only.

**No schema data reaches the authoring stage.** openusd 0.7 accepts a registry a
caller builds (`SchemaRegistry::builder().family(..)`, `StageBuilder::schema_registry`),
but neither openusd 0.7 nor openusd-schemas 0.7 ships the families' data, and
`SchemaRegistry::global` registers none. So an attribute no layer authors, such as a
light's `inputs:exposure`, has no type or fallback on the authoring stage: `query` says
so, and `set_attribute` needs its `type` once. (crust-core's import does not read schema
fallbacks either: it has its own defaults, such as `inputs:intensity = 1` for every
light, where UsdLux gives a DistantLight 50000.)

openusd-schemas' `main` (unreleased; v0.7.0 of 2026-09-05 is the latest tag) vendors
OpenUSD's `schema.usda` files and exposes `openusd_schemas::schema_registry()`, which
fixes this with one line in `open_stage`. It is not taken now, because its API is not
0.7's (`SchemaFamily`, renamed views, `<Class>Schema` traits): moving to it is a
whole-crust migration, and on `main` a typed view answers `None` on a stage opened
without the registry, so crust-core must pass it at every stage it opens, which moves
renders wherever a schema fallback differs from crust's default. That belongs in its own
change, with re-recorded goldens, when the release lands; the session's part of it is
`open_stage` taking the registry, and `query` listing `attributes()` rather than
`authored_attributes()`.

*Alternatives considered:*

- **Registering the families ourselves** from OpenUSD's `generatedSchema.usda` with the
  0.7 API. Probed: the five families crust reads (`usd`, `usdGeom`, `usdLux`,
  `usdShade`, `usdRender`) parse and register (73 schemas, ~12 ms), and on the session's
  own stage shape an unauthored DomeLight `inputs:exposure` reads as `float`, fallback 0.
  Rejected for now: it vendors OpenUSD's files and licence notices, needs a manifest per
  family generated from `plugInfo.json` or `schema.usda`, and `main` has already replaced
  the `FamilySource` API it is written against.
- **A hand-written (schema, property) → type table** from openusd-schemas' typed views,
  whose `create_*_attr` hold the types. Rejected: partial, hand-maintained, and
  thrown away at the migration.

### D4. Undo is a stack of layer files

Before each batch, the override layer's bytes on disk are pushed onto a stack. `undo`
writes the top entry back, reopens the authoring stage on the file and re-imports, and
pops the entry only once that import has succeeded: an undo whose restore or import
fails puts the layer's current bytes back and leaves the scene and the stack as they
were, so the layer, the scene and the history never disagree.
Override layers are small (opinions only), so this is cheap and exact: the file after
`undo` is byte-equal to the file before the edit, which is the spec's test. (The bytes
on disk rather than `export_to_string`: the first edit after `open_session` would
otherwise undo to openusd's formatting of the layer rather than the file as it was.)
The override layer is always a `.usda` (`open_session` refuses any other extension), a
text file of opinions a user can read and diff. An edit whose save or import fails is
undone the same way, before the tool answers. The stack does not survive the process. A
resumed session starts with an empty stack, and that is stated in the spec.

### D5. Path anchoring

- The sublayer path is written relative to `output`'s directory when both files are
  on the same volume, and absolute otherwise.
- A relative asset path given to an edit tool is interpreted relative to the
  **input** stage's directory, which is how the agent sees paths in `query` results.
  It is rewritten relative to `output`'s directory before authoring.
- An absolute path is kept as it is.

A test opens `output` from a different working directory, checks with `query` that
every asset resolves to the same file, and renders it.

### D6. Time-bounded progressive renders

`render` starts a render through the progressive render control, then blocks the
handler, not the session thread, for at most `budget_s`. It returns the latest
snapshot: tone-mapped with the same view transform as the CLI's PNG, downscaled
(box filter, long side ≤ 1024) and PNG-encoded as MCP image content. Renders keep an
id. The last 8 completed or cancelled renders are retained for `probe` and `diff`;
older ones are evicted, with their ids reported as `evicted`.

`render` takes the render-only knobs (region, spp) as arguments. Everything else that
shapes the render is an opinion in the layer (the `RenderSettings` prim), so the final
render needs no flags to reproduce it. A session render requests the AOVs of the stage's
RenderProducts, as `crust render` does, so its beauty is the CLI's by construction and
`probe` has every AOV a final render writes; no argument chooses them.

The session keeps one `Renderer` per import and reconfigures it per render
(`Renderer::reconfigure`, which `new` is), skipping the call when the settings are the
ones it has: reconfiguring rebuilds the light selection, and trains the `learned` one.
The budget counts from the call, so starting a render is inside it.

While a render runs, `spp_reached` is `RenderControl::samples_reached`: the last stage of
the first sweep every pixel has completed (1, 2, 4, … spp), which the render records
with one atomic store per stage. The adaptive rounds after it leave it there, since
pixels then differ; once the render returns, the result gives the fewest and most
samples any pixel took. `generation` (the control's) says whether an image is newer.

### D7. `probe` and `diff` read retained buffers

- **`probe`** returns the beauty and every AOV the render retained at that pixel.
  With the prim-path table from `add-identity-aovs-openexrid`, it also returns the
  hit prim and its bound material. Without the table, those fields are `null` and
  the result says why. So `probe` does not wait for that change, but its scenario
  does.
- **`diff`** runs the `image-comparison` code on two retained buffers, and refuses
  renders with different spp or regions, since comparing them would be misleading.

### D8. `render_final` reuses the CLI's output path

`render_final` cancels any session render and saves the layer. It then runs the same
code path as `crust render -i <output>`: `load_scene`, `select_products`, the
product writers and the PNG preview. Relative `productName`s resolve against the
layer's directory, and with no products the beauty goes to `<output stem>.exr`. To
make "exactly as the CLI" literal, the CLI's render-and-write body is factored into
one function that both call.

### D9. Packaging for Claude Desktop

A Desktop extension bundle (`.mcpb` manifest plus the per-platform `crust`
binaries from the nightly build) declares the `crust mcp` command. The docs page also
gives the manual `claude_desktop_config.json` entry. The bundle is built in
`nightly.yml` beside the existing binaries. It contains no secrets and needs no
network.

### D10. The seam phase 3 will widen

The session talks to crust-core through a small `Session` facade with three
operations:

- `reload(path) -> Scene` (today: `load_scene`);
- `render(&Scene, RenderRequest, &RenderControl)`;
- `write_outputs(...)`.

Phase 3 replaces `reload` with `apply(changes)` driven by `StageSink` change lists,
on a `LiveScene`. Hydra later calls the same `apply` with edits derived from its
scene delegate. The tools above do not change in either step.

## Risks / Trade-offs

- [Memory: the authoring stage and the import's own compositions coexist, roughly
  double a render's import peak] → acceptable for Desktop-sized stages. The authoring
  stage is opened with payloads unloaded, and loads them only for prims a `query`
  asks about. Moana-scale sessions are stated as a known gap until phase 3, where
  one composition serves both.
- [Re-import latency on medium stages (tens of seconds) makes the loop sluggish] →
  every edit result reports import time. `author_usda` batches several edits into
  one re-import. Phase 3 is the real fix.
- [openusd authoring bugs (it is younger than C++ USD) produce a layer that
  composes differently elsewhere] → a test round-trips every edit tool's output
  through a fresh `Stage::open` and compares composed values. Any openusd workaround
  is recorded in the `usd-scene-import` design record's openusd gaps.
- [The agent edits something crust does not read] → the edit's result reports the
  warning codes gained, and `check` explains them. Silence is not success.
- [`author_usda` used to pull in arbitrary files via references] → reading is
  allowed: references and payloads are ordinary composition, and nothing is written
  except the layer. Changing `subLayers` is refused, because it would redefine what
  the session is an override *of*.
- [Async SDK and tokio fail `cargo deny` or bloat the binary] → task 1.1 checks
  `cargo deny` before anything is built on them. The feature can be turned off.
- [Desktop's tool-call timeout is shorter than an import] → `open_session` and edit
  tools return as soon as the import completes. If imports routinely exceed the
  timeout, the import moves behind the same id/poll pattern as `render`. Task 6.1
  measures this against Desktop.

  Measured over the server's pipes (release build, Windows, 2026-10-09; the client
  `latency.py` of the change's session: open, one `set_attribute`, a `render` with
  `budget_s = 10`, `undo`):

  | stage | open_session | set_attribute | render | undo |
  |-------|--------------|---------------|--------|------|
  | `cornellbox.usda` (1.6 k triangles) | 0.05 s | 0.04 s | 9.9 s (128 spp, done) | 0.04 s |
  | `displacement.usda` (65 k triangles) | 0.09 s | 0.10 s | 0.33 s (done) | 0.08 s |

  Every call but `render` is its import, well inside any client timeout; `render` is
  bounded by its budget. No stage on the machine took more than 0.12 s to import, so a
  medium stage (Kitchen_set, tens of seconds) is still to be measured against Desktop.

  In Claude Desktop on Windows (2026-10-09), through the extension bundle built as
  `nightly.yml` builds it: every tool listed; the page's look-dev session on
  `cornellbox.usda` ran call for call with no timeout — imports 0.03–0.17 s, crop renders
  answered done in 0.45–1.4 s with the PNG shown inline, and `render_final` (128 spp, the
  full frame, blocking) answered after 12.3 s. The session left exactly the layer, its
  EXR and its PNG; the input was untouched. macOS is still to be tried.

## Migration Plan

Additive: a new subcommand and feature. Nothing changes for `render`, `check`, `diff`
or `diagnostic`. Rollback is disabling the feature or reverting.

## Open Questions

- The exact Desktop extension manifest fields, and how to sign the bundle per
  platform. These are decided at packaging time without changing the server.
