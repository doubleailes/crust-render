# Design

## Context

See `proposal.md` (Why) and the `render-passes`, `cli` and `scene-check` deltas
for the behaviour. This design builds on #340 (`render-pass-matte-and-camera-visibility`)
and on the code as it stands. What constrains it:

- **Masks are baked at commit, in many places.** `crust_rt::SceneBuilder::set_mask`
  is "only valid before commit". `commit` copies each geometry's `RayMask` into:
  - every `TriangleRecord` (scene/mod.rs);
  - every leaf packet's `LaneMasks` (`masks`, `mask_and`, `mask_or`, triangle.rs);
  - every `PrimNode` (sphere, disk, cylinder, curve);
  - every `InstancePrim` (prim.rs).

  Traversal tests these copies, and there is no per-geometry mask table at trace
  time. Instanced prototypes are separate committed `Arc<Scene>`s with their own
  masks; the instance-level mask composes over them.
- **The world has no post-commit setters.** `World`'s holdout table is private and
  read-only. `replace_materials` is its only `&mut` method.
- **Pass state lands at dispatch, before commit.** `ActivePass::saw` writes
  holdouts and clears `MASK_CAMERA` straight into `WorldBuilder`.
  `ActivePass::light_added` hides lights at infinity from the camera.
- **Light linking is pass-dependent and writes masks.** `LightLinks::resolve`
  runs after the traversal and before commit:
  - `demote` removes lights linked to nothing, and keeps one as a backdrop or a
    camera-only emitter only if the camera still sees it, which is the pass's
    camera bit;
  - `encode_shadows` writes shadow classes into bits 3–31 of geometry and volume
    masks;
  - `set_light_classes` and `LightList::set_links` record the result.

  `LightList::remove` is allowed only before `set_links`.
- **The light selection is per renderer already.** `Renderer::new` builds it
  (`select_lights`) from the world, camera, resolution and frame.
  `Renderer::into_parts` gives the world and lights back.
- **"Same import?" is already decided once.** `usd_import::live::refresh_settings`
  refuses an in-place MCP settings edit when any `StageFacts` field moves:
  - working colour space;
  - subdivision level;
  - adaptive-subdivision camera key;
  - implicit-shape projection;
  - shutter key;
  - product primvars;
  - render camera;
  - `domeLightCameraVisibility`.

  That is exactly the set of settings the import bakes into the scene.
- **Settings readers are pass-aware.** Every settings, products, camera and dome
  reader takes `pass` and goes through `render_settings_path(stage, pass)`.
  `check` already keeps the stage open (`keep_stage`).

## Goals / Non-Goals

**Goals:**

- **One code path decides a pass's visibility.** `--pass` and `--all-passes`
  share it: `--pass X` is the one-pass case of the same machinery, so
  bit-identity is structural, not coincidental.
- **No per-ray cost.** No side table is consulted in traversal. A render with no
  pass keeps its golden images and its zero-AOV instruction count.
- **One grouping rule.** It is the predicate MCP already uses, so "shares an
  import" and "is an in-place settings edit" cannot disagree.

**Non-Goals:**

- **`prune`.** It changes what the traversal emits.
- **Volumes in pass collections.** These still warn.
- **`--all-passes` on `diagnostic`, `light-paths` or MCP sessions.**
- **Rendering passes concurrently.** Passes run one after another; each render is
  already parallel.
- **A `<pass>` output-path token.** The archived output-path design rules out
  non-frame variables, and per-pass products plus the collision check cover the
  need.
- **Narrowing the BVH to a pass.** Hidden geometry stays in the tree and is
  skipped by its mask.

## Decisions

### D1. A pass is a mask table, computed by one function

The per-pass state is a `PassState`, computed from a pass-independent **base**:

```
                 traversal (once per import group)
                          |
      base: per-geom masks before any pass or link,  base light list (all lights),
            unresolved LightLinks,  per-pass membership (matte / camVis / renderVis)
                          |
          +---------------+----------------+
          |  apply_pass(base, pass) -> PassState      (one function)
          |    1. masks  = base masks; clear MASK_CAMERA where camVis excludes,
          |               set 0 where renderVis excludes; holdouts where matte
          |               (and not render-hidden)
          |    2. lights = base lights minus those outside renderVis (remove,
          |               allowed before links); hide_from_camera per D3 of #340
          |    3. LightLinks::resolve over (masks, lights)   -- demote, shadows
          +---------------+----------------+
                          |
       single pass: write the masks into WorldBuilder, then commit
       all passes:  commit once with pass 1's masks, then remask per pass (D2)
```

`ActivePass` stops writing into `WorldBuilder`. It records, per pass, the
membership of each prim's geom-id and volume ranges (three bits), and the
membership of each light. `LightLinks::resolve` writes masks, light classes and
the remap into the table rather than into `WorldBuilder`. That one call site
change is what lets the same resolver serve both appliers.

- **Rejected: a per-geometry visibility side table tested at hit time.**
  `renderVisibility` hides from shadow rays too, so every occlusion query would
  pay a lookup in the kernel's innermost loop, for every render. It would also
  make `--pass` and `--all-passes` take different paths through traversal.
- **Rejected: keep writing at dispatch and re-import per pass.** That is the
  current gap: it gives the workflow but not the shared import the user chose.

### D2. crust-rt rewrites masks in place

`crust_rt::Scene` gains `remask(&mut self, mask_of: impl Fn(u32) -> RayMask)`.
It rewrites the top-level copies of every geometry's mask (triangle records, leaf
packets' `LaneMasks` with their `mask_and` / `mask_or`, prim nodes, instance
prims) without touching topology or bounds.

- **Prototype scenes are never rewritten.** A pass judges membership on the
  instance prim (#340 D3), so only the `InstancePrim` mask changes, and a shared
  `Arc<Scene>` prototype stays shared.
- **Bit-identity rests on one property: BVH construction does not read masks.**
  The first task checks that (SBVH splits, leaf packing and packet grouping).
  Then a test pins `remask(commit(a), b) == commit(b)` structurally, record for
  record and packet for packet, for random mask tables. If leaf packing turns out
  to depend on masks, the fix is to make it not depend on them, verified against
  the goldens. Pinning the build is the simpler invariant.
- **Spike result (task 1.1): it does not.** `build.rs` never reads a mask; the
  splits and the binned SAH see only bounds. `collapse` packs each leaf's
  triangles into packets in reference order and copies each record's mask into
  its lane, and degenerate records are decided by their indices. Masks gate
  traversal only. `a_build_does_not_depend_on_masks` (crust-rt
  `scene/tests.rs`) commits one scene of every geometry kind under eight pairs of
  random mask tables, in both packet layouts, and finds nodes, leaves, packets,
  indices and records identical word for word apart from the masks. No packing
  change was needed.
- `World` gains `apply_pass(&mut self, &PassState)`. It calls `remask`, replaces
  the holdout table, the light classes and the transparent-emitter flags, and
  rewrites volume-region masks.
- **Rejected: patching through `Arc::make_mut` on prototypes.** No pass ever
  needs it, and it would silently copy a prototype that a cache shares.

### D3. Groups come from the `StageFacts` predicate

`refresh_settings`' refusal checks are extracted into
`ImportKey::of(stage, pass, options)`, a value with `PartialEq`, beside
`StageFacts` in `usd_import/live.rs`. `refresh_settings` compares two keys, and
`--all-passes` groups passes by key in `ls pass` order. A group is imported once,
with every pass of the group judged in the one traversal. The streaming
population mask then holds every pass prim of the group.

- The render camera is part of the key only when the import used the projection
  (adaptive dicing, on-screen implicit shapes), exactly as `refresh_settings`
  decides. Two passes through different cameras otherwise share an import.
- **Rejected: a hand-written list of "import-time" attributes.** It would drift
  from MCP's list, and the spec scenario of differing dicing settings would be
  the only check.

### D4. Per-pass settings are re-read from the kept stage

The import of a group returns the shared `Scene` plus one `PassScene` per pass:
`settings`, `camera`, `aovs`, `settings_prim`, `PassSummary`, and the warnings
raised while reading that pass. These come from the same readers `load_scene`
uses, on the kept stage, with that pass's `pass` argument. That is the
`refresh_settings` path, so the camera is rebuilt by the code that builds it for
MCP edits.

### D5. The render loop lives in crust-render

`render_and_write` is split at the point where it takes the `Scene` apart:
`render_pass(world, lights, volumes, PassScene, ...)` returns the world and
lights through `Renderer::into_parts`. `--all-passes`:

1. lists the passes, groups them (D3), and computes every pass's product files
   (`select_products`, frame expanded);
2. refuses a collision across all groups before importing anything heavier than
   the index, so a collision costs no import;
3. per group: imports once, then per pass `world.apply_pass`, renders, and
   writes the files.

The loop polls the interrupt flag between passes (spec: "Interrupting all
passes"). The log gets one INFO line per group ("imported once for N passes")
and one per pass. That keeps INFO bounded by the number of passes.

### D6. `crust check --all-passes` reuses `check_scene`

The no-pass description is the existing report. Each pass entry is built by the
same `check_scene` pieces over that pass's `PassScene`, then diffed field by
field against the no-pass entry: camera, resolution, depth of field (a lens with
`fStop > 0` and depth of field not disabled), motion blur (shutter open), each
`effective_settings` entry, and products. Depth of field and motion blur are
always printed per pass, because a leak shows up as a value that did not change.
`passes_share_output` is a static finding raised by the same collision function
`render` uses.

### D7. The render-hidden count and the precedence rule live in `ActivePass`

`judge` returns three bits. Render-hidden is checked first: such a prim is not
counted as held out or camera-hidden. That matches the spec's precedence rule,
and the check counts follow from it. Mask edits only ever clear bits (`&`), so
the existing guarantee that a pass never reveals what `crust:rayMask` hides
carries over.

## Risks / Trade-offs

- **[BVH build depends on masks]** If leaf packing groups triangles by mask,
  `remask` cannot reproduce `commit(b)`. → The first task checks this. If it
  does, make packing mask-independent, verified with `check_images.sh` and the
  SIMD matrix, before anything else is built on it.
- **[Missing a mask copy]** `remask` misses a place masks are copied to (a future
  prim kind, a curve packet), and a pass leaks visibility. → The structural
  equality test runs over every geometry kind in one scene, and the
  bit-identity test renders `render_passes.usda` both ways. A new geometry kind
  without a `remask` arm fails the test by construction.
- **[Light linking reshaped]** `resolve` moving from `WorldBuilder` writes to a
  table is the riskiest refactor. → The no-pass and `--pass` goldens and every
  `light_linking.rs` / `render_passes.rs` test must stay bitwise unchanged before
  `--all-passes` is wired.
- **[Hidden geometry still costs time]** A plate pass that hides 95% of the set
  still traverses it. → Documented as a known gap. A user who needs speed
  renders that pass alone with `--pass` (whose import is no smaller today
  either).
- **[Memory]** The base keeps per-geom base masks and N per-pass membership
  tables: one byte per geometry per pass, small next to the geometry itself.
- **[A pass's warnings]** Warnings raised once for the shared import must not
  repeat per pass, and pass-only warnings (`render_pass.*`) must name their
  pass. → `PassScene` carries only what reading that pass raised. The shared
  import's warnings are reported once.

## Migration Plan

None for users. `--pass` keeps its meaning, and passes that author no
`renderVisibility` restriction render bitwise as before. A pass whose
`renderVisibility` restricted something used to warn
`render_pass.collection_not_read`. It now hides that geometry, and the warning
remains only for `prune`. The warnings page and the render-passes page say so.

## Open Questions

- Whether `domeLightCameraVisibility` can leave the import key. It is applied to
  the light list after the import, so it could be per pass. It is kept in the key
  until a test shows a per-pass application is bitwise equal.
