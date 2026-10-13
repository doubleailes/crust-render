## Why

A shadow-linked light reaches a receiver twice when the geometry its link excludes
also refracts light. The receiver's NEE and link twin see the light along a line with
the excluded surface removed. That is the link's meaning. The bounce that crosses the
same surface by transmission then reaches the light again, at the glass vertices' own
NEE and link twin. On ALab, every `UsdPreviewSurface` window is thick glass
(`opacity ≈ 0` maps to OpenPBR transmission), and the windows are excluded from all
three sun lights. So the sun reflected in the bench's glossy surfaces is counted once
in a straight line and once refracted through the window. In `crust light-paths`
these are pairs from the same camera sample: `C<RG>L 243` and `C<RG><TS><TS>L 231`.
These samples are part of the speckle in the shadowed areas of the bench close-up.

A reduced scene measures it: a glossy floor under a 5° sun, behind a thick glass slab
the sun's link excludes. With `--strategy bsdf` it reads **1.103 ± 0.016** against
**0.586** with no slab, so it is 1.9× too bright. `--strategy light` hides it, because
NEE rarely lands in a near-specular lobe. That is why the existing "the three
strategies agree" test never caught it.

## What Changes

- A surface a restricted light's shadow rays ignore **transmits nothing more from that
  light** to a path that reached it from a vertex that already estimated the light
  past it. At such a vertex, the light's NEE sample is dropped when its direction
  crosses the surface. Its link-twin hit is dropped when the sampled bounce crosses
  the surface. A bounce that crosses on through delta transmissions collects nothing
  from the light either. Reflection off the surface is unchanged.
- The rule follows a chain of transmissions through excluded surfaces (a pane's two
  faces) back to the vertex that owns the straight estimate. A path that starts at
  the camera has no such vertex. The camera still sees a linked light through
  excluded glass, once.
- Both halves of each MIS pair drop the same paths, so `light`, `bsdf` and `power`
  still estimate one image. That image is now the scene with the excluded surfaces
  absent, as far as the light's transmission is concerned.
- **BREAKING** (images): renders with shadow links whose excluded geometry transmits
  get darker where they were double-counted. ALab's sun through its windows is the
  known case. Scenes without restricted shadow links are bit-identical.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `lighting`: the **Shadow linking** requirement gains the rule that an excluded
  surface transmits nothing more from the light along a path that already estimated
  it past that surface, with scenarios for the excluded glass slab and for the camera
  seeing the light through excluded glass.

## Impact

- `crates/crust-core/src/tracer/path.rs`: per-vertex transmission chain
  (`PrevBounce::through`, the chain at the current vertex), the drop in
  `surface_light_sample` and `link_twin`, and the bounce-side lookups after a delta
  vertex (`bounce_emission_weight_at`, `escaped_emission` ↔ `escaped_split`).
- `crates/crust-core/src/rt_world.rs`: `World::mask(geom_id)`, a per-geometry mask
  table kept only when shadow links encode occluder classes. The importer's link
  resolution (`scene/usd_import/light_links.rs`) asks for it.
- Tests: `crates/crust-core/tests/light_linking.rs` (the excluded glass slab, every
  strategy).
- Documentation: `openspec/specs/lighting/design.md` § Shadow linking and its known
  gaps, `docs/alab_profile.md` (before/after), and the user-facing light-linking page
  in `site/` if it describes shadow-link semantics.
- No new environment switch, CLI flag or USD attribute.
