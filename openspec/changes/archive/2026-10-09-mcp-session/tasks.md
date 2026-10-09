# Tasks

Prerequisites:
- `progressive-cancellable-render` must have landed: render control, snapshots and cancel.
- `add-crust-check` (and through it `structured-warnings`) must have landed.
- `probe`'s prim fields also need the prim-path table task of `add-identity-aovs-openexrid`. Until then they are `null` (D7).

## 1. Server skeleton

- [x] 1.1 Choose the Rust MCP SDK and runtime, add them behind a default-on `mcp` feature of crust-render, and verify that `cargo deny --locked check` passes and that `cargo build --release --no-default-features` builds without them.
- [x] 1.2 Add `crust mcp`: stdio transport, initialisation, an empty tool list, the log on stderr, and no progress bar. Verify with an integration test that drives the binary over pipes: `initialize` succeeds, `tools/list` parses, and every stdout line is a protocol message even at `-l debug`.
- [x] 1.3 Add the session thread that owns the `Stage`, plus the command channel (D2). Verify with a test that issues 100 interleaved commands from concurrent handlers and gets every reply in order per caller, with no `Stage` ever crossing threads (the type system enforces this; the test pins the behaviour).
- [x] 1.4 Factor the CLI's render-and-write body (`load_scene` → `select_products` → render → product writers → PNG) into one function that `render` and the session both call (D8). Verify that `scripts/check_images.sh check` against goldens recorded before the refactor reports no change.

## 2. Sessions and the override layer

- [x] 2.1 `open_session`: create the override layer with the relative or absolute sublayer path (D1, D5), refuse an output that is one of the input's layers, resume an existing override layer of the same input, refuse one of another input, and import. Verify with tests for each scenario of the "Opening", "Resuming" and "Source layers are never written" requirements; in each, check that the input files' bytes are unchanged.
- [x] 2.2 Add `query` for prims (children, type, active), attributes (value at the session's time, typeName) and opinion sources (which layer). Load payloads on demand for queried prims. Verify on `samples/cornellbox.usda` that `query` on a light returns its exposure and names the source layer, and that after a `set_attribute` it names the override layer.
- [x] 2.3 Add `check`, returning the `crust-check/1` report of the current scene. Verify that it equals `crust check -i <output> --json -` on the same layer.
- [x] 2.4 Add `docs/architecture.md` rows for the session (the file is the session, the thread ownership), and a `site/` page section on what a session writes. Verify with `zola build`.

## 3. Editing through composition

- [x] 3.1 Add `set_attribute` with type conversion from the declared `typeName` (D3), and `set_variant`, `set_active` and `bind_material`. Each saves and re-imports, and reports the warning codes gained or lost and the import time. Verify each scenario of the "Edits are opinions" and "Each edit batch is saved and imported" requirements, and round-trip each tool's output through a fresh `Stage::open` comparing composed values.
- [x] 3.2 Add `author_usda` (parse to an anonymous layer, refuse `subLayers` changes, copy specs as one batch). Verify that a three-opinion snippet triggers one import, and that a snippet adding a sublayer or failing to parse leaves the layer byte-identical.
- [x] 3.3 Re-anchor asset paths to the override layer's directory (D5). Verify with the spec's "Output in another directory" scenario, opening the layer from a different working directory.
- [x] 3.4 Add `undo` as a stack of layer texts (D4). Verify that the layer's content after `undo` is byte-equal to before the edit, that a 16-spp render after undo is bit-identical to the one before the edit, and that a resumed session's undo reports an empty history.

## 4. Looking and measuring

- [x] 4.1 Add `render` (time budget, snapshot PNG downscaled to ≤1024 px, spp reached, done, id), `snapshot` and `cancel`. Edits and new renders cancel a running render (D6). Verify that a render that needs minutes returns within budget + 1 s with `done = false`, that `snapshot` later shows more spp, and that `set_attribute` during a render reports it `cancelled`.
- [x] 4.2 Pin reproducibility: a session render at 16 spp to completion, compared with `crust render -i <output> -s 16` by `crust diff`, is identical. Do this for `cornellbox` and one sample with products and AOVs.
- [x] 4.3 Add `probe` (beauty and the retained AOVs at a pixel; prim and material through the prim-path table when present, otherwise `null` with the reason) and `diff` (the `image-comparison` code; refuse mismatched spp or regions), with an 8-render retention policy. Verify by probing a known pixel of the Cornell box against values read from the CLI's EXR, and by checking that `diff` of two renders straddling a no-op edit reports identical.

## 5. The session's result

- [x] 5.1 Add `render_final` (save, then the shared render-and-write path, with products resolved against the layer's directory, and `<output stem>.exr` and `.png` by default). Verify that its files are identical (`crust diff` exit 0, PNG bytes equal) to `crust render -i work/x.usda -o work/x.exr` run from another working directory.
- [x] 5.2 Document the tools, the override-layer contract and a worked look-dev session in a new `site/` page, `mcp` in `reference/command-line.md`, and the subcommand in `openspec/specs/cli/design.md` and CLAUDE.md. Verify with `zola build` and by running the worked session's tool calls in the integration test harness.

## 6. Claude Desktop

- [ ] 6.1 Hand-test with Claude Desktop through `claude_desktop_config.json` on macOS and Windows. Measure `open_session` and edit latencies against Desktop's tool-call timeout on `cornellbox` and one medium sample, and record them in the design record. If the timeout is exceeded, file the follow-up that moves imports behind the id/poll pattern.
- [x] 6.2 Build the Desktop extension bundle in `nightly.yml` beside the existing binaries (D9). Verify that the bundle installs in Claude Desktop and that `tools/list` shows every tool.

## 7. Integration

- [x] 7.1 Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo deny --locked check` and the pinned-nightly clippy leg, and verify all are clean.

## Workflow follow-up

- Phase 3 change (`live-scene-edits`): `SceneEdit`/`LiveScene` driven by `StageSink` change lists, each tier pinned bitwise to a full re-import, behind the D10 `Session` seam.
- Then `hydra-delegate`: an FFI crate (an `unsafe` project decision) mapping Hydra's scene delegate onto the same `SceneEdit` API.
- Archive after the prerequisites are archived, syncing `mcp-session` and the `cli` delta into `openspec/specs/`.
