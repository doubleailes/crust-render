# Proposal

## Why

`motionvector` is `(0, 0)` on geometry animated by time samples (#336). The import reads
every attribute at one time code, the frame given by `-f`, so an animated
`xformOp:*.timeSamples` places the prim at its frame position and nothing records where it
goes next. Only an authored `crust:motion:translate` moves anything over the shutter, and
production stages never author it. Animated props and rigid parts of characters therefore
get no vectors, Nuke's VectorBlur (the pass's intended consumer) cannot blur them, and the
beauty is never motion blurred for them either. The camera's `shutter:open` /
`shutter:close`, the USD attributes that say over which interval motion is measured, are
not read at all.

This change covers rigid motion: time-sampled transforms. Deforming points and
`velocities` (the Alembic character case in #336) are a follow-up change built on the
shutter interval defined here.

## What Changes

- **The camera shutter is read.** The render camera's `shutter:open` and `shutter:close`
  (frame-relative offsets in time codes, USD fallbacks 0 and 0) define the motion
  interval `[f + open, f + close]` around the frame `f` given by `-f`. One interval drives
  both the beauty's motion blur and `motionvector`.
- **Time-sampled transforms move geometry over the shutter.** When the shutter is not
  instantaneous and a frame is given, every drawn placement whose world transform varies
  over time (its own ops or any ancestor's) is placed at its shutter-open transform and
  moves to its shutter-close transform:
  - meshes, implicit shapes and spheres;
  - native instances (an instanceable prim with an animated transform, or under an
    animated ancestor), including multi-part prototypes;
  - every placement of a `PointInstancer` whose own transform, or an ancestor's, is
    animated.

  The beauty is blurred along that motion, as `crust:motion:translate` already is, unless
  `disableMotionBlur` / `instantaneousShutter` is set.
- **Motion vectors for any affine motion.** `motionvector` measures rotation and scale as
  well as translation. Today a geometry whose end transform has a different linear part
  is blurred but reports 0 with `xform.motion_vector_unsupported`. That warning remains
  only for motion the record still cannot attribute to one placement (a forwarded label
  inside a nested instancer, motion inside a prototype).
- **`crust:motion:translate` keeps its meaning**: a world-space displacement over the
  shutter, whatever the shutter's length, including the instantaneous default. Authored
  together with an animated transform, it is added on top of that motion.
- **Shutter open, not the frame, is the sharp pose.** With blur disabled, camera rays
  stay at shutter open, as the existing requirement says. For a shutter that opens before
  the frame (`-0.25 .. 0.25`), animated geometry renders sharp at `f - 0.25`. A forward
  vector from there blurs over exactly the shutter.
- **No silent zero.** Each of the following is reported with one summarised warning:
  - a `motionvector` var is requested over an instantaneous shutter while drawn geometry's
    transform varies over time;
  - the shutter is not instantaneous but no frame was given, so time samples are not
    evaluated;
  - a time-varying transform on geometry that gets no motion (curves, lights, volumes,
    prims inside a prototype, baked non-invertible meshes).
- **Unchanged output when no shutter is authored.** USD's fallback shutter is 0/0, so a
  stage that authors none renders bit-identically to today, with or without `-f`. No
  sample scene authors a shutter.
- **Out of scope:**
  - deforming `points.timeSamples` and `velocities` (the follow-up change);
  - per-instance `PointInstancer` motion (`positions` / `orientations` time samples,
    `velocities`, `angularVelocities`);
  - camera motion (blur and vectors from an animated camera);
  - more than one motion segment per shutter (only the two endpoints are read);
  - motion of curves, lights and volumes;
  - transforms animated inside a prototype.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `usd-scene-import`:
  - the camera's `shutter:open` / `shutter:close` are read and define the motion
    interval;
  - time-sampled transforms produce motion over that interval;
  - `disableMotionBlur` traces at shutter open, which is no longer always the frame;
  - the new warnings.
- `aovs`:
  - `motionvector` covers time-sampled transform motion, including rotation and scale;
  - its units are the shutter interval read from the camera;
  - the instantaneous-shutter warning.

## Impact

- **Code**:
  - `usd_import/camera.rs` and `mod.rs` (`read_index`): read the render camera's shutter
    before the traversal, the way adaptive subdivision already reads its screen
    projection (index stage, masked-stage fallback);
  - `usd_import/mod.rs`, `xform.rs`: the traversal carries a shutter-open /
    shutter-close world pair through moving subtrees, using
    `transform_might_be_time_varying` to skip static prims;
  - `usd_import/mesh.rs`, `shapes.rs`, `implicit.rs`, `instancing.rs`: placements take
    `transform_end` from that pair;
  - `rt_world.rs`: `MotionRecord` gains an affine record and id-range records for
    `Offset`-labelled placements;
  - `aov.rs`: the per-hit velocity from the affine record;
  - `usd_import/live.rs`: a settings refresh refuses a changed shutter;
  - `warnings.rs`: three new codes.
- **Performance**:
  - the zero-AOV render path is unchanged;
  - static scenes are unchanged apart from one time-variance query per `Xformable` prim at
    import, to be measured on the largest available stage;
  - a moving placement costs one extra transform evaluation at import, and 96 bytes of
    motion record instead of 16;
  - the AOV render inverts one affine transform per camera sample that hits rotating
    geometry.
- **Kernel**: none. `crust-rt` already interpolates arbitrary affine end transforms.
- **Dependencies**: none. openusd-schemas already exposes the camera's shutter and
  `transform_might_be_time_varying`.
- **Docs**:
  - `site/content/docs/usd/aovs.md` (Motion vectors);
  - `usd/render-settings.md` (a camera shutter section beside camera exposure);
  - `usd/geometry.md` (`crust:motion:translate` with animated transforms);
  - `usd/overview.md` (time samples);
  - `architecture/limitations.md`;
  - `reference/warnings.md`;
  - the `aovs`, `usd-scene-import` and `intersection-kernel` design records.
- **Coordination**: `physical-depth-of-field` also edits the camera import and
  `read_index`. The two touch neighbouring lines, not the same behaviour.
