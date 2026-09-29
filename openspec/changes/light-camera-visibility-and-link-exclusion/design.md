## Context

See `proposal.md` (Why) for the Moana setup. The requirements are in
`specs/lighting/spec.md` ("Infinite lights", "Lights linked to nothing") and
`specs/usd-scene-import/spec.md` ("Light schema mapping").

The code today:

- **Escaped rays.** `escaped_emission` (`tracer/path.rs:346`) loops over
  `LightList::infinite_at` and sums `Light::escaped` for every light at infinity,
  for every escaping ray. The ray's category plays no part. When no light
  answers, the caller (`path.rs:758`) adds the built-in sky gradient.
- **Ray categories already exist.** Camera rays carry `MASK_CAMERA`
  (`camera.rs:104`). Every bounce carries `MASK_INDIRECT` (`path.rs:668`,
  `path.rs:743`), including delta reflections and refractions. Shadow rays carry
  `MASK_SHADOW`. The ray's mask therefore answers "is this a camera ray"
  exactly as the spec defines it, with no new path state.
- **Area lights are world geometry.** Each one is attached with
  `light_ray_mask` (`usd_import/attrs.rs:33`), which already defaults to
  camera-invisible and reads `crust:light:cameraVisible`. Bounce rays hit that
  geometry, and `bounce_emission_weight` attributes the hit back to the light
  through its `geom_id`. Removing only the light-list entry would therefore
  leave the light shining through BSDF sampling, at unopposed weight.
- **Light and geometry order is not fixed.** Lights are added to `LightList`
  during traversal (`usd_import/mod.rs:317`), chunk by chunk when streaming
  (`CRUST_STREAM_IMPORT`, default on). A light can be traversed before the
  subtrees whose geometry decides whether its link covers anything.

## Goals / Non-Goals

**Goals**

- Make the backdrop-plus-HDRI rig render as authored from either `island.usda`
  (link only) or `islandPrman.usda` (link plus explicit visibility).
- Keep the NEE ↔ bounce pairs consistent by construction. A light that
  illuminates nothing is absent from the machinery, not weighted to zero in it.
- Keep the light path unchanged in cost and bit-identical when nothing is
  authored.

**Non-Goals**

- Per-object light linking and all shadow linking. Those belong to
  `add-light-and-shadow-linking`.
- RenderMan's other visibility primvars (`visibility:indirect`,
  `visibility:transmission`). They are documented as a gap.
- Camera visibility for emissive *materials*. They are not lights.

## Decisions

### D1. A backdrop is not a light

A light that illuminates nothing is never added to `LightList`'s sampled
entries. The alternative, keeping it in the list with a zero pmf and a
bounce-side zero weight, would put a zero in `density`, the light cache's
per-cell tables, guiding and learned selection, and each of those would need
the same exception. Keeping it out makes the "Lights linked to nothing"
requirement structural. No pmf, `density` or MIS weight can mention it.

Camera-visible, non-illuminating infinite lights go into a separate
`LightList::backdrops: Vec<LightKind>`. Only camera rays read it, and they run
no NEE, so the emission is taken at `unopposed_weight`. A camera-invisible
light that illuminates nothing is dropped entirely, with a `DEBUG` line: it
cannot be seen or felt.

For area lights, "illuminates nothing" also governs the geometry. It is
attached with mask `MASK_CAMERA` when camera-visible and not attached
otherwise, and it gets no light-list entry. With only the camera bit, no bounce
or shadow ray can hit it, so it can neither shine through BSDF sampling nor
occlude.

### D2. Camera visibility is a per-infinite-light mask, tested on the ray's category

Each sampled infinite light stores a `RayMask`: `MASK_ALL` by default, or
`MASK_ALL` without `MASK_CAMERA` when camera-invisible. `escaped_emission`
takes the ray's mask. A light whose mask does not `sees` the ray is skipped,
and a camera ray with a non-empty `backdrops` list reads the backdrops instead
of `infinite_at`.

This is the kernel's own visibility vocabulary (`crust_rt::RayMask`), so a dome
is "geometry at infinity" with a mask, like any other prim. The alternatives,
a `depth == 0` check or a `prev.is_none()` check, are both wrong:
`prev = None` also holds for rays leaving a carried-medium scatter, and depth
does not identify the camera ray once volumes are involved.

Skipping a light for camera rays needs no MIS change. Camera rays have no NEE
competitor, so every infinite light they see is already at `unopposed_weight`.
Hiding one from them does not touch any weight on a non-camera ray.

The "backdrops occlude every other infinite light for camera rays" rule is
applied in the tracer, not by rewriting the other lights' masks at import.
That keeps an authored `visibility:camera = 1` on the HDRI readable in the
light's `DEBUG` line, and puts the rule in the one place that implements it.

### D3. "Covers no geometry" is decided after traversal, by stage path

The importer classifies each light's `collection:lightLink` as it adds the
light (`usd_import/light_links.rs`):

- **Default**: nothing restricting it authored → illuminates as today. This is
  the fast path; nothing is recorded.
- **Nothing**: `includeRoot = 0` and no `includes`.
- **Excludes-only**: `excludes` authored and no `includes`. This is a candidate,
  judged after traversal.
- **Anything else** (`includes`, `membershipExpression`) → illuminates as today,
  plus one `WARN`.

A light can be traversed before the receivers its excludes cover, and when
streaming they can sit in different chunks, so the decision waits for the last
chunk. As it goes, the traversal records every receiver prim (mesh, sphere,
curves, volume, native instance, `PointInstancer`) by its **stage path
truncated to four components**, in a deduplicated set, with the last key
cached because a depth-first walk repeats it. That truncation is exact for any
exclude at most four deep: `e` names an ancestor of `r` iff it names one of
`r`'s first four components. It keeps the set to the stage's top few levels.
Moana's 3.1 M geometries collapse to a set of that size, where one path per
receiver would cost hundreds of MB. A deeper exclude is refused as partial.

After traversal, a candidate illuminates nothing iff every recorded receiver is
at or below one of its excludes (`/` covers everything). If none is, the light
is left alone. That is the Moana HDRI's case: its exclude names the backdrop
*light*, which is not a receiver. If only some are, the link is partial:
`WARN`, and it keeps lighting everything.

An earlier draft judged by top-level stream root instead. It was dropped
because the single-stage import has no roots to judge by, and a
per-receiver-prefix test gives the same answer on both import paths.

Judging by the prim that brings a receiver in (the instance or instancer prim)
is what makes the test independent of prototypes, whose geometry lives under
`/__Prototype_N`. When `add-light-and-shadow-linking` lands, its link classes
replace this test with "class set is empty", and D1/D2 stay.

**Demotion.** Lights are added to the list as they are traversed and demoted
afterwards (`LightLinks::resolve`, before `flush_meshes` and `commit`), highest
index first, through `LightList::remove`:

- A light at infinity that the camera sees becomes a backdrop; one it doesn't
  see is dropped.
- An area light keeps its geometry with its mask ANDed to `MASK_CAMERA`, via a
  new `crust_rt::SceneBuilder::set_mask`, or with no category at all.

Deferring the *emission* instead is not possible: a streamed chunk's stage is
dropped before the next one opens.

### D4. Attribute precedence

For camera visibility, the first authored source wins:

- **Area lights**: `crust:rayMask`, then `crust:light:cameraVisible`, then
  `primvars:ri:attributes:visibility:camera`.
- **Infinite lights**: `crust:light:cameraVisible`, then the `ri` primvar. They
  have no geometry, so `crust:rayMask` does not apply.

This keeps every crust attribute authoritative, with the RenderMan primvar
serving as the portable fallback that published assets already carry. The
primvar is an `int`, so non-zero means visible.

### D5. There is no built-in sky

The procedural gradient that escaping rays collected when no light at infinity
answered is removed. An escaping ray is black unless an infinite light (or, for
camera rays, a backdrop) answers it. This is a decision, not a side effect:
with camera-invisible domes, a gradient would appear behind the scene exactly
where the author asked for nothing. And a light nobody authored is not the
"production-inspired" behaviour this renderer aims for (hdEmbree/Typhoon clear
to a colour, RenderMan and Arnold to black).

The first draft kept the gradient whenever nothing covered a direction, but
that silently changed `DistantLight`-only scenes (rays outside the sun's cone
had the gradient). Removing the gradient outright is simpler and has no such
edge. It does change every sample with no infinite light, and
`samples/cornellbox.usda` in particular, which has no light prim at all
(`docs/light_sampling.md` §1). See Risks.

### D6. `domeLightCameraVisibility`

Hydra's render setting of that name (`HdRenderSettingsTokens`, which
hdEmbree/Typhoon read), authored on the `RenderSettings` prim, with
`crust:domeLightCameraVisibility` taking precedence. `false` clears the camera
bit of every infinite light's escape mask and drops the backdrops
(`LightList::hide_infinite_from_camera`) after link resolution. It wins over
per-light attributes, as in Typhoon, where turning it off shows the clear
colour. It changes no light's illumination.

Typhoon, for comparison: on a camera ray that escapes it checks this switch,
then **sums every visible dome, ignoring light links**. It has no per-dome
camera visibility (`visibleInPrimaryRay` applies to area-light shapes only).
The Moana rig would therefore show the HDRI and the backdrop added together
there. Backdrops (D1/D2) are what crust adds beyond it. Its linking is
receiver-based, judged on the previous vertex's categories, which is the model
`add-light-and-shadow-linking` already takes.

## Risks / Trade-offs

- **[Coarse link test misclassifies a real partial link]** → Only the
  excludes-only shape is ever classified as "nothing", and only when every
  geometry root is covered. Everything else keeps today's behaviour and warns.
  The failure mode is today's image, never a light silently disappearing.
- **[Deferred lights break the streamed import's ordering assumptions]** → The
  deferral is applied before the one place the light list is finalised.
  `CRUST_STREAM_IMPORT=0` gives the single-stage comparison the spec's
  "streamed import agrees" scenario asks for.
- **[Camera rays through primary-visible volumes]** → A camera ray that scatters
  in a volume and then escapes is a non-camera ray (its mask is
  `MASK_INDIRECT`). It sees the HDRI, not the backdrop. That is RenderMan's
  semantics and is what the spec states.
- **[Removing the sky blackens dome-less samples]** → `samples/cornellbox.usda`
  has no light, and 14 others relied on the gradient for fill and background.
  They change on purpose (the golden check shows exactly which), and those that
  should still be lit get an authored light. `docs/light_sampling.md`'s
  measurements on `cornellbox` describe the old scene and are marked as such.
- **[Two changes edit the same requirements]** → Called out in `proposal.md`.
  The second to archive restates "Light schema mapping" and "Known gaps" with
  both applied.

## Migration Plan

Scenes with a dome or distant light and none of the new attributes are
bit-identical (golden images). Scenes without one lose the built-in sky: author
a `DomeLight` to get a sky back. The island's two layers change as intended. Rollback is reverting the change. No
environment switch is added: this is a behaviour fix, not an optimisation to
A/B.
