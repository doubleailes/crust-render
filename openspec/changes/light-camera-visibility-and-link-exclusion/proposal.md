## Why

The Moana island lights itself with two `DomeLight`s that do different jobs. One,
`sky_dome_env_llc` (`islandsun.exr`), is the HDRI: it lights and shadows the island,
and the camera should not see it. The other, `sky_dome_cam_llc`
(`islandsunVIS.png`), is a backdrop: the camera sees it in place of the HDRI, but
it lights nothing and must not block the HDRI for any other ray. Crust sums every
infinite light for every escaping ray and treats every light as illuminating, so
both domes light the island and both appear behind it. A measurement of the
unmodified island render found the island at R/G = 0.44 and B/G = 0.48 against
RenderMan's 0.92 and 0.34, and the sky clipped white. Part of that cool cast is
this second sky.

The scene says what it means through two signals crust does not read: camera
visibility (`primvars:ri:attributes:visibility:camera`, authored on both domes by
`islandPrman.usda`) and a light link that excludes the whole scene
(`collection:lightLink:excludes = </island>` on the backdrop in `island.usda`).
Full per-object light linking is proposed separately in `add-light-and-shadow-linking`.
This change covers the two cases a backdrop needs, and it can ship on its own.

## What Changes

- **Camera visibility for infinite lights.** A dome or distant light can be
  hidden from camera rays. A camera ray that escapes collects only the infinite
  lights visible to it. Bounce, refraction and shadow rays are unaffected. The
  flag is read from `primvars:ri:attributes:visibility:camera`, or from
  `crust:light:cameraVisible`, which wins when both are authored. Domes stay
  camera-visible by default, as they are today.
- **The same attribute on area lights.** `primvars:ri:attributes:visibility:camera`
  is read alongside the existing `crust:light:cameraVisible` for rect, sphere,
  disk and cylinder lights. Their default stays camera-invisible.
- **A light linked to nothing illuminates nothing.** When a light's
  `collection:lightLink` provably covers no geometry, the light is removed from
  light sampling and from every non-camera ray. This covers `includeRoot = 0`
  with no `includes`, and `excludes` paths that cover every receiver prim.
  Its NEE and bounce contributions both drop to zero together, so MIS stays
  consistent. A partial link (some geometry in, some out) keeps today's behaviour
  (the light lights everything) and logs one `WARN` pointing at
  `add-light-and-shadow-linking`.
- **Backdrops.** A camera-visible infinite light that illuminates nothing is a
  *backdrop*. The camera sees the backdrops alone, as if they were a surface at
  infinity in front of every other infinite light. Every non-camera ray behaves
  as if the backdrop did not exist. `island.usda` (link only) and
  `islandPrman.usda` (link plus explicit visibility) therefore give the same image.
- **No built-in sky.** The procedural gradient that escaping rays collected
  when no infinite light answered is removed. An escaping ray is black unless a
  light at infinity (or, for camera rays, a backdrop) answers it. **BREAKING**
  for scenes without a `DomeLight` or `DistantLight`: `samples/cornellbox.usda`
  has no light at all and was lit entirely by the gradient.
- **`domeLightCameraVisibility`.** Hydra's render setting of that name (the one
  hdEmbree/Typhoon and usdview use), read from the `RenderSettings` prim, with
  `crust:domeLightCameraVisibility` taking precedence. `false` hides every light
  at infinity and every backdrop from the camera, whatever the lights author.
- **Unauthored scenes with a sky are bit-identical.** Without these attributes,
  every scene that has an infinite light keeps today's visibility and lighting. `scripts/check_images.sh` verifies this.
- A sample `samples/dome_backdrop.usda` (an HDRI dome plus a flat-colour backdrop
  dome over a diffuse plane). The `lighting` and `usd-scene-import` design records
  are updated to match.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `lighting`: "Infinite lights" gains camera visibility, backdrops and the
  no-fallback rule. "Area lights" gains the RenderMan camera-visibility primvar.
  A new "Lights linked to nothing" requirement is added. "Known gaps" narrows
  light linking to partial links.
- `usd-scene-import`: "Light schema mapping" reads the camera-visibility primvar
  and the whole-scene `lightLink` cases, and stops listing light linking as
  entirely unread.

`add-light-and-shadow-linking` modifies the same "Light schema mapping" and
"Known gaps" requirements. Whichever change archives second must restate those
requirements with both changes applied. When full linking lands, the "illuminates
nothing" test becomes "the light's class set is empty". Backdrop semantics and
camera visibility stay as specified here.

## Impact

- `crates/crust-core/src/scene/usd_import/lights.rs`, `attrs.rs`: read the
  camera-visibility primvar, and read `collection:lightLink` just far enough to
  detect the two whole-scene cases.
- `crates/crust-core/src/scene/usd_import/light_links.rs` (new) and `mod.rs`:
  record each receiver prim's stage path (truncated), and after traversal
  demote the lights whose excludes cover every receiver. The decision is keyed
  by stage path, so it works across streamed chunks and does not depend on
  instance prototypes. `settings.rs` reads `domeLightCameraVisibility`.
- `crates/crust-rt/src/scene.rs`: `SceneBuilder::mask` / `set_mask`, so a
  demoted area light's geometry can drop to camera-only before commit.
- `crates/crust-core/src/light/list.rs`: a camera-visibility mask on the infinite
  lights, and a separate `backdrops` list outside light selection. Selection pmf,
  `density`, the light cache and guiding are untouched.
- `crates/crust-core/src/tracer/path.rs`: `escaped_emission` filters by the
  ray's category (`MASK_CAMERA` for primary rays), reads the backdrops for
  camera rays, and no longer falls back to the sky gradient.
- Performance: one mask test per escaping ray. There is no cost for lights on
  the sampling path, and no change when nothing is authored.
