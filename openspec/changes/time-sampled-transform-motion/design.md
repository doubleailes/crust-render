# Design

## Context

See `proposal.md` for the motivation. This is the state of the code the design builds on.

**One time code per import.** `EvalTimeScope` sets a thread-local (`usd_import/time.rs`)
for the length of `load_scene`, and every attribute read resolves at it. That includes
`compose_with_parent` (`xform.rs`), the one composition rule shared by the traversal, the
placement count, the prototype walk, a camera's ancestor chain and MCP's `prim_frames`. The
scopes nest and restore, so a read at a second time is a nested scope. Without `-f` the
time is `None`, and attributes read their default value.

**The camera is met during the traversal**, by `visit_camera`, and settled afterwards by
`resolve_camera`. Geometry is emitted while walking (or queued in `pending_meshes`), so
anything a placement needs from the camera must be known before the walk. Adaptive
subdivision already has this need, and `subdiv_policy` meets it:

- it reads the named camera (`--camera`, else `RenderSettings.camera`) on the index
  stage;
- it falls back to a stage masked to that camera when the camera lives in a payload;
- it warns (`subdiv.adaptive_needs_camera`) when no camera is named.

**Motion is `transform_end`.** Only `crust:motion:translate` produces motion:

- `mesh.rs` attaches a mesh as `Instance { transform: L, transform_end: T(v)·L }`, and a
  moving mesh is never baked;
- `shapes.rs` places a sphere in world space inside an identity instance whose end is
  `T(v)`.

Native-instance and `PointInstancer` placements (`instancing.rs`) always pass
`transform_end: None`.

**The kernel moves points in straight lines.** `lerp_affine` interpolates the two
transforms element by element. For a local point `L`, `M(t)·L = S·L + t·(E − S)·L`, so
every point moves along a straight segment at constant speed, even under rotation. This
is why a large rotation bows inwards, an `intersection-kernel` known gap. The union of
the endpoint bounds is conservative. Arbitrary affine pairs already work: no kernel
change is needed.

**The vector.** `WorldBuilder` derives `MotionRecord::of` for every attached instance:

- `Translation(v)` when the end transform has the start's linear part and the label is
  `Own`;
- `Unresolved` otherwise, which reads 0 and raises one `xform.motion_vector_unsupported`.

`aov::motion_vector(cam, p, v, t)` rebases to `P0 = p − t·v`, clips and projects. It is
exact for any motion in which the hit point moves at constant velocity `v`.

**MCP.** `crust_core::live` rebuilds only materials, lights and settings in place.
Geometry and transform edits are full re-imports. `refresh_settings` re-reads the
settings and camera, and refuses (`Structural`) when a value the geometry was built on
changed. The adaptive-subdivision key is the precedent.

## Goals / Non-Goals

**Goals:**

- One motion interval for the blur and the vector, read from the camera, so the two
  cannot disagree.
- A vector for any affine motion the kernel can blur, derived from the same transform
  pair the kernel interpolates.
- No added cost on static stages beyond one time-variance query per `Xformable` prim, and
  bit-identical output for stages that author no shutter.

**Non-Goals:**

- More than two motion keys per shutter. A curved or accelerating path inside the shutter
  is approximated by its chord, as the kernel does.
- Deformation, `velocities`, per-instance `PointInstancer` motion, camera motion. The
  follow-up change takes the first two.
- Reading attributes other than transforms at the shutter times.

## Decisions

### D1. The shutter is read before the traversal, from the camera the render will use

`read_index` reads the shutter beside `subdiv_policy`'s screen projection:

- **Named camera**: read from the index stage, with the same masked-stage fallback.
- **No camera named**: the first `Camera` prim of a walk over the index stage (payloads
  unloaded, so the walk is cheap), pruned as the traversal prunes.

After `resolve_camera`, the render's camera is compared with the one whose shutter was
read. A different camera with a different shutter raises `camera.shutter_mismatch`, which
names both cameras and the interval used. Most stages have one camera, so this is an edge
case, but it must not be silent.

The resulting `Shutter { open: f64, close: f64 }` is resolved to time codes as
`f + open .. f + close`. It is `Instantaneous` when `close <= open` or when there is no
frame. It lives on the import context, and on the scene's provenance for MCP (D8).

*Rejected:*

- **Settling end transforms after the traversal**, once the camera is known. Streamed
  chunks drop their stage before the walk ends, and shapes are attached immediately.
- **Requiring a named camera, as adaptive subdivision does.** The common one-camera stage
  names none, and #336's repro is one.
- **A render-settings attribute for the interval.** USD already has the camera's shutter,
  and Karma, RTX, Cycles and hdPrman all read it.

### D2. The traversal carries a shutter-open / shutter-close pair through moving subtrees

Stack entries grow from `(prim, W_f, hidden)` to `(prim, W_f, hidden, Option<Pair>)`, with
`Pair { open: GMat4, close: GMat4 }`:

- A prim gets a pair when the shutter is not instantaneous and either its parent has a
  pair or `Xformable::transform_might_be_time_varying()` is true for it.
- The pair is composed with a new `compose_at(prim, parent, time)`. It is
  `compose_with_parent` under a nested `EvalTimeScope`, so the composition rule stays the
  one function every walk applies.
- `W_f` (at the frame) stays what lights, cameras, the zero-scale test and
  `count_placements` read. When `open == 0`, `pair.open` is `W_f` itself, and nothing is
  evaluated twice.
- A static prim below a moving ancestor is composed three times: at `f`, open and close.
  That is two matrix products plus the op stack read. Only moving subtrees pay this.

*Rejected:*

- **Re-running the import at the close time and diffing.** That doubles the import.
- **openusd's `XformCache`.** It is bound to one time, so it would need two caches, and the
  traversal does not use it today.

### D3. One placement rule: start at the open transform, end at the close transform

For a placement with local-to-world `X(t)`:

- start is `X(open)`;
- end is `T(v) · X(close)`, where `v` is the prim's `crust:motion:translate` (zero when
  unauthored).

Applied per kind:

- **Mesh, implicit shape**: `MeshPlacement` gains the end transform in place of
  `motion: Option<Vec3>`. "Moving" means `end != start`, so moving meshes still never
  bake. A non-invertible moving placement is baked at the open transform and counted
  under `xform.motion_ignored`.
- **Sphere**: centre and radius are taken from `X(open)`, and the sphere sits in an
  identity instance whose end is `T(v) · X(close) · X(open)⁻¹`. This generalises today's
  `T(v)`.
- **Native instance** (`attach_proto_parts`): the instance prim's pair gives
  `start = X(open)` and `end = T(v)·X(close)`. `part.local` (prototype-relative, at the
  frame) is folded in on both sides.
- **`PointInstancer`**: each placement `P_i` gets `start = W(open)·P_i` and
  `end = W(close)·P_i`, where `W` is the instancer prim's own pair. Per-instance
  time samples are not read (non-goal).
- **Curves, lights, volumes, prims inside a prototype**: no motion. When their transform
  varies over time, they are counted into `xform.motion_ignored` (D7).

### D4. The motion record stores the affine pair; the vector takes a per-hit velocity

`MotionRecord` gains `Affine(Box<[Affine3A; 2]>)` (start, end) for an `Own`-labelled
instance whose linear parts differ. `Translation(v)` stays: it is exact, 16 bytes, and
needs no inverse. At the first hit, when the layout has a `motionvector` slot:

```
M(t) = lerp_affine(S, E, t)                (the kernel's own interpolation)
L    = M(t)⁻¹ · p
v    = E·L − S·L                           (constant: the point's straight path)
```

`aov::motion_vector(cam, p, v, t)` is then unchanged. The rebase `P0 = p − t·v` is exact
because the path is linear in `t`. The vector still does not depend on the beauty's blur.
If `M(t)` is singular (a scale passing through zero), the record answers 0 for that
sample.

The cost is one 3×4 inverse per camera sample that hits rotating or scaling geometry, in
the AOV instantiation only. The table stays sparse and sorted by `geom_id`: 96 bytes plus
a box per moving geometry with a linear change.

*Rejected:*

- **The kernel returning the instance-local hit point.** It changes `crust-rt`'s hit
  record, which every hit pays for, to serve one AOV.
- **Storing `S⁻¹` and ignoring `t`.** That is wrong for blurred renders, whose hits are
  made at `t > 0`.

### D5. Offset-labelled placements get one record per id range

A native instance of a multi-part prototype is attached with `InstanceHitId::Offset(base)`,
followed by `reserve_slot`s. The ids `base ..= base + inner.max_hit_id()` are reachable
only through that placement, so one motion record covers the range.

The record table becomes sorted, non-overlapping ranges, with single ids as ranges of
length 1, and the lookup stays a binary search.

`As(k)` labels, forwarding inside nested instancers, and an inner scene that has motion
of its own stay `Unresolved`, with the existing warning. This change produces none of
these cases.

### D6. The sharp pose stays at shutter open

With blur off, rays keep `time = 0`, which is shutter open, as the current requirement
states. The VectorBlur workflow blurs forward from the sharp image, so a sharp image at
shutter open blurred by a forward vector covers exactly `[open, close]`.

*Rejected:* a sharp image at the frame `f` (`τ_f = −open / (close − open)`). With a forward
vector, it would blur over `[f, f + close − open]`, a different interval than the one the
camera authors.

### D7. Warnings: four codes, each summarised once

| code | class | raised when |
|------|-------|-------------|
| `camera.shutter_instantaneous` | Approximated | a `motionvector` var is requested, the shutter is instantaneous, and at least one drawn prim's transform varies over time |
| `camera.shutter_mismatch` | Approximated | D1's pre-read camera is not the camera rendered through, and their shutters differ |
| `time.motion_needs_frame` | Skipped | the shutter is not instantaneous, no `-f` is given, and a drawn prim's transform varies over time |
| `xform.motion_ignored` | Skipped | a time-varying transform on a kind that gets no motion (D3), with the count |

`xform.motion_vector_unsupported` keeps its code. Its text changes from "moves other than
by a translation" to "moves in a way the vector cannot attribute to one placement".

Each new code needs a row in `site/content/docs/reference/warnings.md`, and the existing
test keeps the two in step.

Detecting "varies over time" when the shutter is instantaneous costs the same
`transform_might_be_time_varying` query D2 makes. It is made only when one of the first
three warnings could fire: a `motionvector` var is requested, or the shutter is not
instantaneous. A static render with no shutter and no motion vectors makes no extra query
at all.

`camera.` and `time.` are settings families in `live.rs`: a scene that carries one of these
warnings has its MCP settings refresh refused, and falls back to a full re-import. That is
conservative and correct, since the warning could change with the settings.

### D8. MCP: a changed shutter is structural

The scene's provenance records the resolved shutter (open, close and the camera path it
came from). `refresh_settings` re-reads it the way it re-reads the adaptive key. A
different interval is `Structural("the camera shutter changed")`, so the edit re-imports.

This keeps the pair "an MCP edit applied in place ↔ a full re-import of the same layer"
bit-identical. A transform edit is already a full re-import.

### D9. Verified in numbers

- An end-to-end CLI test on `samples/animation.usda` with an override layer: a camera with
  `shutter:close = 1`, `-f 5`, and a `forward` var. `Mover`'s centre pixel is checked
  against an independent pinhole projection of the product's `P` channel moved 4/9 units
  along X (within 0.01 px), and the sky reads `(0, 0)`.
- Rotation: a cube spinning about the view axis. Each pixel's vector is checked against
  the projection of `R·P0 − P0` computed in the test from `P`, and the blurred render must
  agree with the sharp one.
- Bit-identity: every sample scene (none authors a shutter) through
  `scripts/check_images.sh` at 16 spp. The `AOV = false` instantiation's instruction count
  on cornellbox at 2 spp is unchanged under callgrind.
- Import cost: `--stats` import time on `samples/PointInstancedMedCity.usd` and the largest
  production stage available, before and after, with `bench_ab.sh`.

## Risks / Trade-offs

- **Rotation bows.** Element-wise matrix interpolation shrinks a rotating object
  mid-shutter. The blur shows the chord, and so does the vector, so the two agree, but a
  half-turn per shutter is visibly wrong.
  → It is already an `intersection-kernel` known gap. The design record states that the
  vector is the chord. The fix (slerp transforms) is a kernel change for later.
- **Two keys only.** A shutter longer than the animation's sample spacing reads its
  endpoints only, so a prim that goes out and back within the shutter reads 0.
  → Documented in Known gaps. Multi-segment motion would need N transforms per instance
  in the kernel.
- **Import cost on huge static stages.** One `transform_might_be_time_varying` query per
  `Xformable` prim, but only when a shutter is authored or `motionvector` is requested.
  → Measured (D9). If it shows up on a large stage, the query can be limited to prims
  whose ops have any time samples, through the stage's own sample index.
- **The camera pre-read may pick the wrong camera** on a stage with several unnamed
  cameras and different shutters.
  → `camera.shutter_mismatch` says so. Naming the camera fixes it.
- **Mixed-time evaluation.** With `shutter:open ≠ 0`, transforms are read at `f + open`
  while every other attribute, points included, is read at `f`. A skinned mesh whose
  points are baked per frame under an animated transform sits slightly inconsistently.
  → The follow-up change reads points at the same two times. Until then, the gap is
  documented.
- **A behaviour change for stages that already author a shutter.** Their animated prims
  start to blur in the beauty.
  → This is the point of the change. It is listed in the proposal, and no sample stage
  authors a shutter.

## Migration Plan

No data migration. Stages that author no shutter render bit-identically. To get time-sampled
motion vectors with a sharp beauty, author `shutter:close` (e.g. 0.5 or 1) on the camera
and `disableMotionBlur = true` on the render settings. The `aovs` user page documents this.
Rollback is reverting the change, since nothing persists.
