# Tasks

## 1. The import records its layers (design D1)

- [x] 1.1 Collect `layer_identifiers()` from every import stage before it is released (single-stage, each streamed chunk, the index stage; the camera stage masked for adaptive subdivision composes a subset of the traversal's), map identifiers to files (package → outer package, anonymous skipped, relative → root layer's directory), and expose the sorted set as `Scene::layers`; kept as is by in-place updates. Verify with crust-core tests: a stage with a sublayer, a reference and a payload lists all of them, streamed and single-stage imports list the same set, and a `.usdz` input lists the package file.
- [ ] 1.2 Verify the render path is unaffected: `cargo test --workspace` green, and callgrind on `samples/cornellbox.usda -s 2` shows no render function's instruction count moving (import-only delta recorded).

## 2. The session watches its sources (design D2, D3)

- [ ] 2.1 Add `Sources` (stamps of `scene.layers` minus the override layer, taken after each successful import) and `Open::stale`, with `Open::refresh()` reopening the authoring stage and setting `stale` when a stamp moved. Verify with a unit test that a rewritten sublayer is reported and an untouched one is not.
- [ ] 2.2 Call `refresh()` at the start of `query`, `check`, `render`, every edit tool, `undo`, `light_paths` and `render_final`; make `apply_change` go `full` while `stale` is set, and `render` / `check` import first when it is; re-stamp and clear `stale` after each successful import, keep it set after a failed one. Verify with tests: a sublayer rewritten then a light dimmed reports `applied = full` and the changed layer, and renders bitwise as a fresh session of the same layer; a query after a referenced layer's rewrite answers the new value; a render after a rewrite with no edit shows it; a source rewritten to unparsable text fails `render` naming the layer, and a later fix makes the next `render` succeed; a layer behind a payload is picked up.
- [ ] 2.3 Fold the sources' stamps into a job's identity so `light_paths` refuses a render made before an external change. Verify with a test that rewrites a source between a completed render and `light_paths`.

## 3. The `reload` tool (design D4)

- [ ] 3.1 Add `reload` to the MCP router and the session: stop the render, reopen the authoring stage, force a full import through `sync`, report `sources_changed`, `applied`, warnings gained and lost, keep the undo stack. Verify with tests: reload after a rewrite names the layer; reload with no change lists none and the next 16-spp render is bitwise the previous one; set → external rewrite → reload → undo restores the layer bytes and composes over the new source; reload during a render cancels it; the tool list includes `reload`.

## 4. Documentation

- [ ] 4.1 Update `openspec/specs/mcp-session/design.md` (a "Sources on disk" section: what is watched, why the import records it, the lazy import, the unwatched files as a Known gap, the measured stat cost on the largest checked-in stage) and `site/content/docs/help/claude-desktop.md` (the `reload` tool, using the session beside another program that writes the stage, the in-place-rewrite caveat under `mmap`). Verify with `zola build` in `site/` (Zola 0.21) succeeding.

## 5. Integration

- [ ] 5.1 Run the CI set locally: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`, and `scripts/check_images.sh` against goldens recorded before the change (no image changes). Verify all pass.

## Workflow follow-up

- Sync the delta into `openspec/specs/mcp-session/spec.md` and archive the change after review.
