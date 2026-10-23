## 1. Blockers ahead of the custom nodes

- [x] 1.1 Resolve materials bound from outside a streamed chunk on a side stage
      (design D1). Test:
      `a_streamed_chunk_resolves_a_material_bound_from_a_sibling_subtree`.
- [x] 1.2 Build the MaterialX `UsdPreviewSurface` from its nodegraph (design D2).
      Vendor its nodedef and check the table in `tests/nodedefs.rs`. Tests: the
      `usd_preview_surface` cases in `crust-core/tests/mtlx_surfaces.rs`.
- [x] 1.3 Update the `usd-scene-import` and `materials` design records and
      `site/content/docs/usd/materials.md`.

## 2. Definition library

- [x] 2.1 Add a `Config` field for `PXR_MTLX_PLUGIN_SEARCH_PATHS`, with its rows
      in `docs/architecture.md` and
      `site/content/docs/reference/environment-variables.md`.
- [x] 2.2 Parse `<nodedef>` and nodegraph implementations in `crust-mtlx`, from
      the search paths and from the document, and load them once per import.
      Done as `crust_mtlx::Library` (`library.rs`), the document's own in
      `Doc::library`, the import's in `MaterialCache::library`; an unreadable
      file warns `mtlx.library_unreadable`. Tests: `library.rs`'s unit tests,
      `parse.rs`'s implementation-graph test, and
      `the_mtlx_plugin_search_path_loads_a_node_library` in
      `crust-render/tests/check.rs`.

## 3. Expansion

- [x] 3.1 Add the `Doc` expansion pass (design D4). Resolve the inline-USD
      shader id against the library before falling back to `nodedef_category`.
- [x] 3.2 Tests: nesting, multioutput through an instance, defaults, repeated
      interface use, and a bitwise test that an expanded node equals the same
      graph written out by hand. Done in `crust-mtlx/tests/expand.rs`, plus
      `an_inline_custom_node_from_the_library_compiles` in
      `crust-render/tests/check.rs` for the inline-USD path.

## 4. Standard nodes

- [x] 4.1 Add `separate2`/`3`/`4`, `switch`, `over`, `in`, `out`, `premult`,
      `difference` and `place2d`, pinned by `crust-mtlx/tests/stdlib_nodes.rs`
      against the stdlib's expressions. They are listed in
      `scripts/osl_oracle.py`, but the fixture is not regenerated: that needs
      MaterialX's Python bindings and an exact-math OSL build.
- [x] 4.2 Add `transformmatrix` with a constant `matrix44` (design D5), and read
      `matrix4d` values in inline USD.

## 5. Verification

- [x] 5.1 Verify on a production stage that uses a studio node library: before
      this change every such material fell back to grey; after it, every one
      compiles with no `mtlx.*` warning, `crust check` is clean, and the
      rendered albedo shows the projected textures. The stage and its library
      are confidential and are not part of the repository; no sample built from
      them is committed.
- [x] 5.2 Run `cargo fmt`, `clippy -D warnings`, `cargo test --workspace` and
      `cargo deny`. fmt, clippy and deny are clean. The workspace tests were
      run in release on Windows: the only failures are ones that also fail on
      the base commit or alone pass (two `live` tests sharing a temp file, two
      render-timing tests, two Windows path / permission tests, one tracer
      cancellation race).
