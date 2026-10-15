# Proposal

## Why

Crust writes every output path literally. `-o` is a fixed string, and a `productName` changes per
frame only when the stage time-samples it. A farm wrapper therefore has to format the frame into
`-o` itself, and a hand-authored stage with one constant `productName` overwrites the same file on
every frame. Production renderers expand variables in output paths. Crust already copies husk's
`-o` semantics (it replaces the first product's path), so it should copy husk's frame variables too:
`renders/beauty.$F4.exr`.

## What Changes

- Output paths expand husk's frame variables: `$F` (integer frame), `$F2`…`$F9` (zero-padded to
  that width), `$FF` (the frame as a float, subframes kept), and the braced forms `${F}`, `${F4}`,
  `${FF}`. `\$` writes a literal `$`. Husk's UDIM-style forms `<F>`, `<F2>`…`<F9>` and `<FF>` are
  the same variables. They are what Solaris leaves in a USD `productName`, because Houdini expands
  `$F4` before the stage is written.
- Expansion applies to both `-o` and every authored `productName` (after the time code is applied).
  It also applies to the no-products `-o` path. The PNG preview follows the expanded path, as it
  already follows the EXR's.
- The frame is `-f`'s value. Without `-f`, frame variables expand as frame 1 (husk's default
  frame).
- On a subframe, `$F` and `$Fn` round to the nearest frame and raise a coded warning that points to
  `$FF`. Two subframes that round to the same frame would otherwise write the same file without
  saying so.
- Any other `$NAME` is left as written and raises a coded warning. No other variables are expanded:
  no environment variables, and no camera or product names.
- Collision detection between products (`product.shared_path`) compares the **expanded** paths.
- `crust check` reports the expanded file names, for the frame it was given.
- **BREAKING (narrow):** a `productName` or `-o` that already contains a literal `$F…` sequence
  is now expanded. A literal `$` followed by any other name still lands in the path unchanged
  (with a warning).
- Out of scope: per-AOV file splitting (`<aov>`). Each `RenderProduct` names its own file, and a
  shot that wants one file per AOV authors one product per AOV. Also out of scope: rendering a frame
  range in one invocation.

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `image-output`: output paths (`-o`, `productName`, and the PNG that follows them) expand frame
  variables. The time-sampled `productName` requirement gains the expansion step.
- `cli`: `-o/--output` documents the frame variables, both forms.
- `scene-check`: the reported product files are the expanded paths for the checked frame.

## Impact

- Code: `crates/crust-render/src/main.rs` (`select_products`, `beauty_output`, and the call sites
  in `render_and_write` and `check`, which must pass the frame), plus a small path-expansion
  function and its unit tests in `crust-render`. The MCP session's `render_final` goes through
  `render_and_write`, so it inherits the change. `crust-core` is unchanged apart from two new
  `WarningCode` variants in `warnings.rs`.
- Warnings: two new codes, `product.unknown_variable` (`refused`) and `product.subframe_frame`
  (`approximated`). Adding codes bumps no report version (`scene-warnings`).
- Docs: `site/content/docs/reference/command-line.md` (`-o`), `site/content/docs/usd/aovs.md`
  (Products), `site/content/docs/reference/warnings.md` (two codes), and
  `openspec/specs/aovs/design.md` (§ Paths, D16).
- Performance: none. The expansion runs once per product per render.
- No new dependency.
