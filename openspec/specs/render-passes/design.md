# render-passes — design record

> Design record for the **render-passes** capability (`--pass`, `--all-passes`,
> `UsdRenderPass`'s `renderSource`, `matte`, `cameraVisibility` and
> `renderVisibility`): the reasoning, measurements and history behind the
> behaviour `spec.md` states. Introduced by the
> `render-pass-matte-and-camera-visibility` change (issue #340) and extended by
> `render-visibility-and-all-passes` (issue #348); their design is carried here
> with what implementation found. `docs/architecture.md` is the map;
> the user's view is `site/content/docs/usd/render-passes.md`. What a holdout does
> at the camera vertex, and its cost, is in `openspec/specs/rendering/design.md`
> § Holdouts at the camera vertex.

## Why

Production renders are split into passes: a character pass holds out the props
rendered in another pass, and a background pass holds out the characters. Without
a holdout object the split took extra renders and a depth comparison in
compositing, wrong wherever the occluder is partly transparent or the edges
interpenetrate. OpenUSD already describes passes: a `UsdRenderPass` carries a
`matte` collection ("matte objects render with zero alpha") and a
`cameraVisibility` collection, so one stage holds every pass, with no asset edits.

## Context

What the design was built on, as the code stood before it:

- **Settings selection.** `render_settings_path(stage)` (`usd_import/settings.rs`)
  answers "which `RenderSettings` prim" from `renderSettingsPrimPath`, else
  `/Render/settings`. Every settings reader goes through it: the camera, the
  products, `domeLightCameraVisibility`, exposure compensation, the subdivision
  settings and the colour space. The MCP provenance records the same prim, so an
  edit to it is classified `Settings`.
- **Per-geometry flags.** `WorldBuilder` holds one `RayMask` per geometry
  (`set_mask`, bit 0 camera, 1 shadow, 2 indirect, 3–31 shadow-link classes) and
  a `transparent_emitters` bool per geometry, read through
  `World::has_transparent_emitters()` and `is_transparent_emitter(id)`.
- **Collections against streamed geometry.** `LightLinks` (`light_links.rs`)
  records `(first geom_id, interned stage path)` for every prim that emits
  geometry (`saw`, called unconditionally from the traversal). It evaluates the
  light collections after the last chunk. `demote` reads the camera bit to decide
  whether a light linked to nothing becomes a backdrop.
- **Infinite lights' camera visibility** is an escape mask per light in
  `LightList` (`light/list.rs`). `domeLightCameraVisibility = false` clears
  `MASK_CAMERA` from all of them.
- **The camera segment.** `pass_cutouts` (`tracer/path.rs`) decides where a
  segment really ends (stochastic cutouts, thin walls, hidden light sources). The
  AOV instantiation then records a `FirstHit` (`Escaped`, `Volume`, `Surface`).
  `sample_value` (`aov.rs`) maps it to each source. An escaped sample ranks last
  (key `+inf`) in closest accumulation.
- **The MCP session** imports through the CLI's own argument parser
  (`mcp/scene.rs` builds `crust check -i <layer>`; `render_final` builds
  `crust render -i <layer>`). `mcp/delta.rs::classify` allow-lists edits that
  apply in place. Anything else re-imports.
- **openusd** provides `render::Pass` with `COLLECTION_MATTE` and
  `COLLECTION_CAMERA_VISIBILITY`. `CollectionAPI::membership_query` returns a
  query that resolves includes, excludes, expansion rules, nested collections
  and `membershipExpression`, compiling the expression against the stage the
  collection was read from.

## D1. The active pass is a host input, not stage state

`UsdImportOptions` gains `pass: Option<String>`, filled from `--pass` or the
session's `pass`. Unknown or non-`RenderPass` paths are an import error, as an
unknown `--camera` is.

- **Rejected: auto-selecting the stage's only pass.** Adding a second pass to a
  stage would then silently change what an existing command line renders.
  Hydra's `activeRenderPassPrim` lives in the application's scene globals for
  the same reason.
- **Rejected: a `crust:activePass` attribute on the settings prim.** One stage
  serves every pass. An attribute would bring back the per-pass override layer
  that option B exists to avoid.

## D2. One settings resolver, pass first

`render_settings_path(stage)` becomes `render_settings_path(stage, pass)`. It
returns the pass's `renderSource` target when that target is a `RenderSettings`
prim, otherwise the existing answer, with a `render_pass.render_source_invalid`
warning when a target exists but is not settings. Every reader already goes
through this function, so no reader changes. The MCP provenance's settings prim
comes from the same call, so an edit to `charSettings` in a session on the char
pass is still a `Settings` edit.

## D3. Membership is decided at dispatch, on the live chunk stage

For each chunk, the import builds the pass's two membership queries once, from
that chunk's stage. When the traversal dispatches a prim that emits geometry (the
same call site as `LightLinks::saw`), it asks both queries about the prim's
stage path. It then writes the answer straight into `WorldBuilder` for that
prim's geom-id range: the holdout flag, and the camera bit cleared.

- **Why at dispatch and not after the last chunk, as light links do:** a light
  can be traversed after the geometry it links, so light linking must wait. The
  pass is known before the traversal starts. An expression with a predicate
  (`//{isa:Mesh}`) needs the prim itself, and only the chunk stage that holds
  it, payloads loaded, can answer. The index stage has no payloads.
- **Rejected: building the queries once on the index stage.** Same reason:
  predicate expressions would fail on payload contents. Path-only rules would be
  right either way, so this costs nothing for the common case.
- **Ordering falls out for free.** Flags are written during the traversal and
  `LightLinks::resolve` runs after it, so `demote` already sees the pass's
  camera bit (spec: "Camera visibility is decided before light linking").
  A test pins this order anyway.
- Lights are judged the same way when they are added: an area light's emitter
  geometry gets its camera bit cleared; an infinite light's escape mask loses
  `MASK_CAMERA`. A light at infinity in `matte` is treated as excluded from
  `cameraVisibility` (spec: "Lights at infinity follow the pass"). For such a
  light, zero colour plus zero alpha is the same result as being hidden, so it
  gets no second code path.
- Targets inside an instance prototype are detected with light linking's
  `inside_instance` and warned once per collection
  (`render_pass.target_in_instance`).

## D4. The holdout flag lives beside `transparent_emitters`

`WorldBuilder` gets `holdouts: Vec<bool>` (empty when no geometry is a holdout)
and `World::has_holdouts()` / `is_holdout(geom_id)`, shaped exactly like the
transparent-emitter pair.

- **Rejected: a `RayMask` bit.** Bits 3–31 already carry shadow classes, and the
  mask means "this ray category sees it". A holdout is seen by the camera and
  then ends the path, so masking it out would show what is behind it, which is
  `cameraVisibility`'s semantics, not `matte`'s.
- **Rejected: a holdout material wrapper.** Instances share materials, so the
  flag would follow the material, not the prim. It would also add a vtable hop to
  every shading query and enter the `resolve` / `Signature` pairs for nothing.

## D5. The camera vertex checks once, after the pass-through walk

On a camera ray only, after `pass_cutouts` has settled the segment's end:
`if world.has_holdouts() && world.is_holdout(hit.geom_id)`, the path ends with
zero contribution. The check sits inside the existing camera-vertex branch and
behind `has_holdouts()`, so a world without holdouts runs one well-predicted
branch.

- A cutout holdout is met with its opacity's probability by the existing walk,
  then ends the path. Partly present holdouts need no further code.
- Camera rays never meet hidden light sources: the camera mask leaves them out.
- A thin wall passed before the holdout makes `first_wall` the recorded hit, so
  alpha is 1 and colour 0. That is the documented gap; nothing is special-cased.
- A volume scatter before the surface means the segment never reaches the
  holdout, so the path continues normally.

## D6. AOVs see a holdout sample as an escape

The integrator records `FirstHit::Escaped` for a holdout sample and its light
path expression terms are all zero. Every AOV then takes the escaped arm:
alpha 0, the clear value for data sources, and key `+inf` in closest
accumulation, which makes depth at an edge pick the visible surface.

- **Rejected: a `FirstHit::Holdout` variant.** No source distinguishes it from an
  escape, so a variant would only add match arms that must stay identical. Add
  one when a source needs the difference (a deep output, a shadow catcher).
- `C.*[LO]` stays bitwise equal to the beauty: both are 0 on the same samples.

## D7. The session holds its pass and passes it on

`open_session` stores `pass` and appends `--pass <path>` to the argument lists
`check_args` and `render_final` already build. The image is therefore the CLI's
by construction. The pass is not written to the override layer, so resuming
means passing it again. Edits to the pass prim's collections are relationship or
non-allow-listed attribute edits, which `classify` already sends to a full
re-import. A test pins that.

## D8. Warnings

New `WarningCode`s, one per cause, listed in
`site/content/docs/reference/warnings.md`:

- `render_pass.collection_not_read`: `prune` restricts something.
  (`renderVisibility` raised it too until #348 read it.)
- `render_pass.render_source_invalid`
- `render_pass.target_in_instance`
- `render_pass.volume_ignored`: a volume region in `matte` or excluded from
  `cameraVisibility` or `renderVisibility`.
- `render_pass.nested_not_composed`: a pass collection includes a collection
  that a streamed chunk does not compose.

A missing or non-pass `--pass` is an error, not a warning.

## Measured: the two-pass composites

On `samples/render_passes.usda` (its camera focused on the hero at `fStop` 4)
at 48×27, unclamped, adaptive sampling off (`crust-core/tests/render_passes.rs`,
the composite tests):

- **Holding each other out, `plus`.** The char and props passes sum to the
  no-pass render at 16 and 64 spp: RMS < 1e-6 for the beauty, the alpha and a
  `C<RS>.*[LO]` light path expression. `char over props` is off by 0.0107 at
  16 spp and 0.0104 at 64: a bias, not noise.
- **Double counting.** With `/World/Sky` dropped from the char pass's `matte`,
  `char + props` equals the no-pass render plus a pass that holds out every
  object and keeps only the sky (RMS < 1e-6 in all three).
- **Hidden in back, `over`.** The props pass leaves the hero out of
  `cameraVisibility` instead of holding it out. `char over props` matches the
  no-pass render to < 1e-6 wherever the char alpha is 0 or 1. At an edge pixel
  the error is `α(1 − α)·(E[background | hero] − E[background | not hero])`:

  | spp | edge pixels | RMS, all edges | sky-backed (n) | floor-backed (n) |
  |-----|-------------|----------------|----------------|------------------|
  | 16   | 91  | 0.00591 | 1.4e-8 (40) | 0.00694 (37) |
  | 64   | 106 | 0.00401 | 3.6e-8 (44) | 0.00474 (39) |
  | 256  | 119 | 0.00331 | 9.2e-8 (48) | 0.00341 (46) |
  | 1024 | 128 | 0.00347 | 3.1e-7 (54) | 0.00375 (44) |

  (Edge pixels whose background is part sky, part floor are in neither
  column.) Against the uniform dome both conditional means are equal, so the
  composite is exact to float rounding. Against the floor the error falls
  while noise dominates, then stops near 0.0035: what lies behind the hero's
  silhouette is its own contact shadow, so background and coverage are
  correlated. 64× the samples divide it by 1.85, where noise alone would
  divide it by 8. `over` is exact at an edge only where the background behind
  it does not depend on which side of the edge a sample lands. (Measured
  first with a pinhole camera, before the sample's lens: 0.0072 → 0.0056 over
  the same range, the same conclusion.)

## Measured: the shared import (`--all-passes`)

Wall-clock seconds, release build, 4 cores, five interleaved rounds (one
`--all-passes` run, then the four `--pass` runs, per round, so background load
lands on both), each render at 1 spp over a 16×16 region so the import
dominates; the import's own figure is `--stats`' "Parse USD stage"
(`render-visibility-and-all-passes` task 8.1).

| stage | import, one pass | import, the group of 4 | `--all-passes` min / mean | 4 × `--pass` min / mean |
|-------|------------------|------------------------|---------------------------|-------------------------|
| `samples/render_passes.usda` | 0.006 s | 0.003 s | 0.0227 / 0.0235 | 0.0589 / 0.0628 |
| 6000 unique catmull-clark cubes at level 3, ~970 MiB peak | 4.57 – 4.80 s | 4.71 s | 5.47 / 5.74 | 19.34 / 19.75 |

The second stage is synthetic (no production stage is in the repository): the
sample's four passes over 6000 jittered cubes, so each is refined on its own.
The group's import costs what one pass's does — judging four passes'
collections at dispatch is lost in the noise — so N passes cost one import
plus N renders: 3.4× faster for four passes there. What a pass adds over a
render of it alone (its `MaskTable` and light list copied, `remask` over the
built tree, its products written) is 0.26 s per pass against 0.24 s for the
process of a `--pass` render past its import.

## Risks and trade-offs

- **[Per-prim query cost on huge stages]** Moana-scale traversals dispatch
  hundreds of thousands of prims. → Queries are built only with an active pass,
  and a collection that restricts nothing (`matte` with `includeRoot = 0` and no
  rule, `cameraVisibility` with `includeRoot = 1` and no rule) is skipped
  entirely, as `link_query` skips a default link. Measure on the Moana profile
  with a one-include matte.
- **[Expression compile per chunk]** Each streamed chunk rebuilds the query. →
  This is one compile per chunk, not per prim. It is listed in `--stats` import
  phases if it shows up.
- **[Settings resolver signature change]** Every caller of
  `render_settings_path` must pass the pass. Missing one would read the wrong
  settings silently. → Each caller is touched by the signature change, so the
  compiler finds them all. A test checks that products, camera and spp all come
  from `renderSource`.
- **[Thin wall over a holdout]** Renders opaque black. → Documented gap; Arnold
  behaves comparably for refraction.
- **[Membership disagreement with light linking]** Both judge by the emitting
  prim's stage path. → Shared `inside_instance` helper and the same dispatch
  site.

## Known gaps

- `prune` is not read: it warns `render_pass.collection_not_read` and renders
  as unauthored. It changes what the traversal emits (streaming, instancing,
  placement count), which passes sharing one import cannot each change.
- Volume regions are neither held out nor hidden by a pass
  (`render_pass.volume_ignored`): a camera ray inside a held-out volume needs its
  own transport rule.
- A holdout behind a thin wall the camera ray passes straight through (glass at
  IOR 1) renders black with the wall's alpha of 1, not as a hole (D5).
- No shadow catcher: a holdout records no shadow or reflection of the other
  objects into alpha (the RenderMan `holdouts` LPE prefix stays refused).
- Geometry a pass hides from every ray stays in the acceleration structure the
  passes of an import share: rays skip it, but the pass still pays to traverse
  it, and rendering that pass alone with `--pass` imports no less.
- `over` is exact at an edge only where the background behind it does not depend
  on coverage (see the measurements above).
- In a streamed import, a pass collection that includes another collection
  authored in a different top-level subtree cannot read it while that subtree's
  chunk is imported (`render_pass.nested_not_composed`).
