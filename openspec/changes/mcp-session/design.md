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

### D3. Edit tools map onto openusd authoring; `author_usda` goes through a scratch layer

- **`set_attribute`:** reads the composed attribute's `typeName` and converts the
  JSON value to it (number → `float`/`double`/`int`; array of 3 → `color3f`/`float3`/
  `point3f`, …). An undeclared attribute requires an explicit `type`.
- **`set_variant`, `set_active` and `bind_material`:** author variant selection,
  `active` and the `material:binding` relationship.
- **`author_usda(text)`:** parses the text into an anonymous layer and refuses it if
  it carries layer metadata that changes composition of the root (`subLayers`).
  It then copies its prim specs into the override layer with openusd's spec
  copy (`sdf/copy.rs`). That is one transaction, so one undo step and one re-import.

All edits author `over`s unless the snippet says `def` or `class`. Asset-valued
inputs are re-anchored per D5.

### D4. Undo is a stack of layer texts

Before each batch, `export_to_string` of the override layer is pushed onto a stack.
`undo` pops it, replaces the layer's content, saves and re-imports. Override layers
are small (opinions only), so this is cheap and exact. Byte-equality of the content
is the spec's test. The stack does not survive the process. A resumed session starts
with an empty stack, and that is stated in the spec.

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

`render` takes the render-only knobs (region, spp, AOVs to retain) as arguments.
Everything else that shapes the render is an opinion in the layer (the
`RenderSettings` prim), so the final render needs no flags to reproduce it.

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

## Migration Plan

Additive: a new subcommand and feature. Nothing changes for `render`, `check`, `diff`
or `diagnostic`. Rollback is disabling the feature or reverting.

## Open Questions

- The exact Desktop extension manifest fields, and how to sign the bundle per
  platform. These are decided at packaging time without changing the server.
