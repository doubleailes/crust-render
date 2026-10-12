## 1. Register the schemas on the session's stage

- [ ] 1.1 In crust-core, add `pub fn usd_schema_registry()` returning
      `openusd_schemas::schema_registry()`, re-export it from `lib.rs`, and have
      `usd_import::stage_builder()` call it. Done when the workspace builds and every
      import test passes unchanged.
- [x] 1.2 In `crates/crust-render/src/mcp/session.rs`, pass it to
      `Stage::builder().schema_registry(..)` in `open_stage`; keep the session's
      resolver and `InitialLoadSet::LoadNone`.

## 2. Query and set_attribute

- [x] 2.1 `tests/mcp.rs`: the unauthored `/scene/Sky.inputs:exposure` query answers
      `type` `"float"`, `value` `0`, `source` `"fallback"` (replacing the current
      `expect_err`); `/scene/Sky.inputs:nonsense` fails naming an explicit `type`.
- [x] 2.2 `tests/mcp.rs`: `set_attribute` on the unauthored exposure with no `type`
      authors `float inputs:exposure`; with `type` `"double"` it is refused naming
      `float`, and the layer is unchanged.
- [x] 2.3 Check that `query`'s fallback for a DistantLight's unauthored
      `inputs:intensity` equals the intensity the render imports for it. If they
      differ, record it in the `mcp-session` design record's known gaps.
- [x] 2.4 Reword the remaining refusal in `mcp/query.rs` (`attribute_json`) per D4.

## 3. Verify and document

- [ ] 3.1 `scripts/check_images.sh check` against goldens recorded before the change:
      every sample unchanged (the import's stage is untouched).
- [x] 3.2 Docs: remove the limitation from `site/content/docs/help/claude-desktop.md`
      (and drop `type="float"` from its `inputs:exposure` example) and from
      `site/content/docs/architecture/limitations.md`; update the `mcp-session`
      record. Done when `zola build` passes.
- [ ] 3.3 CI locally: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`, and
      `openspec validate mcp-session-schema-registry --strict`.
