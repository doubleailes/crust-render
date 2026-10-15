# Design

## Context

See `proposal.md` (Why) and `specs/render-passes/spec.md` (the behaviour). This section covers only
the parts of today's code that shape the approach.

- **One settings prim per render.** `render_settings_path` (`usd_import/settings.rs:108`) picks the
  `renderSettingsPrimPath` metadatum, else `/Render/settings`. `import_render_settings` and the
  product selection (`select_products` in `crust-render`) read everything from that one prim. No
  code reads `RenderPass` today; `aovs/design.md` lists it under "not consumed".
- **Collection membership is solved already.** Light linking (`usd_import/light_links.rs`) resolves
  `collection:<name>` through openusd's `CollectionAPI::compute_membership_query`. It handles the
  chunk-vs-index-stage opinion problem of the streaming import, and it records which `geom_id`
  range and volume region each prim emitted (`LightLinks::saw`, `geometry_runs`). The result is a
  per-`geom_id` table on the world (`WorldBuilder::light_classes`).
- **The film already knows the camera ray's first hit.** `trace_path::<AOV = true>` records
  `scratch.first: FirstHit` (`aov.rs:611`): `Escaped`, `Volume { p }`, or `Surface { .. }` after
  cutout pass-through, with a thin wall counted as met (`first_wall`, `path.rs:1675`). It carries
  no identity yet. `AovSamplePlanes::add` (`aov.rs:907`) adds every slot from one sample.
- **LPE colour is already pinned to the beauty.** `C.*[LO]` is pinned bit for bit to the beauty
  (`route.rs` header), and the zero-AOV render is pinned to its instruction count.
- **Products to files.** `crust-render/src/products.rs` names channels (`channel_names`, `layer.C`,
  beauty bare), refuses two products writing one path (`refuse_shared_paths`), and writes one
  single-part EXR per `AovProduct` (`write_product`).
- **Hits on instances.** A hit carries `geom_id` plus an opaque `placement`. A native instance
  attaches ids of its own (`attach_proto_parts`: a leaf reports its own id, a group its own base),
  so two native instances of one prototype are two `geom_id` runs, judged by their two instance
  paths. What shares ids is everything one emitting prim brings in: a point instancer's instances
  are one run, and a prim inside a prototype has no run of its own. (This change first assumed
  native instances shared ids too; task 3.1's test showed they do not.)
- **The word "pass" is taken.** Inside the engine, a "pass" is a sampling sweep (`render_pass`, a
  guided render's passes). This change always says **render pass** for the USD prim, and names its
  warnings and flag `pass` only where users see them.

## Goals / Non-Goals

**Goals:**
- One traversal per render pass, however many layers it has.
- A render without `--pass` stays bit-identical and keeps its instruction count. A render with
  `--pass` and only the `default` layer gives that layer the beauty bit for bit.
- Reuse the light-link membership machinery and the existing film. No parallel implementation of
  either.

**Non-Goals:**
- Changing the lighting world per pass (`renderVisibility` / `cameraVisibility`), or per-pass
  material and light overrides. Both are warned (spec) and left to a follow-up change.
- Distinguishing instances of one prototype by layer (see Risks).
- Multi-part EXRs, deep output, and per-layer AOV lists.

## Decisions

### D1. `--pass` selects the settings prim, nothing else changes shape

`--pass` resolves the RenderPass prim, follows `renderSource` to a `RenderSettings` prim, and hands
that path to the import wherever `render_settings_path` is called today. The settings path becomes
an import input instead of being re-derived from the stage in each `render_settings_*` helper. With
no `--pass`, the input is today's `render_settings_path(stage)`, so the default render cannot change.

*Alternative:* a separate `--settings` flag, husk-style. It is not rejected for good, but it is not
needed, and it would be a second way to reach the same prim.

### D2. Layers are `collection:layer:<name>` on the RenderPass prim

Layers belong to a pass, so two passes can split the same objects differently.
Multiple-apply collections on the pass prim express exactly that in standard USD. Layer order is
the order of the `CollectionAPI:layer:*` entries in the prim's composed `apiSchemas`. `default` is
always last and always exists.

*Alternatives rejected:*
- A per-prim `crust:layer` attribute. It cannot vary per pass.
- Reading the pass's `cameraVisibility` as layer membership. USD defines exclusion from it as "the
  camera sees through", which is the opposite of a holdout. Bending it would make crust's stages
  mean something different in every other renderer.

### D3. A per-`geom_id` layer table, built like the light-link classes

The import resolves each layer's membership query once, through the same `link_query` path
(`light_links.rs:63`, including its chunk-vs-whole rule and its `membershipExpression` and
inside-instance warnings). It walks the `geom_id` runs and volume regions that `saw` already
records, and writes one `u16` layer index per `geom_id` and per volume region. Overlaps resolve to
the earlier layer, with the coded warning.

The table is empty when the pass has only `default`. The film then never looks it up, and every
sample goes to layer 0.

*Alternative:* resolve the layer at hit time from the prim path. Rejected: it is a hash lookup per
camera sample, and hits carry no path.

### D4. `FirstHit` carries the owner

`FirstHit::Surface` and `FirstHit::Volume` carry the owner's layer, resolved by the integrator where
it writes them: `World::layer` of the hit's `geom_id` (the wall's, in `first_wall`), and the
scattering region's layer, which `VolumeEvent::Scatter` carries beside its light class. `Escaped`
goes to `default`. (As built: the integrator resolves the layer rather than handing the film a
`geom_id` and a region index to resolve. The answer is the same, and the film needs no table.) Ownership is
therefore decided by the same code that already decides the alpha and the geometric AOVs. The
cutout and thin-wall rules for ownership are, by construction, the coverage rules alpha already
uses, so a layer's alpha and colour cannot disagree about who owns a sample.

### D5. Film slots keyed by (var, layer)

`AovLayout` multiplies its slots by the layer count, keyed `(var, layer)`. `AovSamplePlanes::add`
picks the owning layer's slot block and adds only there. Other layers' slots receive nothing, which
is the holdout, and pixels left empty keep each source's clear value.

- **Per-layer beauty** is not the beauty buffer, which is the whole frame. It is the beauty's own
  colour routed like `C.*[LO]`. That is the expression already pinned bitwise to the beauty, so
  with one layer the layer's colour is the beauty bit for bit, and with several layers the layers
  sum to it up to rounding. Both are pinned by tests.
- **Per-layer LPE** expressions are evaluated once per sample, as today. Only the destination block
  changes.
- **`sampleCount` and `variance`** stay per pixel, not per layer: adaptive sampling remains one loop
  over the beauty. A layer with few pixels cannot stop earlier than its neighbours, which is
  correct, since the beauty is what converges. Each layer's file carries the same per-pixel values.

The light-path inspection and the MCP session do not use layers. They keep their current films.

*Alternative:* one `AovFilm` per layer. Rejected: N films means N pixel-state walks and N
allocation sites, where one wider slot vector is a single index offset.

### D6. Patterns become products, and the writer is unchanged

In `crust-render`, a pass's pattern is parsed once into literal and specifier tokens, so a refusal
(an unset `$(NAME)`, or a non-`.exr` extension) is reported before the import. An unknown
specifier is kept as written and warned, as D7 explains. The
pattern is then expanded for every (layer, var), layer-major. The outputs are grouped by expanded
path in first-occurrence order, and each group becomes one synthesized `AovProduct`. Its vars are
the group's (var, layer) pairs, and its channel prefix is the layer name whenever the group holds
more than one layer. The existing writer, header stamp and PNG rule then apply unchanged.
`refuse_shared_paths` is not called on synthesized products, because there a shared path is the
grouping. It still guards authored products.

The pass's var list is every RenderVar of the source settings' products, deduplicated by prim path
in product order. Products' authored `driver:parameters:*` are not copied into pattern files, which
is a known gap. Crust's own stamp (working space, sampling, source, exposure) is copied.

Environment variables are read in `crust-render` while the pattern is expanded. They are path
inputs, not behaviour switches, so the "`Config` is the only place the environment is read" rule
(it covers `CRUST_*` switches) does not apply. `design.md` of `cli` will record why.

### D7. Two dialects, two inputs, one set of path rules

`productName` and `-o` without `--pass` keep the husk dialect of `output-path-frame-tokens` (`$F4`).
A pass's pattern, and `-o` with `--pass`, use the pass pattern dialect (`$04f`). The branch is taken once, where
`-o` is routed: with `--pass` it never reaches `expand_frame_vars`, so `$n` or `$x` in it can
never raise a husk `product.unknown_variable`. The two deltas that state this (`image-output`
"Output paths expand frame variables", `cli` "Command-line argument parsing") modify requirements
that change adds. **This change therefore archives after `output-path-frame-tokens`.**

Everything the two dialects have in common follows that change, so a shot cannot get two answers
for one frame:

- the frame is `-f`, and 1 without it (its D5);
- a subframe rounds halves away from zero and raises `product.subframe_frame` (its D4, D6);
- padding counts the sign (`-003`);
- `\$` is the literal `$`;
- an unknown name is kept as written and raises `product.unknown_variable`. A lone `$` raises
  nothing.

The pattern adds two refusals that a product path does not have. An unset `$(NAME)` would move the
file to the filesystem root, and a non-`.exr` extension would be written as EXR under a wrong name.
Both fail the command instead of warning.

The frame helpers (rounding, padding, subframe detection) are shared with `expand_frame_vars`,
not copied. Only the grammars differ.

**Sources (task 1.1).** The specifiers were pinned against the reference renderer's documentation
of its file naming and render settings; no running copy was available, so each rule below comes
from that documentation or is crust's own choice, marked as such.

| specifier | reference | crust |
|-----------|-----------|-------|
| `$l` | the pass path, prefix and hierarchy included | the pass prim's name. **Differs**: USD has no reference prefix, and the prim path would add directories. The user chose the name |
| `$L` | the pass name without prefix or hierarchy | the pass prim's name |
| `$n` | the layer name | the layer's name |
| `$o` | the output (AOV) name | the RenderVar prim's name |
| `$c` / `$C` | camera name with / without prefix | both the camera prim's name (no prefix in USD) |
| `$e` | the eye number, 1 for the leftmost | `1` |
| `$E` | the eye name, `left` / `right` | `left` |
| `$f`, `$0Nf` | frame number, `$03f` zero-padded to 3 | as the reference; sign counted in the padding (shared rule) |
| `$x` | the default image extension | `exr` |
| `$(NAME)` | environment variable, in any path | as the reference; unset is refused (crust's choice: undocumented) |
| `$_` | an underscore after the `$` lower-cases it | as the reference, on every specifier |
| subframe | not documented | shared rule: rounded half away from zero, `product.subframe_frame` |
| literal `$` | not documented | shared rule: `\$` |

crust's default pattern is `$l_$n_$o.$04f.$x`. A default rooted at an environment variable would be
refused on most machines, and `$e` is constant for crust's single view.

### D8. Single-part multichannel files, channels named by the layer pattern

This is the user's decision: grouped outputs go into one multichannel EXR, as products already are.
Each output inside a shared file is named by the pass's **layer pattern**, as the reference
renderer does: `crust:layerPattern` on the pass, default `$n_$o`, expanded with the file pattern's
specifiers for each (layer, var). In a file that holds several outputs, each output's channels are
`<layer name>.<channel>` (`hero_Diffuse.R`), the beauty included. A file holding one output keeps a
product's names (the beauty bare), which is what a one-AOV-per-file pattern gives comp. Two
outputs of one file with the same layer name would overwrite each other's channels, so they are
refused before rendering.

The reference channel list of task 1.2 (an `exrheader` of a reference file written with a pattern
that omits `$n`) is not available yet. The rules above follow the documentation. Whether the
beauty of a several-output file is written bare is the one point the documentation leaves open.
When the studio provides that header, this decision is checked against it.

Multi-part EXRs, one part per layer with a per-part data window, were considered and deferred:
they would need the products module to write parts, and comp has not asked for them.

## Risks / Trade-offs

- **[Memory grows with layers × vars]** Five layers × ten `color3f` vars at 4K is about 5 GB of
  f32 planes (5 × 10 × 12 B × 8.29 M pixels; `aovs/design.md` § Render layers has the formula). → `--stats` reports the peak memory, not the film alone; the formula lets a shot
  budget it.
- **[Point-instancer instances cannot be split across layers]** An instancer is one run
  (Context), so all its instances are in its layer, and a target inside a prototype is judged on
  the instance or instancer that draws it. → The same limitation light links document; a target
  inside an instance or under a point instancer raises `pass.layer_target_in_instance`. Native
  instances of one prototype split across layers correctly. Splitting an instancer by
  `placement` is a follow-up.
- **[Channel names unconfirmed against a real file]** The layer-pattern rule (D8) comes from
  the reference renderer's documentation, not an EXR it wrote. → Task 1.2 checks it against an
  `exrheader` dump from the studio, and the spec follows the reference where it differs.
- **[Specifier details]** Task 1.1 pinned the specifiers against the reference renderer's
  documentation (D7's table), not against a running copy. Subframes, the literal `$` and an
  unset `$(NAME)` are not documented and follow crust's own rules.
- **[Glass owns what is behind it]** Comp sometimes wants the hero seen through a window in
  `hero`. → Out of scope (proposal). The ownership rule is a single function, so a per-object
  "transmit layer" switch can be added there later.
- **[Guided renders]** A guided render blends its sampling passes' buffers (`blend_passes`). Layer
  slots must blend as the existing AOV slots do. → Task 4.4 pins a guided layered render's layer sum
  to its beauty.
- **[A hot-path cost for the default render]** → The layer lookup sits behind the film's `AOV`
  instantiation and an empty-table check. The zero-AOV instruction-count pin and `check_images.sh`
  are the gates (task 7).

## Migration Plan

The change is additive. Stages without RenderPass prims, and commands without `--pass`, are
untouched. There is nothing to migrate. Rolling back means dropping `--pass`, and nothing persisted
depends on it.
