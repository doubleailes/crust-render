## Context

The motivation, the ALab evidence and the reduced-scene numbers are in proposal.md
§ Why.

### Where the second count comes from

At a continuous vertex, a restricted light is estimated by NEE and by its link twin
(`link_twin` in `tracer/path.rs`). Both are shadow rays with the light's mask, so they
pass every surface the link excludes. The ordinary bounce collects nothing from a
twinned light there (`LightList::bounce_skips`): one owner per vertex.

The rule is per vertex, though, and the path keeps going. ALab's windows are
`UsdPreviewSurface` with `opacity ≈ 0`, which the importer maps to OpenPBR
transmission. Thick OpenPBR transmission is a **continuous** Walter BTDF with a
roughness floor (`transmission_is_continuous`), not a delta lobe. In the light-path
report the `S` in `<TS>` only labels the lobe as near-specular. So a bounce from the
bench that refracts into the pane makes two more continuous vertices, the pane's two
faces. Each runs its own NEE and twin toward the sun, and each of those shadow rays
also ignores the pane. The bench's twin had already counted the sun along that line
with the pane absent. That is the duplicate.

Thin-walled panes do not have the problem. The bounce crosses them as thin walls
(`pass_walls`, no path vertex), so the escape's previous vertex is still the receiver,
and `bounce_skips` already applies. A reduced scene confirms it: with a thin-walled
slab, all three strategies match the no-slab floor (0.5856 / 0.5886 / 0.5844
against 0.5856).

### What identifies an excluded surface

`light_links::encode_shadows` writes each occluder's class bit into its geometry mask,
and gives each restricted light a shadow mask holding the bits of the classes it sees.
"Light `i`'s shadow rays ignore the geometry `g`" is therefore
`!lights.shadow_mask(i).sees(mask(g))`. That includes geometry that casts no shadows
at all. An instance's reserved slots take the instance's mask (`attach_proto_parts`).
So a hit's `geom_id`, whichever placement it reports, carries the mask the kernel
tested. The committed `World` keeps no masks today: the kernel folds them into the
BVH leaves.

## Goals / Non-Goals

**Goals:**

- Drop exactly the duplicated paths: `owner → (transmission through excluded
  surfaces)+ → light`, on both MIS sides, so the three strategies keep estimating one
  image, and that image is the no-surface image for the light's direct transmission.
- Keep scenes without restricted shadow links bit-identical, and keep the
  instruction count of unlinked renders flat (callgrind, cornellbox, `-s 2`).
- Keep the pairs `CLAUDE.md` names: surface NEE ↔ twin; `escaped_emission` ↔
  `escaped_split`; `C.*[LO]` ≡ beauty.

**Non-Goals:**

- Paths that start at the camera. Today each glass face estimates the light with the
  glass absent, so the camera sees a linked sun at the once-refracted angle *and* the
  exit angle. This change leaves that as it is (D3, a gap).
- Lights without a restricted shadow set, crossing surfaces that cast no shadow at
  all (`crust:rayMask` without the shadow bit). That is the same double count, outside
  shadow linking. It is unchanged, and noted as a gap.
- Volumes. A phase vertex is an owner (it runs NEE), never a transmission through a
  surface.

## Decisions

### D1. Track a transmission chain per path, as one mask

Each vertex carries `chain: Option<RayMask>`: the union of the masks of the surfaces
crossed by transmission since the last **owner**, or `None` when there is no owner.
An owner is a continuous vertex whose bounce reflects, or a phase vertex. Both ran NEE
and their twin along the outgoing direction.

- At a surface hit `x`, `chain_here = prev.through().map(|m| m | world.mask(x))`.
- What `x` hands on (`PrevBounce::through`):
  - if its bounce transmits, `chain_here` (still `None` without an owner);
  - if its bounce reflects and `x` is continuous, `Some(NONE)` (`x` is the new owner);
  - after a delta reflection, `None`.
- A phase vertex hands on `Some(NONE)`. The camera and a carried-medium (subsurface)
  exit hand on `None`.
- "Transmits" is geometric: the sampled direction leaves on the far side of the
  ray-facing normal (`n·ω_in · n·ω_out > 0`). That works for every material,
  including closures, whose `delta_event` does not tell reflection from transmission.

Light `i` is dropped at `x` when `chain_here = Some(m)`, the light is restricted, and
`!shadow_mask(i).sees(m)`.

*Alternative, one chain per light:* exact when a chain passes a surface the light
does see and then an excluded one. Its cost is a vector per vertex, for a case where
the one-mask test only keeps the old double count (a leaf in front of a window). That
case is recorded as a gap.

*Alternative, marking excluded surfaces "absent" for the bounce ray too, so it
passes straight through:* that changes what every other light sees through the glass.
Excluded geometry is a per-light idea, and the bounce ray serves all lights.

### D2. Drop on both MIS sides, by direction

At a vertex with a chain that passes for light `i`:

- **NEE** (`surface_light_sample`): a sample of light `i` whose direction transmits
  contributes nothing. The test sits next to `illuminates`, before the shadow ray, so
  a dropped sample casts no ray.
- **Link twin** (`link_twin`): when the sampled bounce transmits, light `i` is
  skipped. A twin's weight of zero already means "no ray".
- **Bounce side after a delta vertex**: `bounce_emission_weight_at`, `escaped_emission`
  and `escaped_split` skip light `i` when `prev` is delta, its `through` passes for
  `i`, and `bounce_skips(i)`. That is the case where no twin ran at the last vertex.
  After a continuous vertex, `bounce_skips` already hands the light to that vertex's
  twin, which D2 has filtered.

The NEE and bounce sides drop the same set of directions, the transmitted hemisphere
of the same vertex, so the MIS weights still sum to one over what is left. NEE-only
(dome) lights take the NEE drop. Their bounce side already collects nothing at a
continuous vertex while the strategy samples lights.

### D3. Camera-started chains keep today's behaviour

Without an owner, nothing estimated the light past the glass, so nothing is a
duplicate *of an owner*. Making the entry face an owner would put the light at the
once-refracted angle, which is wrong for a sun seen through a pane. The correct
answer needs one estimate along the *exit* direction, which a camera chain cannot
know at its entry face. Deferred, and recorded as a gap.

### D4. `World::mask` only for linked scenes

`WorldBuilder::keep_masks()` asks `commit` to copy each geometry's mask into a
`World::masks` table (4 bytes per geometry). `encode_shadows` calls it after it
rewrites the masks. Unlinked scenes keep an empty table, and `World::mask` answers
`MASK_ALL` there, which every shadow mask sees. The kernel (`crust-rt`) is not
touched.

### D5. Gate everything on `LINKED`

`trace_path` was already monomorphised on `TWINS` (the scene has a twinned light). This
change widens that flag to `LINKED`, meaning any restricted shadow set
(`LightList::shadow_linked`). A scene whose only restricted light is a dome now runs
the linked instantiation too, with an empty twin loop. The chain is computed only
there. Elsewhere `chain_here` is the constant `NONE`, and the NEE test folds away.

The first build gated on `lights.links()` at run time and cost cornellbox **+0.59%**
instructions (callgrind, `-s 2`), all of it in the inlined integrator. Monomorphised,
it runs 4 244 804 411 against 4 244 820 933 (−0.0004%).

## Risks / Trade-offs

- [ALab images change, in the sun-through-window paths] → That is the fix. The
  before/after is measured (beauty and per-light glossy means, two-seed variance) and
  recorded in `docs/alab_profile.md`.
- [The extra NEE parameter shows up in the unlinked instruction count] → Measured: the
  run-time gate did (+0.59%), and the `LINKED` instantiation (D5) removed it.
- [Bump-mapped glass misclassifies a grazing reflection as a transmission] → The
  transmission test uses the ray-facing (shading) normal, which a normal map can tilt
  across a grazing direction. That is rare, and the error is the old double count.
  Using the geometric normal would need a field the hit does not carry.
- [The one-mask approximation (D1)] → It keeps the old double count only when a
  chain mixes surfaces the light sees with ones it ignores. Recorded as a gap.
