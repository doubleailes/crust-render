# Proposal

## Why

The compositing pipeline expects renders split into passes and layers. A **render pass**
is one engine invocation. Inside a pass, **render layers** partition the objects. One traversal
produces every layer, and each layer holds out the others for free. The pass's **file pattern**
decides how the outputs are grouped into files.

Crust today writes exactly the files its RenderProducts name. It cannot give comp a per-layer split
of one render. USD has a `RenderPass` schema but no layer concept and no file-grouping rule, and
crust deliberately reads no `RenderPass` today (`aovs/design.md`).

## What Changes

- **Render passes.** `crust render --pass <RenderPass prim>` renders one pass in one invocation.
  The pass's `renderSource` names the `RenderSettings` the render uses (camera, resolution, samples,
  RenderVars). Without `--pass`, nothing changes.
- **Render layers.** A pass declares its layers as multiple-apply collections on the RenderPass
  prim, `collection:layer:<name>`, resolved through `UsdCollectionAPI` as light links already are.
  Every sample is owned by the layer of what the camera ray first meets (after cutouts). That
  layer's film receives the sample's colour, alpha and AOVs. Every other layer receives nothing,
  which makes it a holdout there. Lighting is untouched: every object still shadows, reflects and
  bounces light in every layer. Geometry no layer claims, and a backdrop the camera sees, belong to
  an implicit `default` layer. The layers therefore partition the frame, and their colours sum to
  the beauty.
- **File patterns.** A pass's `crust:filePattern` (or `-o` with `--pass`) uses the pass pattern
  specifiers: `$l`/`$L` (pass), `$n` (layer), `$o` (AOV), `$f`/`$0Nf` (frame), `$c`/`$C` (camera),
  `$e`/`$E` (eye), `$x` (extension), `$(VAR)` (environment variable), and `$_` to lower-case a
  specifier (`\$` for a literal `$`). The frame, its rounding, its padding and the warnings
  follow `output-path-frame-tokens` for product paths. The pattern is expanded for every
  (layer, AOV). **Outputs that expand to the same path
  are written to one multichannel EXR.** Omitting `$o` puts a layer's AOVs together. Omitting `$n`
  puts the layers together. In a file holding several outputs, each output's channels are named
  by the pass's `crust:layerPattern` (default `$n_$o`).
- **Listing and checking.** `crust ls pass` lists the stage's RenderPass prims. `crust check --pass`
  reports the files the pass would write, and the channels in each.
- **Refusals before rendering.** An unset `$(VAR)`, an extension other than `.exr`, or a
  `--pass` that is not a RenderPass stops the command with a non-zero exit before anything is
  traced. These would write somewhere other than intended. An unknown specifier is kept as
  written, with `product.unknown_variable`, as a product path's unknown variable is.
- **Out of scope, and warned when authored:** a pass's `renderVisibility` and `cameraVisibility`
  collections (they change the lighting world, which needs a separate traversal), and per-pass
  lighting or material overrides (USD has no schema for them). Also out of scope: per-layer AOV
  lists (every layer gets the pass's AOVs), a per-object "transmit the layer behind" switch (glass
  owns what is seen through it), and rendering several passes in one invocation.

## Capabilities

### New Capabilities

- `render-passes`: selecting a RenderPass and its settings; render layers as collections on the
  pass; ownership of each sample by the camera ray's first hit, and the partition and holdout it
  implies; file patterns and the grouping of outputs into multichannel EXRs.

### Modified Capabilities

- `cli`: `crust ls` gains the `pass` kind, and `render` / `check` gain `--pass`. "Command-line
  argument parsing" is modified, as `output-path-frame-tokens` leaves it, to say that with
  `--pass`, `-o` replaces the pass's pattern.
- `image-output`: "Output paths expand frame variables" (added by `output-path-frame-tokens`) is
  modified so that, with `--pass`, `-o` is not expanded as a husk path.
- `scene-check`: a new requirement for reporting a pass's files and channels.

## Impact

- **Code.**
  - `crust-core` USD import: read the RenderPass, its `renderSource` and its layer collections,
    reusing `light_links.rs`'s membership resolution, and resolve a geometry → layer table.
  - `crust-core` film: `FirstHit` carries the owning geometry, and `AovFilm` slots are keyed by
    (var, layer).
  - `crust-render`: pattern expansion, grouping into products, and per-file channel naming in
    `products.rs`; the `--pass` flag and `ls pass`.
- **Relation to `output-path-frame-tokens`: this change depends on it and archives after it.**
  That change gives `productName` and `-o` husk's frame variables (`$F4`). This change gives a
  pass's file pattern the pass pattern dialect (`$04f`). The two dialects apply to different inputs,
  and with `--pass`, `-o` goes to the pattern expander only. The rules the two dialects share
  match that change's rules: default frame 1, halves rounded away from zero, sign counted in the
  padding, `\$` as the escape, and its `product.subframe_frame` and `product.unknown_variable`
  codes. Its `product.shared_path` refusal still applies to authored products. In a pattern, a
  shared path is how outputs are grouped, by design.
- **Performance.** A render without `--pass` is unchanged. A layered render adds one table lookup
  per camera sample. Memory grows to one set of film planes per layer.
- **Warnings.** New codes for overlapping layer membership, a layer target inside an instance or
  a point instancer, and ignored pass collections. Patterns reuse `output-path-frame-tokens`'s two path codes.
- **Docs.** `site/` pages for the `--pass` flag, `crust:filePattern`, the layer collections and the
  warnings, plus a new `render-passes` design record.
- **No new dependency.** The `exr` crate already writes the single-part multichannel files.
