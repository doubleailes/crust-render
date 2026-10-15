# Tasks

## 1. Pin the husk semantics

- [x] 1.1 Check SideFX's husk documentation (default `--frame`) and Houdini's `$F`/`$FF`/padding rules against design D3–D5 (default frame 1, rounding, sign counted in the padding width). Record any difference in `design.md` and, if it changes behaviour, in the image-output delta spec before writing code. Verify: `design.md` cites the source for each rule.

## 2. Path expansion function

- [x] 2.1 Add the pure `expand_frame_vars(path, frame)` to `crates/crust-render/src/products.rs` (grammar D3, formatting D4). It returns the expanded string and the unknown-name / subframe issues. Verify: unit tests cover every scenario in the image-output delta (`$F4` at 12, `$F`/`${F3}`/`$FF`, `<F4>`/`<F>`/`<FF>`, `<UDIM>` left alone, `-3`, `\$`, `12.5` with `$F4`, `<F4>` and `$FF`, `$SHOT`, `$F4x`, a lone `$`, an unclosed `${`, a Windows `\` path, and a path with no `$` or `<` returned byte-identical); `cargo test -p crust-render` passes.

## 3. Warning codes

- [x] 3.1 Add `ProductUnknownVariable` (`product.unknown_variable`, `Refused`, `Each`) and `ProductSubframeFrame` (`product.subframe_frame`, `Approximated`, `Each`) to `crates/crust-core/src/warnings.rs`, with their rows in `site/content/docs/reference/warnings.md`. Verify: the warnings-reference consistency test passes (`cargo test -p crust-core`).

## 4. Wire expansion into product selection

- [x] 4.1 Give `select_products` a `frame: f64` and expand every product name after the `-o` substitution and before the empty-name check and `refuse_shared_paths` (D2). Turn each issue into a coded warning at the product's prim. Verify: a unit test in `main.rs` shows `out.$F4.exr` + `out.0003.exr` at frame 3 raising `product.shared_path` on the second product.
- [x] 4.2 Pass `cli.frame.unwrap_or(1.0)` from `render_and_write` and `args.scene.frame.unwrap_or(1.0)` from `check`. Make `beauty_output` take the frame and expand `-o` for the no-products case, raising the same codes with no prim. Verify: an integration test renders `samples/cornellbox.usda` at `-s 1 -f 12 -o <tmp>/b.$F4.exr` and finds `b.0012.exr` and `b.0012.png`; `crust check -f 5 --json -` on a stage with `productName = "x.$F4.exr"` reports `x.0005.exr` and, at `-f 5.5`, a `product.subframe_frame` record.
- [x] 4.3 Confirm the MCP `render_final` path still anchors the expanded names (the anchor join runs after expansion). Verify: the existing MCP `render_final` test passes, and a new assertion on an `$F4` productName (if that test fixture permits it) finds the expanded file under the session directory.
- [x] 4.4 Confirm that nothing changes when there is no `$`. Verify: `cargo test --workspace` passes with the existing product, check and image-output tests untouched, and `scripts/check_images.sh check` on a recorded golden directory reports no change.

## 5. Documentation

- [x] 5.1 Update the `-o` help text in `main.rs` and `site/content/docs/reference/command-line.md#output`: list the variables, the default frame 1, the rounding warning, and the advice to single-quote. Update `site/content/docs/usd/aovs.md` § Products: `productName` expands the same variables, and collisions are judged after expansion. Verify: `zola build` (0.21) in `site/` succeeds with every link checked.
- [x] 5.2 Update `openspec/specs/aovs/design.md` § Paths and D16 with the expansion step and a pointer to this change's design. Verify: the sections name the order `-o` → expand → refuse → anchor.

## 6. Final checks

- [x] 6.1 Run the CI set: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`. Verify: all pass.

## Workflow follow-up

- Sync the delta specs (`/opsx:sync`) and archive the change after review.
