## Context

See `proposal.md` for the motivation. Below is the state of the code this
design builds on.

**Motion in crust is one translation per geometry, at the top level only.**

- `crust:motion:translate` (a world-space `float3`, the displacement over the
  shutter) is read on `Mesh` and `Sphere` prims (`usd_import/attrs.rs`).
- The importer turns it into a top-level `Geometry::Instance` with
  `transform_end`:
  - for a mesh, `T(v)·L` (`usd_import/mesh.rs:792`), always instanced,
    never baked;
  - for a sphere, `T(v)` over an identity placement (`shapes.rs:57`).
- Every other `transform_end` is `None`: instancer prototypes, curves,
  lights, volumes. The camera is static.
- The kernel interpolates element by element (`crust-rt/src/prim.rs`,
  `lerp_affine`). For `L` and `T(v)·L` this is `L` with its translation
  moved by `t·v`, so **every point of the prim moves by exactly `v` over
  the shutter, at constant speed**.
- Both kinds are attached with `InstanceHitId::Own`, so a hit reports the
  moving placement's own `geom_id`.

**Shutter time.**

- `ray.time ∈ [0, 1)` is drawn per camera sample from the `K_TIME` domain
  only when `World::has_motion()` (`tracer/mod.rs:868-925`).
- `InstancePrim::transforms_at` uses the static start transform at
  `time = 0` exactly.

**AOVs.**

- The first-hit record `FirstHit` is filled only in the `AOV = true`
  instantiation of `trace_path` (`tracer/path.rs:2145`; thin walls build it
  in `first_wall`).
- It is turned into per-slot values by `aov.rs::sample_value`, using
  `CameraFrame`, the camera's origin and axes at shutter open.
- Channel suffixes come from `ChannelKind` (`crust-render/src/products.rs`).
- `HitRecord` carries no `geom_id`. The vertex-0 code has the `WorldHit`
  ids, but only for that moment.

**Render settings.**

- `warn_unhonoured` (`usd_import/products.rs:243`) warns about an authored
  `disableMotionBlur` / `instantaneousShutter = true`, then renders blurred
  anyway.
- It runs on the settings prim and on each product.

## Goals / Non-Goals

**Goals:**

- A `motionvector` channel VectorBlur2 can read with no conversion: forward,
  2D, raw pixels, `+v` up.
- The vector is exactly what the kernel moved, derived from the same
  `transform_end` the kernel interpolates, so the AOV and the blur cannot
  disagree.
- The vector does not depend on whether the beauty is blurred (spec: "Motion
  vectors do not depend on the beauty's blur").
- A sharp beauty while motion data is kept.
- The zero-AOV render is unchanged.

**Non-Goals:**

- Any motion other than a translation recorded on a top-level instance:
  - no nested motion;
  - no rotation, scale or deformation;
  - no camera motion.
- Per-frame units, `shutter:open/close`, a backward or centred vector, Arnold's
  normalised encoding, a 3D velocity source.
- Depth of field in the vector: it is measured through the pinhole.

## Decisions

### D1. A 2D forward vector in raw pixels, not a 3D velocity

Nuke's VectorBlur reads pixel-space `u`/`v` vectors, one unit to the pixel.
Arnold's raw `motionvector` is the same quantity, and is what Nuke artists
already wire up. So the source is defined in screen space, and 3D only
appears as an intermediate value.

- **Rejected: a 3D camera-space velocity.** Nothing in Nuke consumes it.
  Projecting it in comp needs `Peye` plus the camera, through an expression
  or BlinkScript, and gets the conversion wrong in every script that
  re-implements it.
- **Rejected: aliasing other renderers' names** (`velocity`, `Vector`,
  `motionFore`). Each has its own encoding (Cycles packs four components,
  V-Ray and RenderMan have their own conventions). An alias would be a
  plausible but wrong channel, the trap the `aovs` design record's D5
  already records for `diffuse_albedo`. Each can be added later as its own
  definition.

### D2. The motion record is derived in `WorldBuilder`, not in the importer

`WorldBuilder::attach_labelled` / `set_geometry` already inspect every
`Geometry::Instance` they are given (`vertex_source`). The motion record is
derived at the same point:

```
 Instance { transform: L, transform_end: Some(E), label: Own }
   E.matrix3 == L.matrix3        → motion[geom_id] = E.translation − L.translation
   otherwise, or the inner scene has_motion()
                                 → no record (vector 0), counted for one WARN
```

- **Why here.** The record is read off the transform the kernel will
  actually interpolate, so it cannot fall out of step with the beauty.
  Every importer special case is covered automatically: a non-invertible
  mesh drops its motion and gets no record, and a prototype gets
  `transform_end: None`. This turns another pair that would otherwise have
  to change together into one that is enforced by the code.
- **Storage.** A sparse table sorted by `geom_id`
  (`Vec<(u32, Vec3A)>`), binary-searched. It is empty for a static scene.
  Most geometries never move, and `SideTables` exists once per geometry, so
  a field there would cost every static `geom_id` 16 bytes. On a scene with
  millions of ids that is real resident memory, for a value read at most
  once per camera sample.
- **Unresolved motion** (a rotating end transform, or nested motion) gives
  vector 0 and one summarised `WARN` at the end of import with the count.
  Import produces neither today. The kernel accepts both, so the guard
  exists for the next producer.
  - A `WARN` because something authored is approximated.
  - Summarised because the count grows with the scene (CLAUDE.md, Logging).
- **Rejected: storing both endpoint transforms** and evaluating
  `(E − L)·M(t)⁻¹·P` exactly, which handles rotation too. It costs 96 bytes
  per moving geometry and a matrix inverse per sample, for motion nothing
  produces. Revisit when rotation motion exists.

### D3. Plumbing: what the first hit carries

- `FirstHit::Surface` gains `motion: Vec3A`: the world translation per
  shutter of the hit geometry, zero when none.
- The sample's shutter time is added to `SampleExtras`. The tracer already
  holds it as `time` in the sample loop.
- The thin-wall first hit (`first_wall`) gets the same field. The wall's
  `geom_id` is kept alongside its record in the AOV instantiation only.
- `FirstHit::Volume` and `Escaped` mean zero motion.
- The lookup runs only when the AOV plan has a `motionvector` slot, so other
  AOV renders do not pay for it.

The `AOV = false` instantiation is untouched. All of this sits behind the
existing `if AOV` at vertex 0.

### D4. The value: rebase, clip, project

Per sample, in `sample_value`:

```
 P0 = p − time·v          the hit point at shutter open (exact: constant velocity)
 P1 = P0 + v              the same point at shutter close
 clip [P0, P1] to depth ≥ z_near   (D5)
 (s0,t0) = proj(P0'), (s1,t1) = proj(P1')
 motionvector = ((s1 − s0)·width, (t1 − t0)·height)
```

- **Projection.** `proj` inverts `Camera::get_ray` at the lens centre:
  1. the `(s, t)` whose pinhole ray passes through the point;
  2. that is a scale onto the image plane (by camera depth), then the
     coordinates along `horizontal` and `vertical`.
  `CameraFrame` gains the image plane (`lower_left − origin`, `horizontal`,
  `vertical`) and the resolution.
  - **Why the pinhole.** Depth of field is ignored on purpose. The vector
    describes the in-focus image, which is what VectorBlur expects; defocus
    is a separate comp operation.
- **Orientation.** `t` runs along `vertical`, the camera's up, so `+v` is up
  on the displayed image, as Nuke expects. The film's internal row order
  must not leak in. A test pins it numerically: a sphere moving along the
  camera's up axis gives `v > 0` in the written EXR.
- **Why rebase to shutter open.** `p` is where the sample hit, at its own
  `time`. Subtracting `time·v` makes the value independent of which shutter
  time the sample drew, and therefore of whether blur is on. With blur off,
  `time = 0` and the rebase is a no-op.
- **Perspective.** The vector varies across an object even though `v` is
  constant: equal world displacements cover more pixels up close. No
  interpolation is involved. A straight 3D path projects to a straight 2D
  segment, the one VectorBlur blurs along; only the speed along it is
  non-uniform.

### D5. Near-limit clipping

The camera has no near plane, and a projection at or behind depth 0 blows
up or flips sign.

- The segment `[P0, P1]` is clipped to camera depth `≥ z_near`, with
  `z_near = 1e-3 · max(z0, z1)`.
- `max(z0, z1) > 0` always holds, because the visible hit lies on the
  segment, in front of the camera.
- `z_near` depends only on the path, never on the sample's `time`, so
  clipping keeps D4's independence from blur.
- The result is finite, and points the way the visible part moves.
- **Rejected:**
  - writing the clear value for such points: a fast object moving towards
    the camera would lose its blur exactly where it is most visible;
  - a fixed absolute epsilon: it is wrong at either end of scene scale.

### D6. Accumulation, type and channels

- **Closest by default**, like `P`. VectorBlur on blended edge vectors
  smears the foreground into the background, and Arnold users pair
  `motionvector` with `closest_filter` for the same reason. The overrides
  in the `aovs` design record's D7 still apply. Clear value 0, or an
  authored `clearValue`.
- **Two components only.** Any `*2*` `dataType` or `aov:format` (`float2`,
  `half2`, `vector2f` …) is accepted. Anything else is refused with a `WARN`,
  per the `aovs` design record's D4.
- **Channel kind.** A new `ChannelKind::Motion` writes `u`, `v`, lowercase,
  matching Nuke's built-in `forward.u` / `forward.v`. A RenderVar named
  `forward` therefore lands on that layer, and any other name gives a
  `<name>.u/.v` layer that VectorBlur2's `uv` knob can select. The UV AOV
  keeps `U`/`V`. Changing it now would rename existing outputs.

### D7. `disableMotionBlur`

- `RenderSettings` gains `motion_blur: bool`, default `true`.
- Import resolves it as `RenderSettingsBase`:
  - the first product's authored value, else the settings prim's;
  - `disableMotionBlur || instantaneousShutter` means off.
- The two leave `warn_unhonoured`.
- The tracer's gate becomes `motion = world.has_motion() && settings.motion_blur`.
  With blur off:
  - no `K_TIME` domain is derived, so the other sample dimensions are the
    ones a static scene would draw (`new_domain` is pure);
  - every ray has `time = 0`;
  - `transforms_at` returns the start transforms;
  - the beauty is sharp at the authored positions, and the motion records
    are still there for D2–D4.
- **Rejected:**
  - a CLI flag: not needed for the target scenes, and easy to add on top;
  - an env switch: this is a scene setting, not an optimisation to A/B.

## Risks / Trade-offs

- [Nuke may read `forward.u` differently from `forward.U`] → load the
  written EXR in Nuke before the change is archived (task 6.4). If Nuke
  folds case, nothing breaks.
- [The vertical direction comes out flipped] → a numeric test on the written
  EXR, not on internal buffers (task 5.2).
- [Per-shutter units against VectorBlur's per-frame expectation] →
  documented. The user sets VectorBlur's scale to their shutter fraction.
  Per-frame units wait for `shutter:open/close` support.
- [A forward-only vector against VectorBlur's centred default] → documented
  as forward. The user doc gives the VectorBlur2 setting for forward vectors,
  verified in Nuke (task 6.4).
- [With blur on, the closest sample may come from a different object than
  the one dominating the blurred pixel] → inherent to blurring twice. The
  intended workflow is blur off (D7), where the sample and the pixel agree.
- [Very large vectors near the camera after clipping] → finite by
  construction. VectorBlur clamps with its own maximum, and the user doc
  says so.
- [Instancer prototypes authoring `crust:motion:translate` neither blur nor
  get vectors] → consistent with the beauty. Already true today, and listed
  in the `usd-scene-import` known gaps. Unchanged here.

## Migration Plan

- A scene authoring `disableMotionBlur = true` or `instantaneousShutter = true`
  used to render blurred with a warning. It now renders sharp. This is the
  authored intent and the documented USD meaning. Houdini authors both at
  their `false` fallback, so typical Solaris exports are unaffected.
- Nothing else changes for existing scenes. `motionvector` used to be
  refused as unknown, and is now produced.
- Rollback: revert the change. No file formats or caches are involved.

## Open Questions

- Should 3-component `dataType`s (`color3f`, `vector3f`) be accepted for
  `motionvector`, with a zero third channel, for Arnold-authored RenderVars
  that ask for RGB? This doesn't change the approach. It's a later
  relaxation of D6's type check if real files need it.
