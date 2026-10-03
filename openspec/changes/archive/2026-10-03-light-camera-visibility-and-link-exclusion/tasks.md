## 1. Camera visibility for infinite lights (`light/`, `tracer/`)

- [x] 1.1 Give each infinite light a `RayMask` (default `MASK_ALL`), held by
      `LightList` beside its infinite-light index (`add_masked`) rather than on
      the light types, so the lights stay pure emitters. Verify: `cargo test -p crust-core`
      passes unchanged.
- [x] 1.2 Pass the escaping ray's mask into `escaped_emission`, and skip
      infinite lights whose mask does not `sees` it
      (`LightList::infinite_seen_by`). Verify with a unit test in
      `tracer/tests.rs`: a camera-invisible dome returns black for a
      `MASK_CAMERA` escape and its radiance for a `MASK_INDIRECT` escape.
- [x] 1.3 Add `LightList::backdrops`, outside selection, `pmf`, `density`,
      `power` and the light cache. Make camera escapes read only the backdrops
      when the list is non-empty. Verify with a unit test: with an HDRI plus a
      backdrop, a `MASK_CAMERA` escape returns exactly the backdrop's radiance,
      a `MASK_INDIRECT` escape exactly the HDRI's, and the HDRI's selection pmf
      is bit-equal to a list without the backdrop.
- [x] 1.4 Verify the golden images: `scripts/check_images.sh check <dir>`
      (recorded on `main`) reports no difference.
- [x] 1.5 Remove the built-in sky gradient (design D5): an escaping ray no
      infinite light answers is black. `escaped_emission` returns the radiance
      alone. Verify: `nothing_at_infinity_is_black` and
      `ray_color_of_an_escaping_ray_without_lights_is_black` pass, and
      `a_different_frame_changes_the_noise_but_not_the_mean_much` is lit by an
      explicit dome.

## 2. Import: camera-visibility attributes (`usd_import/attrs.rs`, `lights.rs`, `settings.rs`)

- [x] 2.1 Read `primvars:ri:attributes:visibility:camera` (int, non-zero means
      visible) in `light_ray_mask`, after `crust:rayMask` and
      `crust:light:cameraVisible` (design D4). Verify with an import test:
      primvar alone = 1 makes the geometry camera-visible, and
      `crust:light:cameraVisible = 0` beats primvar = 1.
- [x] 2.2 Read the same pair for `DomeLight` and `DistantLight` (default
      visible, `infinite_light_escape_mask`) and add them with `add_masked`.
      Log it in the `Imported DomeLight` / `DistantLight` `DEBUG` lines. Verify
      with import tests on a small `.usda` per case.
- [x] 2.3 Read `domeLightCameraVisibility` / `crust:domeLightCameraVisibility`
      off the `RenderSettings` prim, and on `false` apply
      `LightList::hide_infinite_from_camera` after link resolution (design D6).
      Verify with an import test: with an HDRI plus a backdrop and the setting
      false, no infinite light is camera-visible, no backdrop is left, and the
      HDRI still illuminates.
- [x] 2.4 Update the "Area lights" and "Infinite lights" sections of
      `openspec/specs/lighting/design.md` and the light mapping section of
      `openspec/specs/usd-scene-import/design.md` with the precedence order,
      the camera-ray definition (ray mask, not depth), `domeLightCameraVisibility`
      and the removed sky. Verify the documented attributes by grepping the code.

## 3. Import: lights linked to nothing (`usd_import/light_links.rs`)

- [x] 3.1 Classify each light's `collection:lightLink` as default / nothing /
      excludes-only candidate / other (design D3), and log one `WARN` per light
      for "other". Verify with import tests on the default, `includeRoot = 0`,
      excludes-only and partial shapes, and unit tests on `/` in `excludes`.
- [x] 3.2 Record every receiver prim's stage path, truncated to four
      components, as the traversal emits it (mesh, sphere, curves, volume,
      native instance, `PointInstancer`). Verify with unit tests: coverage by a
      root exclude, by `/`, by a light-only exclude (none), by a sibling-prefix
      non-ancestor (none), deduplication, and a deep exclude refused as partial.
- [x] 3.3 After the last chunk, demote lights that illuminate nothing
      (`LightLinks::resolve`, via `LightList::remove` and the new
      `SceneBuilder::set_mask`): a camera-visible light at infinity becomes a
      backdrop, an area light keeps only its camera geometry, anything else is
      dropped. They never stay light-list entries (design D1). Verify with
      import tests: an area light with `includeRoot = 0` has no light-list
      entry and no indirect or shadow geometry (so no strategy can reach it),
      keeps camera geometry only when camera-visible, and a partial exclude
      leaves the light illuminating.
- [x] 3.4 Test that a light whose excludes name only another light prim (the
      Moana HDRI's `excludes = </island/lights/sky_dome_cam_llc>`) still
      illuminates everything, and that a backdrop traversed in an earlier
      streamed chunk than its receivers is still judged a backdrop.
- [x] 3.5 Record D3's receiver-prefix test, and its replacement by link classes
      once `add-light-and-shadow-linking` lands, in
      `openspec/specs/lighting/design.md` and
      `openspec/specs/usd-scene-import/design.md`. Narrow the linking
      "Known gaps" entries to partial links. Verify the gap entries against the
      spec delta.

## 4. Sample and scene checks

- [x] 4.1 Add `samples/dome_backdrop.usda`: an HDRI (or uniform-colour) dome,
      a flat-colour backdrop dome with
      `collection:lightLink:includeRoot = 0`, and a diffuse plane plus a mirror
      sphere. Verify in numbers (no eyeballing): with `exr_diff`, the render
      matches the same scene with the backdrop deactivated wherever the camera
      sees geometry, the escaped pixels equal the backdrop's colour, and the
      mirror reflects the HDRI.
- [x] 4.2 Add the sample to the integration tests that load `samples/*.usda`
      (`loads_dome_backdrop_usda`); `check_images.sh` picks it up by glob. Give
      the open showcase samples that were lit by the removed sky
      (`cornellbox`, `openpbr_showcase`, `instancing`, `nested_instancing`,
      `subdivision`, `curves`, `motionblur`, `animation`) a `DomeLight` over
      `samples/sky_gradient.exr`, the old gradient baked into an 8×512
      lat-long map. Leave the MIS, volume, emissive and light-visibility test
      scenes, and the closed `cornellbox_guided`, on a black background.
      Verify: against the pre-change goldens, the eight domed samples differ
      by noise only (relMSE 9e-5 to 2.5e-3, falling 14× from 16 to 128 spp on
      `cornellbox`, mean RGB equal to 1e-4), scenes that already had an
      infinite light are bit-identical, and `cargo test --workspace` passes.
- [x] 4.3 Island check: render `renders/moana_island/island_water.usda` (from
      `island.usda`) and the same over `islandPrman.usda` at `-s 16
      --indirect-clamp 0`. Verify: the `--stats -l debug` log lists
      `sky_dome_cam_llc` as a backdrop and `sky_dome_env_llc` as camera-visible
      in the first and camera-invisible in the second, the sky pixels are the
      backdrop's in both, and the island region's R/G and B/G move toward the
      reference values measured in `proposal.md`. Record the before/after
      numbers in `docs/moana_profile.md`.
- [x] 4.4 CI parity: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings` and
      `cargo test --workspace --no-fail-fast` are clean.
