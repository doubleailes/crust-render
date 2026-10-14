## Context

See `proposal.md` for why. The in-place machinery of `live-stage-scene-deltas` is
unchanged: the layer diff and the classifier (`crust-render/src/mcp/delta.rs`),
`Provenance::rebuild` (`crust-core/src/scene/usd_import/live.rs`), and `Update::check`
/ `apply`. Each of the three refusals sits at one point in that path:

- **settings.** `refresh_settings` returns `Structural` as soon as
  `StageFacts::adaptive` or `StageFacts::on_screen_shapes` is set. Both flags stand for
  one dependency. `subdiv_policy` (`usd_import/mod.rs`) reads the render camera into a
  `ScreenProjection` (`eye`, `f_px`, and the frustum built from `forward`, `up`,
  `tan_v`, `tan_h`) through `camera::screen_projection`, and wraps it in a `ScreenRate`
  with the edge-length target and the level ceiling. Adaptive subdivision and
  `ImplicitPolicy::radial_count` read nothing else from the camera. `read_index`, which
  the refresh already calls, returns that same policy. The refresh can therefore see
  the new projection; it just never compares it.
- **lights.** `Rebuilding::light` refuses when `StageFacts::linked` is set, and so
  does `LightList::check_replace` when the list has links or backdrops.
  `LightLinks::resolve` then runs after the last chunk. When a light's `lightLink`
  includes no receiver, it `demote`s that light (`LightList::remove`), which shifts
  every later light down one place. The provenance's `LightOrigin::index` is recorded
  at emission (`light_origins`, before resolution), so on a linked scene it can name
  the wrong light. The refusal is what currently keeps that from mattering.
- **materials.** `owning_material` reads the edited prim's type from the session's
  authoring stage, opened with payloads unloaded (design "Risks" of `mcp-session`). A
  shader inside a payload is not composed there, so the classifier finds no `Shader`
  and returns `full`. The material rebuild already composes a stage masked to the
  material with payloads loaded (`Rebuilding::masked`).

## Goals / Non-Goals

**Goals:**

- Each relaxation is exact, not approximate. An edit stays in place only when the
  scene a fresh import would build is provably the one the update builds, and the
  existing bit-identity pair pins each one.
- Keep each decision on values the import computes, not on an attribute-name list
  beside it. A list goes stale the day `CameraFrame::read` reads one more attribute.

**Non-Goals:**

- A light's transform, shape or link collections (still `full`, design D1 of
  `live-stage-scene-deltas`).
- Re-tessellating in place when the projection *does* move. A camera move on an
  adaptive scene remains a full re-import.
- Loading payloads in the authoring stage. Its unloaded payloads are what keep the
  session's queries cheap.

## Decisions

### D1. Settings: compare the projection the tessellation read

`StageFacts` replaces the `adaptive` and `on_screen_shapes` booleans with keys of what
they stood for: the `ScreenRate` adaptive subdivision used (or `None`), and the
`ScreenProjection` the import passed to `ImplicitPolicy`, but only when some shape's
count came from it (`on_screen`). `refresh_settings` takes the policy `read_index`
returns and refuses only when either recorded value differs from the new one. Floats
are compared by `to_bits`, so `-0.0`, `NaN` and rounding cannot pass for equal.

The new value comes from `read_index` → `subdiv_policy` → `screen_projection`, the
functions the import calls. It does not come from a second reading of the camera, so
the check moves exactly when what the tessellation sees moves. The level-ceiling and
level checks the refresh already makes stay.

*Alternative:* an allow-list of camera attributes that cannot move the projection
(`exposure`, `fStop`, `focusDistance`, `shutter:*`). Rejected: it duplicates
`CameraFrame::read`'s knowledge in the opposite direction. The projection also depends
on `RenderSettings.resolution` and the aspect-ratio policy, which an attribute list on
the camera prim would miss.

*Trap:* `frustum` is built only when `CRUST_ADAPTIVE_FRUSTUM` is on (`Config`).
The key holds exactly what the tessellation is handed (`eye`, `f_px`, and the
frustum's planes when there is one), not the camera inputs it was built from. With
the frustum off, the camera's orientation reaches no tessellation, so it is left out
of the key and does not cause a refusal. Both sides of the comparison run in one
process under one `Config`, so the switch cannot differ between them. The key is
`crate::live::ScreenKey`, a list of `to_bits()`, because `StageFacts` lives outside
`usd_import` and the projection types are private to it.

### D2. Lights: provenance indices after link resolution

`LightLinks::resolve` returns the remap it applied: for each light index at emission,
its index after the demotions, or `None` if it was demoted. `load_scene` applies the
remap to `light_origins` before building the `Provenance`. A demoted light gets no
entry, so `provenance.light(path)` is `None`. The classifier then falls through to
`full`, as it does for any prim that is not a scene light. That is the correct answer:
a demoted dome is now a backdrop, and a demoted area light is camera-only geometry.
Neither is a `LightList` entry that a `replace` could reach.

With indices correct, both refusals go. `Rebuilding::light` drops the `linked` check,
and `LightList::check_replace` drops its links/backdrops check. `replace` already
touches only `lights[index]`. The resolved links (`illuminates`, `shadow_masks`,
`restricted`, `nee_only`), the backdrops, the per-geometry light classes and the
shadow bits in the geometry masks are all keyed by index or `geom_id`, and none of
them is rebuilt. They stay as resolution left them. That is right because resolution
reads only the collections (`link_query`), and an edit to a collection is a
relationship or a non-`inputs:` attribute, so it is `full` under the allow-list.

`StageFacts::linked` goes with them. Nothing else read it: the settings refresh
never checked it, and no setting changes a link collection.

*Alternative:* re-run `LightLinks::resolve` on update. Rejected: it needs every
receiver path from the traversal, so it is a re-import in disguise.

*Trap:* the light rebuild's scratch context emits the light with an empty
`LightLinks`. Check that nothing in `emit_light` reads link state onto the
`LightKind` or the emitter's mask. A rebuild that did would rebuild an unlinked light.
The linked bit-identity test (D4) is what catches it.

### D3. Materials: verify the shader on the material's own stage

`owning_material` no longer needs the authoring stage to compose the prim. It looks for
the nearest ancestor path (or the prim itself) that the provenance lists as a used
material, as it does today. When the authoring stage does compose the prim, it keeps
the `Shader` type check there. When it does not (the prim sits under an unloaded
payload), it records the edit as a *material input to be verified*. The classification
stays `materials`, and the `Change` carries the edited prim paths.

`Provenance::rebuild` then verifies, on the payload-loaded stage masked to the
material that it composes anyway, that each such prim is a `Shader` (or the material
itself) in that material's network. Anything else is `Structural`, so the edit falls
back to `full` through the existing path. The other refusals (`other_readers`,
`closed_network`, displacement, signature) apply unchanged.

*Alternative:* load the one payload in the authoring stage
(`Stage::load(path)`). Rejected: it changes what the session's `query` sees and keeps
the composed payload for the rest of the session, against the stage's memory design.
*Alternative:* trust the path prefix alone. Rejected: a `Mesh` or `NodeGraph` nested
under a `Material` would then classify as a shader edit.

### D4. Each relaxation is pinned by the bit-identity pair

The session tests in `mcp/tests.rs` (`same_as_full_import`) gain three cases, each
on a stage that hit the old refusal:

- `samples/subdivision_adaptive.usda`: exposure → `settings`, focal length → `full`.
  A stage with an unshared implicit sphere and a camera (on-screen sizing): exposure
  → `settings`.
- `samples/light_linking.usda`: `inputs:intensity` on `Key` (shadow-linked), on
  `Fill` (light-link excludes) and on `Rim` (include-only) → `lights`. A written stage
  with a light whose `lightLink` includes nothing, placed *before* an edited light in
  traversal order so that the remap is exercised: the demoted light → `full`, the
  later one → `lights` and bit-identical.
- A written stage whose material is defined in a layer brought in by a payload:
  a shader input → `materials`. A `NodeGraph` input under it → `full`.

Each case also undoes, as the existing in-place test does.

## Risks / Trade-offs

- **[A projection field the comparison misses]** → The comparison is over the struct
  the tessellation is handed, so a new field there is a compile-time question. The
  focal-length `full` case shows the check is live.
- **[A linked light's update loses its links]** → The links are never rebuilt, only
  kept, and the linked-scene test compares beauty, AOVs and warnings bitwise with a
  full import.
- **[The remap is wrong for several demotions]** → The written stage demotes a light
  ahead of the edited one. A unit test on `resolve`'s remap covers two demotions on
  either side of a kept light.
- **[Verifying on the masked stage costs a composition]** → The rebuild composes that
  stage already. The check is a type read on prims it has open.

## Migration Plan

Nothing on disk or in the protocol changes: more edits report `applied = settings`,
`lights` or `materials` where they reported `full`. Rollback is
`CRUST_SCENE_DELTAS=0` or a revert. Archive `live-stage-scene-deltas` before this
change, since this change modifies the requirement that change adds.
