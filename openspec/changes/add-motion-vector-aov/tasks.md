# Tasks

## 1. Honour `disableMotionBlur` (design D7)

- [ ] 1.1 Add `motion_blur: bool` (default `true`) to `RenderSettings`, with a
      builder setter. Resolve it at import as `RenderSettingsBase`: the first
      product's authored `disableMotionBlur` / `instantaneousShutter`, else
      the settings prim's; either `true` turns blur off. Verify with unit
      tests on small `.usda` stages covering settings-only, product-override
      and synonym.
- [ ] 1.2 Remove `disableMotionBlur` and `instantaneousShutter` from
      `warn_unhonoured`, keeping `disableDepthOfField`, `pixelAspectRatio`
      and `dataWindowNDC`. Verify with a log-capture test: no warning for
      the two, the existing one still emitted for `disableDepthOfField`.
- [ ] 1.3 Gate the tracer's shutter draw on
      `world.has_motion() && settings.motion_blur`. Verify with integration
      tests on a sphere with `crust:motion:translate = (1, 0, 0)` against a
      black background:
      - with blur disabled, no pixel right of the sphere's authored
        silhouette is lit by it;
      - with blur on (the default), those pixels are lit, as before;
      - `instantaneousShutter = true` gives an image bit-identical to
        `disableMotionBlur = true`.
- [ ] 1.4 Document it:
      - `site/content/docs/usd/geometry.md` (`crust:motion:translate`: blur
        can be disabled);
      - the render-settings page (the two attributes and their resolution);
      - `architecture/limitations.md`, if it lists them as ignored;
      - the `usd-scene-import` design record.
      Verify with `zola build` in `site/` (Zola 0.21).

## 2. Per-geometry motion record (design D2)

- [ ] 2.1 In `WorldBuilder`, derive a sparse `geom_id → translation` table
      (sorted, binary-searched) from every `Geometry::Instance` attached or
      set with `InstanceHitId::Own` and a `transform_end` whose linear part
      equals the start's. Expose a `World` lookup returning `Vec3A::ZERO`
      for no record. Verify with unit tests:
      - a moving sphere and a moving mesh give their authored `v`;
      - a static instance and a baked mesh give zero;
      - a non-invertible mesh with motion gives zero (its motion was
        dropped).
- [ ] 2.2 Count, and do not record, an end transform with a different linear
      part, and an instance whose inner scene `has_motion()`. Emit one
      summarised `WARN` with the count at the end of import. Verify with
      unit tests building both shapes directly through `WorldBuilder`.
- [ ] 2.3 Add the pair to `docs/architecture.md` § Invariants: the
      motion-vector AOV reads the `transform_end` the kernel interpolates,
      and is derived in `WorldBuilder`, not in the importer. Verify the
      architecture doc's links still resolve.

## 3. The `motionvector` source, type and channels (design D1, D6)

- [ ] 3.1 Add `AovSource::MotionVector` with `RAW_NAMES` entry
      `motionvector` and no aliases, 2 components, closest by default,
      clear value 0. Verify with `aov.rs` unit tests for name resolution,
      component count, default mode, and `velocity` still refused as
      unknown.
- [ ] 3.2 Accept only 2-component `dataType` / `aov:format` for it, and
      refuse anything else with one `WARN` per var. Verify with a test that
      `dataType = "float"` is refused and produces no channel.
- [ ] 3.3 Add `ChannelKind::Motion` writing lowercase `u`, `v` in
      `crust-render/src/products.rs`. Verify with a `channel_names` test: a
      var named `forward` gives `forward.u`, `forward.v`, and the UV source
      still gives `U`/`V`.
- [ ] 3.4 Document the source in `site/content/docs/usd/aovs.md`:
      - its definition: forward, raw pixels, `+v` up, per shutter, closest;
      - a RenderVar example named `forward`;
      - why other renderers' names aren't aliases.
      Add its row to the `aovs` spec's vocabulary and design record (D5
      table). Verify with `zola build`.

## 4. First-hit plumbing and the value (design D3, D4, D5)

- [ ] 4.1 Add `motion: Vec3A` to `FirstHit::Surface`, filled at vertex 0
      and in `first_wall` (keep the wall's `geom_id` in the AOV
      instantiation only). Add the sample's shutter time to
      `SampleExtras`. Look the motion up only when the AOV plan has a
      `motionvector` slot. Verify the `AOV = false` path is unchanged: the
      zero-AOV instruction-count pin in `docs/architecture.md` still holds,
      measured with callgrind on `samples/cornellbox.usda` at `-s 2`.
- [ ] 4.2 Extend `CameraFrame` with the image plane (`lower_left − origin`,
      `horizontal`, `vertical`) and the resolution, and add the
      pinhole-inverse `proj`. Verify with a unit test that projecting
      `get_ray(s, t, centre, 0)`'s point at any depth returns `(s, t)`
      within 1e-5.
- [ ] 4.3 Implement the value: rebase `P0 = p − time·v`, clip `[P0, P0+v]`
      to depth `≥ 1e-3·max(z0, z1)`, project both ends, scale by
      `(width, height)`. Verify with `sample_value` unit tests:
      - the same point hit at `time` 0, 0.3 and 0.9 gives the same vector
        within 1e-4 px;
      - a segment crossing the camera plane in either direction gives a
        finite vector pointing along the visible motion;
      - zero motion gives `(0, 0)`;
      - `Volume` and `Escaped` give the clear value.

## 5. End-to-end behaviour

- [ ] 5.1 Add a sample scene (`samples/motionvector.usda`): a sphere moving
      along the camera's right axis, one moving up, a receding plane moving
      sideways, a `forward` RenderVar, and `disableMotionBlur = true`.
      Verify it renders with
      `cargo run --release -- render -i samples/motionvector.usda`.
- [ ] 5.2 Integration tests on the written EXR at `-s 16` with
      `--indirect-clamp 0`:
      - the right-moving sphere's centre pixel `forward.u` equals the
        predicted projection difference within 0.01 px, with `forward.v`
        about 0;
      - the up-moving sphere has `forward.v > 0`;
      - the receding plane's near pixels have longer vectors than its far
        ones;
      - background pixels are `(0, 0)`;
      - a straddling edge pixel holds one of the two values, never a blend.
- [ ] 5.3 Integration test: the same scene with blur on and off gives equal
      vectors (within 1e-3 px) on pixels whose chosen sample hits the
      moving sphere's interior in both renders.
- [ ] 5.4 Verify the existing AOV invariants still hold with a
      `motionvector` var present:
      - tiles and scanlines are bit-identical;
      - the beauty is unchanged by requesting the AOV (`exr_diff` against
        the render without it);
      - `scripts/check_images.sh check` against goldens recorded before
        the change reports no difference on the existing samples.

## 6. Integration checks

- [ ] 6.1 Run the CI commands and verify that each one passes:
      - `cargo fmt --all -- --check`;
      - `cargo clippy --workspace --all-targets -- -D warnings`;
      - `cargo test --workspace --no-fail-fast`;
      - `cargo deny --locked check`.
- [ ] 6.2 Run the pinned-nightly leg and verify it passes:
      `cargo +nightly-2026-09-26 clippy --workspace --all-targets -- -D warnings`.
- [ ] 6.3 Run `openspec validate add-motion-vector-aov --strict` and verify it
      passes.
- [ ] 6.4 Manual check in Nuke, done by a person: load
      `samples/motionvector.usda`'s EXR. Confirm that `forward.u/v` land on
      Nuke's built-in `forward` layer, and that VectorBlur2 blurs the sharp
      beauty in the direction of motion. Record the VectorBlur2 settings
      for a forward, per-shutter vector in `site/content/docs/usd/aovs.md`.

## Workflow follow-up

- Archive the change once implemented and reviewed. This syncs the `aovs`,
  `image-output` and `usd-scene-import` deltas into the main specs.
