## 1. Baseline

- [x] 1.1 Record golden images from the unmodified tree
      (`scripts/check_images.sh record <dir>`), so the bit-identical claim in
      §5 is checkable rather than asserted.

## 2. Importer change (`crates/crust-core/src/scene/usd_import.rs`)

- [x] 2.1 Add `light_ray_mask(prim: &Prim) -> u32` next to `prim_ray_mask`,
      defaulting to `MASK_ALL & !MASK_CAMERA`, documenting *why* the camera bit
      is the only one dropped (the indirect bit carries the bounce half of MIS)
      and that `crust:rayMask` overrides it verbatim.
- [x] 2.2 Import `MASK_CAMERA` alongside the existing `MASK_ALL`.
- [x] 2.3 Add a `mask: u32` parameter to `emit_sphere_light` and
      `emit_rect_light`; swap their `world.attach(..)` calls for
      `world.attach_masked(.., mask)`.
- [x] 2.4 Pass `light_ray_mask(prim)` at the two dispatch sites.
- [x] 2.5 Update the inline comments in both emit functions, which currently
      state the surface is visible geometry.
- [x] 2.6 Add a comment on `world.rs::add_sphere_light` recording that the
      procedural fallback scene deliberately keeps camera-visible lights (no
      prim, so no attribute to opt back in with).

## 3. Samples

- [x] 3.1 New `samples/light_visibility.usda`: a floor plus two lights of the
      same shape, one default and one authoring `int crust:rayMask = 7`, placed
      so a known ray hits each. Document the contract in the stage `doc`
      string, as `samples/motionblur.usda` does.
- [x] 3.2 Author `int crust:rayMask = 7` on the lights of the samples that
      frame them. Measured set (per the golden check, which corrected the
      framing arithmetic — see design.md): `veach_mis.usda` (4 sphere lights),
      `openpbr_showcase.usda` (2 sphere lights), `fog.usda`, `smoke.usda`, and
      `rectlight.usda` — the last was predicted out of frame but is not, since
      a light is an extent rather than a point.
- [x] 3.3 Update `samples/openpbr_showcase.usda`'s comment about lights being
      rendered as visible geometry — it is now true because of the authored
      opt-in, not automatically.
- [x] 3.4 Leave `cornellbox_guided.usda` at the new default: its light is
      deliberately shrouded and the scene wants it unseen.

## 4. Tests (`crates/crust-core/tests/usd_scene.rs`)

- [x] 4.1 Add `light_surface_is_hidden_from_the_camera_unless_authored` over
      `samples/light_visibility.usda`, modelled on `loads_motionblur_usda`'s
      masked-ray assertions: a `MASK_CAMERA` ray passes the default light and
      reaches the floor; `MASK_INDIRECT` and `MASK_SHADOW` rays along the same
      line hit it; a `MASK_CAMERA` ray hits the opted-in light; and
      `scene.lights.count() == 2` so masking never drops a light entry.
- [x] 4.2 Confirm the `MASK_INDIRECT` assertion is the first anywhere in the
      workspace — it closes a real coverage gap, so say so in the test's doc
      comment.
- [x] 4.3 Verify every existing `scene.world.count()` assertion still passes
      unchanged (geometry is still attached; only its mask narrows).

## 5. Verification

- [x] 5.1 `cargo test` green.
- [x] 5.2 `scripts/check_images.sh check <dir>` reports every pre-existing
      sample identical. The new sample reports `NO GOLDEN` and is skipped.
- [x] 5.3 Prove the bounce half of MIS survived: render a scene whose light is
      camera-hidden at high spp under `--strategy power`, `balance`, `light`
      and `bsdf` and confirm they converge to the same image via `exr_diff`.
      `bsdf` traces no shadow rays, so it finds the light only through
      `MASK_INDIRECT`.
- [x] 5.4 Run the ignored guiding tests
      (`cargo test -p crust-core --test guiding -- --ignored`), since
      `cornellbox_guided.usda`'s only light is a `SphereLight`.

## 6. Docs and specs

- [x] 6.1 `CLAUDE.md`: the `Light` section ("Cornell-box semantics: a light is
      both light and visible object"), the `crust:rayMask` bullet (now honored
      on lights, with a different default), and the SphereLight/RectLight
      mapping lines.
- [x] 6.2 `CLAUDE.md` island section: it says "crust has no per-light
      camera-visibility". Reword without overclaiming — the doubled *dome*
      lights are unaffected, since domes carry no geometry.
- [x] 6.3 `README.md`: rewrite the Lights section (also independently stale —
      it claims `DistantLight`/`DomeLight` are skipped) and the
      `crust:rayMask` bullet.
- [x] 6.4 Rustdoc: `AreaLight` ("Cornell-box semantics — the same surface is
      both light and visible object") and `Scene`'s USD-mapping summary.
- [x] 6.5 `openspec/specs/usd-scene-import/spec.md`: apply this change's delta
      to the "Light schema mapping" requirement and add the "Ray visibility
      mask" requirement, which the main spec does not currently carry at all.
- [x] 6.6 Keep `openspec/changes/add-material-color-management`'s copy of the
      "Light schema mapping" requirement consistent — it repeats the old
      "both a light and visible geometry" wording verbatim.
