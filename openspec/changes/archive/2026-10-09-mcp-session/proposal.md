# Proposal

## Why

Through the CLI an agent can render and inspect a stage, but every question costs a
fresh import (minutes on ALab) and nothing persists between calls. An agent in Claude
Desktop that keeps a stage open could do look-dev and lighting by conversation:
query the stage, change it, render a crop, look at the result, and repeat. The
session would end with something a pipeline can use: a USD override layer that
sublayers the original, plus the final render.

openusd 0.7 already provides the pieces this needs: authoring, edit targets, change
notification and layer export. `progressive-cancellable-render` supplies the
non-blocking renders. This is also the first consumer of the live-session engine a
Hydra delegate will need later, so building it now exercises that API before any
FFI exists.

## What Changes

- **New subcommand `crust mcp`.** It is a Model Context Protocol server over stdio,
  meant to be registered in Claude Desktop. It keeps one session open at a time.
  stdout carries only the protocol; the log goes to stderr.
- **A session is an override layer on disk.** `open_session(input, output)` writes
  a new USD layer at `output` whose only composition arc is `subLayers = [<input>]`,
  and opens the stage on it.
  - Every edit is authored as opinions in that layer, and the layer is saved after
    each edit batch, so no work is lost and the file always opens in any USD tool.
  - Opening an existing override layer resumes its session.
  - Source layers are never written. The server writes nothing but the override
    layer, the final render, and the `.tx` files `--auto-tx` creates.
- **Composition is the editing model.** The edit tools author opinions:
  - `set_attribute`;
  - `set_variant` (variant selection);
  - `set_active`;
  - `bind_material`;
  - `author_usda`: an `over`/`def` snippet in USDA text, merged into the layer.

  `undo` restores the layer as it was before the last edit batch. Asset paths and
  the sublayer path are anchored to the override layer's own directory.
- **The scene follows the layer by a full re-import.** After each edit batch the
  saved layer is imported exactly as `crust render <output>` would. The tool's
  result reports the warning codes the edit added or removed. Incremental sync is
  out of scope here.
- **Look and measure.**
  - `query`: prims, attributes, and which layer an opinion comes from;
  - `check`: the `crust check` report;
  - `render(region, spp, budget_s)`: returns within a time budget with a downscaled
    PNG and its stats, while the render keeps refining; `snapshot` and `cancel`
    manage it;
  - `probe(x, y)`: per-pixel AOV values, and the prim under the pixel;
  - `diff`: compares two of the session's renders.
- **The session's result.** `render_final` renders the saved override layer exactly
  as `crust render <output>` would. It writes the EXR and the PNG preview, and every
  product with its `productName` resolved against the override layer's directory.
  The output name and the render settings are themselves opinions in the layer.

## Capabilities

### New Capabilities

- `mcp-session`: covers the `crust mcp` server and its session:
  - the protocol surface (stdio, the tool list, image results);
  - what a session writes, and what it guarantees never to write;
  - the override-layer contract (sublayers the original; saved per edit batch;
    resumable; paths anchored to its directory);
  - editing through composition, and undo;
  - re-import after each edit;
  - time-bounded progressive renders, probe and diff;
  - the final render.

### Modified Capabilities

- `cli`: the `Subcommands` requirement gains `mcp`.

## Impact

- **Depends on:**
  - `progressive-cancellable-render` (snapshots, cancellation);
  - `add-crust-check` and `structured-warnings` (the `check` tool, and warning codes
    in edit results);
  - the prim-path table from `add-identity-aovs-openexrid` (the prim under a pixel
    in `probe`).
- **crust-render:** the `mcp` subcommand behind a default-on cargo feature `mcp`,
  which brings in the Rust MCP SDK and an async runtime. A session thread owns the
  openusd `Stage`, which is not `Sync`. Renders run on the rayon pool. The server
  reuses `load_scene`, the product writers, `check` and `diff`.
- **crust-core:**
  - `load_scene` is unchanged: the session imports its saved layer by path, so the
    streaming import, which re-opens masked stages, sees every edit with no special
    case;
  - a small session API (render control handle, scene swap) shaped so that a later
    `SceneEdit`/`LiveScene` seam (phase 3, Hydra) can replace full re-import without
    changing the tools.
- **Dependencies:**
  - the MCP SDK and its async runtime must pass `cargo deny` (licences,
    advisories);
  - no `unsafe` is added.
- **Distribution:** the nightly workflow already builds Windows and macOS binaries.
  A Claude Desktop extension bundle and its install documentation are added.
- **Docs:**
  - a new `site/content/docs/usage/claude-desktop.md` (or similar): install, tools
    and a worked look-dev session;
  - `reference/command-line.md` (`mcp`);
  - `openspec/specs/cli/design.md`.
- **Performance:** `render` is unchanged. A session holds the composed stage and the
  imported scene at the same time, so its memory is roughly double a render's import
  peak. That is acceptable for the stages Desktop users open, and stated as a known
  gap for Moana-scale stages.
