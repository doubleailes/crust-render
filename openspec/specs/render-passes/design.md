# render-passes — design record

> Design record for the **render-passes** capability: the reasoning, measurements and
> history behind the behaviour `spec.md` states — selecting a USD `RenderPass` and its
> settings, render layers as collections on the pass, the ownership of each camera sample
> and the holdout it implies, file patterns, and the grouping of outputs into
> multichannel EXRs. It began as the design of the change `render-passes-and-layers`.
> The film's side of layers (slots keyed by layer, a layer's beauty, the memory formula)
> is recorded in `aovs/design.md` § Render layers.
>
> **Status.** Implemented. The specifiers are pinned against a reference renderer's
> documentation, not a running copy; the channel names inside a shared file wait on a
> reference `exrheader` from the studio (D8, Known gaps).

## Context

The compositing pipeline expects renders split into passes and layers. A
**render pass** is one engine invocation. Inside a pass, **render layers** partition the
objects; one traversal produces every layer, and each layer holds out the others. The pass's
**file pattern** decides how the outputs are grouped into files. USD has a `RenderPass`
schema but no layer concept and no file-grouping rule, and crust read no `RenderPass` before
this capability.

What the existing code gave the design:

- **One settings prim per render.** Every `render_settings_*` helper of the import read the
  prim `render_settings_path` names. They now take the path as an input (D1).
- **Collection membership was solved already**, by the light links
  (`usd_import/light_links.rs`): openusd's `CollectionAPI::compute_membership_query`, judged
  against the stage path of every prim that emitted geometry or a volume (`LightLinks::saw`).
- **The film already knew the camera ray's first hit** (`FirstHit`), after cutouts, with a
  thin wall counted as met: the coverage rules alpha uses.
- **`C.*[LO]` is pinned bit for bit to the beauty**, so a layer's beauty can be that
  expression restricted to the layer's samples.
- **Instances.** A native instance attaches `geom_id`s of its own (`attach_proto_parts`: a
  leaf reports its own id, a group its own base), so two native instances of one prototype
  are two runs, judged by their two instance paths. What shares ids is everything one
  emitting prim brings in: a point instancer's instances are one run, and a prim inside a
  prototype has no run of its own. The change first assumed native instances shared ids,
  as light links document for their targets; task 3.1's test
  (`two_instances_of_one_prototype_in_two_layers`) showed they do not.
- **The word "pass" is taken.** Inside the engine a pass is a sampling sweep (a guided
  render's passes). The capability always says **render pass** for the USD prim, and names
  its flag, warnings and module `pass` / `render_pass` only where users see them.

## Decisions

### D1. `--pass` selects the settings prim, nothing else changes shape

`--pass` (`UsdImportOptions::render_pass`) resolves the `RenderPass` prim on the index
stage, follows `renderSource` to a `RenderSettings` prim (`passes::pass_settings`), and hands
that path to every `render_settings_*` helper and to `import_render_products`: the path is
an input of `read_index`, which the import and an MCP in-place settings update both call.
Without `--pass` the input is `render_settings_path(stage)`, so the default render cannot
change (D9). A path that is not a `RenderPass`, or a source that is not a `RenderSettings`,
is an error (`Error::NotARenderPass`, `Error::PassSourceNotRenderSettings`) raised before
the traversal; a malformed path is refused before the stage opens (`Error::InvalidPassPath`).
A pass without `renderSource` renders the stage's own settings.

*Alternative:* a husk-style `--settings` flag. Not rejected for good, but not needed, and a
second way to reach the same prim.

### D2. Layers are `collection:layer:<name>` on the RenderPass prim

Layers belong to a pass, so two passes can split the same objects differently.
Multiple-apply collections on the pass express that in standard USD. Layer order is the
order of the `CollectionAPI:layer:*` entries in the prim's composed `apiSchemas`
(`Prim::api_schemas`, strongest first). `default` is always last and always exists; a
collection named `layer:default` is the implicit layer's, not a layer of its own.

*Alternatives rejected:* a per-prim `crust:layer` attribute (cannot vary per pass), and
reading the pass's `cameraVisibility` as membership (USD defines exclusion from it as "the
camera sees through", the opposite of a holdout).

### D3. A per-`geom_id` layer table, built like the light-link classes

`passes::install_layers` reads each layer's claim once off the index stage
(`layer_claim`): the light links' `opinions` and `compute_membership_query`, but with
`UsdCollectionAPI`'s own fallback — a layer that authors nothing claims nothing, where a
light link that authors nothing links everything. `LightLinks::per_receiver` then asks each
emitting prim once, in traversal order, and writes one `u16` per `geom_id`
(`WorldBuilder::set_layers`) and per volume region (`VolumeRegion::layer`). A prim two
layers claim belongs to the earlier (`pass.layer_overlap`, once per import, every prim
counted); a target inside an instance or under a point instancer is judged on the instance
or instancer (`pass.layer_target_in_instance`); a `membershipExpression` claims nothing
(`pass.layer_membership_expression`). It runs before the light links are resolved, which
consume the receivers.

The table is empty when the pass declares no layer of its own: `World::layer` then answers
0, `default`, for every id, and nothing is looked up per sample beyond that.

*Alternative:* resolve the layer at hit time from the prim path. Rejected: a hash lookup
per camera sample, and hits carry no path.

### D4. `FirstHit` carries the owner's layer

`FirstHit::Surface` and `FirstHit::Volume` carry `layer`, filled where the integrator writes
them: `World::layer` of the hit (of the thin wall, in `first_wall`), the scattering region's
layer (`VolumeEvent::Scatter::layer`, the first region with density at the point, beside its
light class), and `World::default_layer` for a camera inside a carried medium. `Escaped` is
the layout's default layer. Ownership is decided by the code that decides the alpha and the
geometric AOVs, so a layer's alpha and colour cannot disagree about who owns a sample.

*As built:* the design first had `FirstHit` carry the raw `geom_id` and region index for the
film to map. Resolving in the integrator gives the same answer and keeps the film free of
tables.

### D5. Film slots keyed by (var, layer)

`AovVar::layer` and `SlotKey::layer` (see `aovs/design.md` § Render layers). A sample adds
only to its owner's slots, which is the holdout; a layer's beauty is `C.*[LO]` in its
layer's slot. `sampleCount` and `variance` stay per pixel: adaptive sampling stops on the
beauty. The light-path inspection and the MCP session do not use layers.

*Alternative:* one `AovFilm` per layer. Rejected: N films are N pixel-state walks and N
allocation sites, where one wider slot vector is a field and a comparison.

### D6. Patterns become products, and the writer is unchanged

In `crust-render`, `products::Pattern` parses a pattern once into literal and specifier
tokens. `pass_products` expands it for every (layer, var), layer-major, groups the outputs by
expanded path (compared lexically, as `refuse_shared_paths` compares) in first-occurrence
order, and makes each group one synthesized `AovProduct` whose prim is the pass. The existing
writer, header stamp and channel naming then apply unchanged: a group of one output keeps a
product's names; a group of several sets each var's `channel_prefix` to its layer name (D8).
`refuse_shared_paths` is not called on synthesized products — there a shared path is the
grouping — and still guards authored products.

The pass's vars are every RenderVar of its settings' products (`products::pass_vars`), each
prim once, in product order; settings that name no product give the pass one var, `Beauty`
(`color4f`), so a pass always writes its layers. Products' authored `driver:parameters:*`
are not copied (Known gaps); crust's stamp is.

The refusals (an unset `$(NAME)`, an extension other than `.exr`, two outputs of one file
with one layer name) are raised after the import, before the render: an authored pattern
is read by the import. `check --pass` runs the same function and fails the same way.
Environment variables are read in `crust-render` while the pattern is expanded: they are
path inputs of the command, not behaviour switches, so the "`Config` is the only place the
environment is read" rule (it covers `CRUST_*`) does not apply.

`--checkpoint` writes no preview with `--pass`: a pass's previews are its layers', which
exist only once the film is gathered, and the progressive snapshot holds only the beauty.

### D7. Two dialects, two inputs, one set of path rules

`productName` and `-o` without `--pass` keep husk's dialect (`$F4`,
`output-path-frame-tokens`). A pass's pattern, and `-o` with `--pass`, use the pass
pattern dialect (`$04f`). The branch is taken once, where `-o` is routed (`render_and_write`, `pass_files`):
with `--pass` it never reaches `expand_frame_vars`, so `$n` or `$x` in it never raises a husk
`product.unknown_variable`.

Everything the dialects share follows `output-path-frame-tokens`, so a shot cannot get two
answers for one frame: the frame is `-f`, 1 without it; a subframe rounds halves away from
zero (`FrameVar::format`, shared, not copied) with one `product.subframe_frame` per pattern;
padding counts the sign; `\$` is a literal `$`; an unknown name is kept as written with one
`product.unknown_variable` per pattern naming the pass, and a lone `$` raises nothing.

**Sources (task 1.1).** The specifiers were pinned against a reference renderer's
documentation of its file naming and render settings. No running copy was available.

| specifier | reference | crust |
|-----------|-----------|-------|
| `$l` | the pass path, prefix and hierarchy included | the pass prim's name. **Differs**: USD has no reference prefix, and a prim path would add directories (the user's choice) |
| `$L` | the pass name without prefix or hierarchy | the pass prim's name |
| `$n` | the layer name | the layer's name |
| `$o` | the output (AOV) name | the RenderVar's name (`driver:parameters:aov:name`, else the prim's) |
| `$c` / `$C` | camera name with / without prefix | both the render camera prim's name |
| `$e` | the eye number, 1 for the leftmost | `1` |
| `$E` | the eye name, `left` / `right` | `left` |
| `$f`, `$0Nf` | frame, `$03f` zero-padded to 3 | as the reference; the sign counts in the padding |
| `$x` | the default image extension | `exr` |
| `$(NAME)` | an environment variable, in any path | as the reference; unset is refused (crust's choice: undocumented) |
| `$_` | an underscore after the `$` lower-cases it | as the reference, on every specifier, `$(NAME)` included |
| subframe | undocumented | the shared rule |
| literal `$` | undocumented | `\$`, the shared rule |

The parser reads a specifier as one character after the `$` (or `0N f`, or `(NAME)`); any
other `$` followed by a name character is unknown, its whole name kept (`$SHOT`, husk's
`$F4`). crust's default is `$l_$n_$o.$04f.$x`: a default rooted at an environment variable
would be refused on most machines, and `$e` is constant for one view.

### D8. Single-part multichannel files, channels named by the layer pattern

Grouped outputs go into one multichannel EXR, as products already are (the user's
decision). Each output inside a shared file is named by the pass's **layer pattern**, as the
reference renderer does (the user's decision after task 1.1): `crust:layerPattern` on the
pass, default `$n_$o`, expanded with the file pattern's specifiers. In a file of several
outputs, each output's channels are `<layer name>.<channel>` (`hero_Diffuse.R`), the beauty
included; a file of one output keeps a product's names (the beauty bare), which is what a
one-AOV-per-file pattern gives comp. Two outputs of one file with one layer name would
overwrite each other's channels, so they are refused. A scalar output's channel is its layer
name alone (`hero_Depth`), as a product's scalar is its var name.

The reference channel list (task 1.2: an `exrheader` of a reference file written with a
pattern that omits `$n`) is not available yet. Whether the beauty of a several-output file
is written bare is the one point the documentation leaves open.

Multi-part EXRs, one part per layer with its own data window, were considered and deferred:
they need the products module to write parts, and comp has not asked for them.

### D9. The default render is untouched

A render without `--pass` reads the settings prim it always read (D1), installs no layer
table, and requests no layered var; the `AOV = false` instantiation of the integrator never
reads a layer. Gates: `scripts/check_images.sh check` on goldens recorded by the binary
before the change reports every scene bit-identical, `render_passes.usda` included (task
7.1), and the callgrind count on cornellbox at `-s 2` (Measurements).

## Measurements

Instruction counts, callgrind, `RAYON_NUM_THREADS=1`, release build, `-s 2`.

**The default render (task 7.1).** cornellbox, no pass: 4,250,409,816 instructions before the
change, 4,250,460,332 after (+50,516, 0.001%). Two runs of the *same* pre-change binary differ
by 30,175 (4,250,409,816 and 4,250,439,991: the progress and timing paths), and the eleven
hottest functions — `advance_pixel`, the BVH and the shading — have identical counts in both
binaries, so the difference is that noise, not the render loop. `samples/render_passes.usda`
without `--pass`: 394,820,300 → 395,519,681 (+0.18%), in the import (the settings path now
an input, the pass check). `scripts/check_images.sh check`, goldens recorded by the
pre-change binary: every scene bit-identical, this capability's sample included.

**A layered render (task 7.2).** `samples/render_passes.usda` at its 160×90, its three vars
(`Beauty` color4f, `Diffuse` LPE, `Depth`), every output in one file (`-o '$l.$x'`): the
pass `render` with three layers against the same settings as a pass with one layer.

| | one layer | three layers | difference |
|-|-----------|--------------|------------|
| whole run | 422,774,454 | 472,058,084 | +49.3 M (+11.7%) |
| `advance_pixel` (tracing, incl. `UnitAov::add`) | 274,356,118 | 276,666,270 | +2.31 M |
| `UnitAov::add` | 13,762,793 | 16,072,945 | +2.31 M |
| `render_pass` (the gather into the film) | 8,391,684 | 17,333,776 | +8.94 M |

The per-sample overhead of two more layers is **about 80 instructions per camera sample**
(2.31 M over 28,800 samples): `UnitAov::add` walking three times the slots, two thirds of
them skipped at once. The gather adds about 620 instructions per pixel (three times the
planes resolved). The rest of the difference, about 38 M, is output: three times the
channels compressed into the EXR. The light transport costs the same at any layer count, as
D4 requires: the traversal and shading counts are unchanged.

## Known gaps

- **Pass visibility collections.** A pass's `renderVisibility` and `cameraVisibility` are
  not read: they change the lighting world, which needs a separate traversal. Warned
  (`pass.collection_not_read`) when they restrict anything. `prune` and `matte` are not read
  either, and not warned.
- **Per-pass overrides.** Lighting or material overrides per pass have no USD schema and
  are not supported.
- **Instance splitting.** The instances of one point instancer are all in its layer, and a
  layer target inside a prototype is judged on the instance or instancer that draws it
  (`pass.layer_target_in_instance`). Splitting an instancer by `placement` is a follow-up.
- **Per-layer AOV lists.** Every layer gets every RenderVar of the pass.
- **A transmit-layer switch.** Glass owns what is seen through it. Comp sometimes wants the
  hero seen through a window in `hero`; the ownership rule is one function
  (`FirstHit`'s layer), where a per-object switch could go.
- **Multi-part files** (D8), and several passes in one invocation.
- **`driver:parameters:*`** of the settings' products are not copied into a pass's files.
- **Channel names unconfirmed against a reference file** (D8, task 1.2).
- **A guided render is not repeatable bit for bit** (`rendering`'s known gap: the last
  pass is chosen from a wall-clock efficiency), with or without layers. A layer's beauty
  equals its own render's beauty bit for bit, guided or not; comparing it with a second
  guided render is only meaningful unguided (`one_layer_is_the_beauty`).
