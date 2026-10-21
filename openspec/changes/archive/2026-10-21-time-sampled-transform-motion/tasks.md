# Tasks

## 1. Read the camera shutter (design D1)

- [x] 1.1 Read the render camera's `shutter:open` / `shutter:close` in `read_index`, beside
      `subdiv_policy`'s camera read:
      - the named camera from the index stage, with the masked-stage fallback;
      - else the first `Camera` of an index-stage walk pruned like the traversal.

      Resolve it to `Shutter` time codes (`f + open .. f + close`, `Instantaneous` when
      `close <= open` or there is no frame) and keep it on the import context. Verify with
      unit tests on small `.usda` stages:
      - authored 0 / 0.5 at `-f 5` gives 5 .. 5.5;
      - −0.25 / 0.25 gives 4.75 .. 5.25;
      - unauthored is instantaneous;
      - no frame is instantaneous;
      - a named camera wins over an earlier one;
      - a camera inside an unloaded payload is read through the masked stage.
- [x] 1.2 After `resolve_camera`, compare the camera rendered through with the pre-read one,
      and raise `camera.shutter_mismatch` when they differ and their shutters differ. Add the
      code to `warnings.rs` and its row to `site/content/docs/reference/warnings.md`.
      Verify with a two-camera stage, unnamed, whose traversal settles on the second: one
      warning naming both. Also verify that the warnings-doc test passes.
- [x] 1.3 Document the shutter in `site/content/docs/usd/render-settings.md` (a "Camera
      shutter" section beside camera exposure: units, fallback, which camera, the
      instantaneous default). Verify with `zola build` in `site/` (Zola 0.21).

## 2. Carry the shutter pair through the traversal (design D2)

- [x] 2.1 Add `compose_at(prim, parent, time)` in `xform.rs`: `compose_with_parent` under a
      nested `EvalTimeScope`. Verify with a unit test that, on a time-sampled translate, it
      returns the transform at that time and restores the outer time afterwards (a read
      after it resolves at the frame again).
- [x] 2.2 Extend `traverse_into`'s stack entries with `Option<Pair { open, close }>`. A prim
      gets a pair when the shutter is not instantaneous and either its parent has one or
      `transform_might_be_time_varying()` is true; reuse `W_f` for `open` when
      `open == 0`. Verify with unit tests:
      - a static stage with a shutter creates no pair;
      - a static child of an animated `Xform` inherits a moving pair;
      - a `!resetXformStack!` child under an animated parent with static ops of its own gets
        no pair.
- [x] 2.3 Count drawn prims whose transform varies over time but gets no motion: curves,
      lights, volumes, inside a prototype, and the no-frame case. Raise
      `xform.motion_ignored` and `time.motion_needs_frame` once each with the count, with
      rows in `reference/warnings.md`. Verify with stages covering each cause: one warning
      per cause. Also verify that no such warning appears with an instantaneous shutter.

## 3. Placements move from open to close (design D3)

- [x] 3.1 Meshes and implicit shapes: replace `MeshPlacement::motion` with the end
      transform `T(v) · X(close)`, start `X(open)`. A moving placement is never baked. A
      non-invertible moving one bakes at open and is counted under `xform.motion_ignored`.
      Verify with an integration test (time-sampled translate, `shutter:close = 1`, `-f 5`,
      black background): pixels along the path are lit, and the image with
      `disableMotionBlur` is sharp at the open position. Also verify that
      `crust:motion:translate` on a static mesh still blurs by `v` with no shutter.
- [x] 3.2 Spheres: centre and radius from `X(open)`, inside an identity instance ending at
      `T(v) · X(close) · X(open)⁻¹`. Verify with the same test on a sphere, and with a
      sphere authoring both an animated translate and `crust:motion:translate`: its
      streak's end matches the sum of the two motions.
- [x] 3.3 Native instances: in `attach_proto_parts`, give the instance prim's placement
      `transform_end` from its pair, `part.local` folded in on both sides. Verify with a
      stage that has two instances of a two-mesh prototype, one with a time-sampled
      transform: only that one blurs, and both of its meshes do.
- [x] 3.4 `PointInstancer`: each placement `P_i` gets `W(open)·P_i` → `W(close)·P_i` from the
      instancer prim's pair. Verify with an instancer whose own `xformOp:translate` is
      time-sampled: every instance blurs by the same world displacement.
- [x] 3.5 Document the motion sources in `site/content/docs/usd/geometry.md`
      (`crust:motion:translate` adds to animated motion) and in `usd/overview.md` (time
      samples on transforms produce motion over the shutter). Update
      `architecture/limitations.md`: animated transforms are now blurred; what remains is
      deformation, curves / lights / volumes, two keys only, the camera. Verify with
      `zola build`.

## 4. Motion records and vectors for affine motion (design D4, D5)

- [x] 4.1 Add `MotionRecord::Affine(Box<[Affine3A; 2]>)` for `Own`-labelled instances
      whose linear parts differ, and keep `Translation` for equal ones. Add the per-hit
      velocity `v = E·L − S·L` with `L = lerp_affine(S, E, t)⁻¹ · p`, giving 0 for a
      singular `M(t)`. Feed it into the unchanged `aov::motion_vector`. Verify with unit
      tests:
      - a pure translation still yields `Translation`;
      - a rotation's velocity at `t = 0` and `t = 0.7` rebases to the same `P0`;
      - a singular midpoint gives 0.
- [x] 4.2 Turn the record table into sorted, non-overlapping id ranges. An
      `Offset(base)`-labelled moving placement records `base ..= base + max_hit_id`.
      `As(k)` labels and inner scenes that themselves move stay `Unresolved`. Verify with
      unit tests:
      - every id of a moving multi-part instance resolves to its record;
      - the neighbouring geometry's id does not;
      - the existing `Unresolved` tests still pass.
- [x] 4.3 Reword `xform.motion_vector_unsupported`'s text ("moves in a way the vector cannot
      attribute to one placement") in `warnings.rs` and `reference/warnings.md`. Verify
      with the warnings-doc test.
- [x] 4.4 End-to-end, in `crates/crust-render/tests/motion_vector.rs`, render
      `samples/animation.usda` with an override layer: camera `shutter:close = 1`, `-f 5`, a
      `forward` var, sharp and blurred. Verify:
      - `Mover`'s centre pixel matches the independent pinhole projection of `P` moved 4/9
        units along X within 0.01 px, with `v ≈ 0`;
      - the sky is `(0, 0)`;
      - the sharp and blurred vectors agree on the interior;
      - `shutter:close = 0.5` halves the vector at constant depth.
- [x] 4.5 Rotation end to end: a cube spinning about the view axis by a time-sampled
      `xformOp:rotateZ`. Verify that each pixel's vector matches the projection of the
      chord `R·P0 − P0` computed in the test from `P`, and is near zero at the axis.
- [x] 4.6 Update the "Motion vectors" section of `site/content/docs/usd/aovs.md`: sources
      (time-sampled transforms, rotation, scale), units (the camera shutter), the sharp
      beauty at shutter open, and the recipe (`shutter:close` plus `disableMotionBlur`).
      Verify with `zola build`.

## 5. The instantaneous-shutter warning (aovs spec, design D7)

- [x] 5.1 Raise `camera.shutter_instantaneous` once when a `motionvector` var is requested,
      the shutter is instantaneous, and a drawn prim's transform varies over time. Make the
      time-variance query only in that case or when a shutter is authored. Add its row to
      `reference/warnings.md`. Verify:
      - `samples/animation.usda` at `-f 5` with a `forward` var and no shutter gives one
        warning;
      - a static stage with the same var gives none;
      - `crust:motion:translate` vectors are still produced.

## 6. MCP live session (design D8)

- [x] 6.1 Record the resolved shutter (open, close, the camera path) on the scene's
      provenance, and make `refresh_settings` refuse a changed one with
      `Structural("the camera shutter changed")`. Verify with a live-session test:
      - editing `shutter:close` re-imports, and its render equals a fresh import's
        bitwise;
      - editing only the camera's exposure still applies in place.

## 7. Integration checks and records (design D9)

- [x] 7.1 Run `scripts/check_images.sh check` against goldens recorded before the change, at
      16 spp with `--indirect-clamp 0`. Verify every sample scene is bit-identical (none
      authors a shutter).
- [x] 7.2 Count instructions with callgrind (`RAYON_NUM_THREADS=1`, cornellbox, 2 spp) on the
      `trace_path` `AOV = false` instantiation before and after. Verify the count is
      unchanged.
- [x] 7.3 Measure import time with `bench_ab.sh` / `--stats` on
      `samples/PointInstancedMedCity.usd`, and on the largest production stage available,
      both with and without an authored shutter. Verify the static case shows no
      regression beyond the noise floor, and record the numbers in the
      `usd-scene-import` design record.
- [x] 7.4 Update the design records:
      - `aovs` ("Motion vector AOV": affine record, ranges, shutter units; Known gaps
        without rotation);
      - `usd-scene-import` (shutter read, the traversal pair, the placement rule, the
        warnings, Known gaps: two keys, mixed-time evaluation, per-instance instancer
        motion);
      - `intersection-kernel` (the chord, now also a vector property).

      Verify with `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings` and
      `cargo test --workspace --no-fail-fast`, all clean.

## 8. Review fixes on #347 (design D2, D4, D6)

- [x] 8.1 Moving direct instances keep a tangent frame: `VertexSource::Moving` carries the
      hit's vertices through both transforms interpolated at the ray's time. Verify with
      `motion_blurred_instances_get_a_tangent_at_the_ray_time` (a quad turning a quarter
      turn: +X at time 0, the diagonal at 0.5, +Y at 1), replacing the test that pinned the
      gap, and update the `textures` design record and the tangent requirement.
- [x] 8.2 `disableMotionBlur` is sharp at the frame, `instantaneousShutter` at shutter open
      (`RenderProducts::sharp_at_frame`, `TransformMotion::span`), with both set
      `disableMotionBlur` winning, and a product that differs in either warned about.
      Verify with `a_frame_pose_starts_at_the_frame_and_moves_by_the_shutter` and
      `blur_off_is_sharp_at_the_frame_and_instantaneous_at_shutter_open` (centred shutter:
      sharp at x = −0.5 vs −0.75, the vector the shutter's 0.5 either way).
- [x] 8.3 The frame pose enters the MCP shutter key, so toggling `disableMotionBlur` over a
      shutter that opens away from the frame re-imports. Verify with the extended
      `a_shutter_edit_reimports_and_an_exposure_stays_in_place`.
- [x] 8.4 Correct the texts the review found inconsistent: D2's rule (a reset child does
      not inherit the parent's pair), the animated-curves scenario and
      `xform.motion_ignored` (unmoved prims sit at the frame, not shutter open), and the
      user documentation for both flags (`render-settings.md`, `aovs.md`). Verify with
      `openspec validate --strict`, the warnings-doc test, and the docs workflow on the PR.

## Workflow follow-up

- Archive the change after review, then start the deforming-points / `velocities` change
  on top of this shutter interval.
