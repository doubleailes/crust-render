## Why

`live-stage-scene-deltas` made a confined MCP edit cost milliseconds instead of a
re-import, and on a look-development stage it does: the DPEL teapot goes from 22.6 s
per edit to ~1 ms. On ALab, the production shot it was meant for, **every edit is
still a full re-import (~105 s)**. Its `notes.md` measured this and a debug session
named one refusal per class, each one broader than it needs to be:

- **settings**: the shot uses adaptive subdivision (`crust:subdivisionEdgeLength`), and
  the refresh refuses whenever adaptive subdivision is on, because it reads the
  camera. That includes an exposure change, which leaves the tessellation as it was.
  Implicit shapes sized on screen are refused the same way.
- **lights**: the light rig authors light links, and both the rebuild and
  `LightList::check_replace` refuse any light in a linked scene.
- **materials**: the stoat's looks arrive through a payload. The session's
  authoring stage keeps payloads unloaded, so the classifier cannot see that the edited
  prim is a `Shader`, and it classifies the edit as `full`.

Production shots author all three: adaptive or camera-driven tessellation, light
linking, and looks delivered by payloads. Until these refusals are narrowed, the
in-place path pays off only on toy stages.

## What Changes

- **A settings edit on a camera-tessellated scene stays in place when the
  tessellation's input does not move.** The refresh recomputes what adaptive
  subdivision and on-screen implicit shapes read from the camera, with the function the
  import reads it with. It refuses only when that value differs bit for bit from the
  one the import used. Exposure, depth of field, the shutter, `crust:*` sampling
  settings and the AOVs stay in place. A camera move, a focal length or aperture
  change, a resolution change or a new edge-length target is still `full`.
- **A material input under a payload classifies as `materials`.** When the edited
  prim is beneath a material the scene uses, the classifier reads its type from the
  same payload-loaded stage, masked to the material, that the rebuild already composes.
  It no longer reads it from the payload-unloaded authoring stage. Every existing
  refusal (another reader, a displacement, a structural signature change) still
  applies.
- **A light input on a light-linked scene stays in place.** Link resolution reads only
  the `collection:lightLink` / `collection:shadowLink` relationships, and an edit to
  those is already `full`, so an `inputs:*` edit cannot move the resolved links. The
  provenance records each light's index *after* resolution, which renumbers the list
  when a light illuminates nothing. A light that resolution demoted (to a camera-only
  backdrop, camera-only geometry, or nothing) is not in the list, so an edit to it is
  `full`. The replacement keeps the list's resolved links, shadow masks and backdrops.
- Each of the three is pinned by the existing bit-identity pair (in place ↔ full
  re-import), on a scene that triggered the old refusal.
- ALab's measurement in `live-stage-scene-deltas`'s notes is re-run. The
  `mcp-session` Known gap "ALab applies nothing in place" is retired or narrowed to
  what remains.

Not changing: what counts as `full` in the layer diff (design D1 of
`live-stage-scene-deltas`), the switch `CRUST_SCENE_DELTAS`, and `crust render`.

**Ordering:** this change modifies a requirement that `live-stage-scene-deltas`
adds. That change must be archived first, so the requirement exists in
`openspec/specs/mcp-session/spec.md`.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `mcp-session`: the "Edits apply in place when they are confined" requirement no
  longer refuses a settings edit just because the scene's tessellation reads the
  camera, a light input just because the scene authors light links, or a shader
  input just because its material arrives through a payload.

## Impact

- `crust-core`:
  - `live.rs`: `StageFacts` keeps the screen projection the import tessellated with,
    in place of the `adaptive` and `on_screen_shapes` booleans. `Provenance` keeps
    light indices after link resolution, and marks demoted lights.
  - `scene/usd_import/live.rs`: `refresh_settings` compares projections. The light
    rebuild drops its `linked` refusal, and gains a lookup "which material owns this
    prim" on a masked, payload-loaded stage.
  - `scene/usd_import/light_links.rs` / `mod.rs`: `resolve` reports the index remap
    it applied, and the import applies it to `light_origins`.
  - `light/list.rs`: `check_replace` accepts a linked list when the index is a light
    the list still holds.
- `crust-render`: `mcp/delta.rs` `owning_material` falls back to the material's
  composed stage when the authoring stage does not compose the prim.
- Tests: session bit-identity cases on `subdivision_adaptive.usda`,
  `light_linking.usda` and a written stage whose material sits behind a payload.
- Docs: the `mcp-session` design record (refusal list, Known gaps), and
  `site/content/docs/help/claude-desktop.md` (what applies in place).
- No new dependency, no `unsafe`, no render-time cost: every change is in the
  session's update path or in import-time bookkeeping.
