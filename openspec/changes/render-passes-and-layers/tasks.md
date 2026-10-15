# Tasks

## 1. Pin the pattern semantics

- [x] 1.1 Check the specifier rules against the reference renderer's documentation, or against a
  mono render of it: the `$e`/`$E` values for a single view, `$c` versus `$C`, `$L`, `$_` on each specifier,
  how a subframe expands in `$f`/`$0Nf`, whether an unset `$(NAME)` is an error, and its
  escape for a literal `$` (the spec uses `\$`, as `output-path-frame-tokens` does). Where
  the reference renderer differs on a rule the two dialects share (D7), keep `output-path-frame-tokens`'s rule
  and record the difference. Record each
  rule and its source in `design.md` (D7). If a rule differs from `specs/render-passes/spec.md`,
  update the spec before any code. Verify: every specifier in the spec's "File pattern specifiers"
  requirement cites a source in `design.md`.
- [ ] 1.2 Get the channel list of an EXR the reference renderer wrote with a pattern that omits
  `$n` (for example `exrheader` output, from the studio), and compare it with the spec's "Channel names inside
  a pattern file". Align the spec with the reference where they differ. Verify: `design.md` (D8) quotes
  the reference channel list, and the spec's scenarios match it.

## 2. Selecting a render pass

- [x] 2.1 Make the render settings prim an input of the import in `crates/crust-core/src/scene/usd_import/` (D1). Every `render_settings_*` helper takes the path, and a render without a pass passes today's `render_settings_path(stage)`. Verify: `cargo test --workspace` passes unchanged, and `scripts/check_images.sh check` on a recorded golden directory reports no change.
- [x] 2.2 Add `--pass <path>` to `render` and `check` (`crates/crust-render/src/main.rs`). Resolve the RenderPass and its `renderSource`, and fail with a non-zero exit, naming the prims, when either is not the right type. Verify: integration tests in `crates/crust-render/tests/` cover "A pass picks its settings", "Not a render pass" and "A source that is not render settings".
- [x] 2.3 Add the `pass` kind to `crust ls`, with its `crust-ls/1` JSON record (`render_source`, `layers`, `file_pattern`, `layer_pattern`). Verify: tests for "Listing the render passes of a stage" and "Pass records"; `crust ls mesh` still names the valid kinds, `pass` included.
- [x] 2.4 Add a sample stage `samples/render_passes.usda` (a set, a hero, a cutout leaf, a glass pane, a dome, two passes) for the tests in this and later groups. Verify: `crust ls pass -i samples/render_passes.usda` lists both passes.
- [x] 2.5 Document `--pass` and `ls pass` in `site/content/docs/reference/command-line.md`. Verify: `zola build` in `site/` succeeds, link check included.

## 3. Layer membership

- [x] 3.1 Resolve each `collection:layer:<name>` through the light-link membership path (`light_links.rs`'s `link_query`) into one `u16` layer per `geom_id` and per volume region, in `apiSchemas` order, with `default` last (D2, D3). The table is empty when only `default` exists. Verify: import tests on `samples/render_passes.usda` assert the layer of the set, hero and leaf geometries, and of an unclaimed prim (`default`).
- [x] 3.2 Add the warning codes for overlap (earlier layer wins), for a layer target inside an instance or under a point instancer (judged on the instance or instancer; native instances of one prototype split correctly, see design Context), and for a pass's restricting `renderVisibility` / `cameraVisibility`. Each gets its row in `site/content/docs/reference/warnings.md`. Verify: the warnings-reference consistency test passes (`cargo test -p crust-core`), and tests cover "Overlapping layers" and "A camera-visibility collection".
- [x] 3.3 Document the layer collections, the `default` layer and the instance limitation in a new `site/content/docs/usd/render-passes.md`. Verify: `zola build` succeeds.

## 4. One traversal, one film per layer

- [x] 4.1 Carry the owner in `FirstHit` (`geom_id` for surfaces and thin walls, region index for volumes) and map it to a layer in the film (D4). Verify: unit tests for "A cutout leaf in front of the hero", "Seeing the sky" and "Glass owns what is seen through it", using single-ray renders on the sample stage.
- [x] 4.2 Key `AovLayout` slots by (var, layer). Add each sample only to its owner's block, and route per-layer colour as `C.*[LO]` is routed (D5). Verify: tests for "A hero behind a wall is held out" and "Depth where a layer owns nothing".
- [x] 4.3 Pin the light-transport invariants. Verify: a bitwise test that one layer's colour equals the beauty without `--pass` ("One layer is the beauty"); a test that the layers' colours sum to the beauty within rounding at `-s 16 --indirect-clamp 0` ("The layers add up to the beauty"); and the hero-shadow scenario.
- [x] 4.4 Make a guided render's blend carry the layer slots as it carries the other AOV slots. Verify: the sum-to-beauty test also passes with guiding on.
- [x] 4.5 Record the film layout, the memory formula and the ownership rules in `openspec/specs/aovs/design.md`, and remove `RenderPass` from its "not consumed" list. Verify: the section exists and states the per-layer memory formula.

## 5. File patterns

- [x] 5.1 Write the pattern parser and expander in `crates/crust-render/src/products.rs` (D6, D7): every specifier, `$_`, `\$`, `$(NAME)`, and refusal before the import for an unset variable or a non-`.exr` extension. Reuse `output-path-frame-tokens`'s frame helpers (rounding, padding, subframe detection) rather than copying them. Verify: unit tests cover every scenario of "File pattern specifiers", "Frame specifiers in a pattern" and "Patterns that would write elsewhere are refused", and a pattern with no `$` is returned unchanged.
- [x] 5.2 Raise `product.unknown_variable` and `product.subframe_frame` (from `output-path-frame-tokens`) for patterns, naming the pass. Verify: tests for "Unknown specifier", "A lone dollar" and "Frame 12.5".
- [x] 5.3 Route `-o` with `--pass` to the pattern expander only. Verify: tests for the `cli` scenario "Output with a pass is a pattern" and the `image-output` scenario "-o with a pass is not expanded as husk's" (no `product.unknown_variable` for `$n`, `$04f`, `$x`).
- [x] 5.4 Group the (layer, var) outputs by expanded path into synthesized products. Name channels with the expanded `crust:layerPattern` (default `$n_$o`) when a file holds several outputs, refuse two outputs of one file with one layer name, write a PNG only for a single-layer file with a beauty, and keep `refuse_shared_paths` for authored products only. Verify: integration tests on the sample stage for "No `$o`", "Neither `$n` nor `$o`", "One layer per file", "Several layers per file", "An authored layer pattern", "Two outputs with one layer name", "Per-layer previews", "The authored pattern" and "`-o` replaces the pattern".
- [x] 5.5 Document `crust:filePattern`, `crust:layerPattern`, the specifiers, the default pattern and the grouping rule in `site/content/docs/usd/render-passes.md`. Verify: `zola build` succeeds, and the page's example pattern produces the files it lists when rendered on the sample stage.

## 6. Checking a pass

- [x] 6.1 Make `crust check --pass` report the pattern's files and channels as `products`, and fail as a render would on a refused pattern. Verify: tests in `crates/crust-render/tests/check.rs` for "Files of a layered pass" and "A refused pattern", plus "Checking a pass" from the cli delta.

## 7. Integration and performance

- [x] 7.1 Confirm the default render is untouched. Verify: the zero-AOV instruction-count pin passes, and `scripts/check_images.sh check` reports no change on its recorded scenes.
- [x] 7.2 Measure a layered render's cost. Verify: `callgrind` instruction counts at `-s 2` for the sample pass, with three layers against the same pass with one layer, recorded in the new design record (7.3). The per-sample overhead is stated as a number.
- [x] 7.3 Write `openspec/specs/render-passes/design.md`: decisions D1–D8, the measurements from 7.2, and the known gaps (pass visibility collections, per-pass overrides, instance splitting, per-layer AOV lists, a transmit-layer switch, multi-part files, product `driver:parameters:*` not copied). Add the capability to CLAUDE.md's documentation table and the pair "layer colours ↔ beauty" to `docs/architecture.md` § Invariants. Verify: CI's four jobs pass (`fmt`, `clippy -D warnings`, `test --workspace`, `deny`).

## Workflow follow-up

- Implement and archive `output-path-frame-tokens` **first**. This change depends on it: task 5.1
  reuses its frame helpers, task 5.2 its warning codes, and the `image-output` and `cli` deltas
  modify requirements that it adds. `openspec validate` reports the `image-output` delta as not
  applicable until then.
- Archive the change after review, then confirm `openspec/specs/render-passes/spec.md` exists with
  its Purpose.
