# Design

## Context

See `proposal.md` (Why) and the `render-passes` spec for the behaviour. The code
this change touches, as it is today:

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

## Goals / Non-Goals

**Goals:**

- The holdout costs one predictable branch per camera ray in a world without one.
  The zero-AOV instruction count and the goldens stay put.
- One answer to "which settings prim", so a pass's `renderSource` reaches every
  settings reader and the MCP provenance at once.
- Membership is judged exactly where light linking judges it (the emitting
  prim's stage path), so the two collection features cannot disagree about what
  "this prim" means.

**Non-Goals:**

- `prune` and `renderVisibility`. They change what the traversal emits, which
  touches streaming, instancing and the placement count. That is a separate
  change.
- Holding out or hiding volume regions. A camera ray inside a held-out volume
  needs its own transport rule.
- Rendering several passes from one import (swapping the holdout table between
  renders). The data layout below allows it, but this change does not do it.
- A shadow catcher (the RenderMan `holdouts` LPE prefix stays refused).

## Decisions

### D1. The active pass is a host input, not stage state

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

### D2. One settings resolver, pass first

`render_settings_path(stage)` becomes `render_settings_path(stage, pass)`. It
returns the pass's `renderSource` target when that target is a `RenderSettings`
prim, otherwise the existing answer, with a `render_pass.render_source_invalid`
warning when a target exists but is not settings. Every reader already goes
through this function, so no reader changes. The MCP provenance's settings prim
comes from the same call, so an edit to `charSettings` in a session on the char
pass is still a `Settings` edit.

### D3. Membership is decided at dispatch, on the live chunk stage

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

### D4. The holdout flag lives beside `transparent_emitters`

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

### D5. The camera vertex checks once, after the pass-through walk

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

### D6. AOVs see a holdout sample as an escape

The integrator records `FirstHit::Escaped` for a holdout sample and its light
path expression terms are all zero. Every AOV then takes the escaped arm:
alpha 0, the clear value for data sources, and key `+inf` in closest
accumulation, which makes depth at an edge pick the visible surface.

- **Rejected: a `FirstHit::Holdout` variant.** No source distinguishes it from an
  escape, so a variant would only add match arms that must stay identical. Add
  one when a source needs the difference (a deep output, a shadow catcher).
- `C.*[LO]` stays bitwise equal to the beauty: both are 0 on the same samples.

### D7. The session holds its pass and passes it on

`open_session` stores `pass` and appends `--pass <path>` to the argument lists
`check_args` and `render_final` already build. The image is therefore the CLI's
by construction. The pass is not written to the override layer, so resuming
means passing it again. Edits to the pass prim's collections are relationship or
non-allow-listed attribute edits, which `classify` already sends to a full
re-import. A test pins that.

### D8. Warnings

New `WarningCode`s, one per cause, listed in
`site/content/docs/reference/warnings.md`:

- `render_pass.collection_not_read`: `prune` or `renderVisibility` restricts
  something.
- `render_pass.render_source_invalid`
- `render_pass.target_in_instance`
- `render_pass.volume_ignored`: a volume region in `matte` or excluded from
  `cameraVisibility`.

A missing or non-pass `--pass` is an error, not a warning.

## Risks / Trade-offs

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

## Migration Plan

Additive. No flag or attribute changes default. Without `--pass`, the import
reads no pass and the integrator's only addition is the `has_holdouts()` branch.
Roll back by reverting. No data format changes; `crust-check/2` gains fields
(`pass`, `settings`, holdout and camera-hidden counts), which readers of the
report ignore when unknown.
