## Why

Art-directed, stylised hair wants its colour designed into the *mass* of the
hair — hue and value varying across clumps and through the volume as the art
direction paints them, independently of the lighting — with the specular kept
as a separate, separately controlled layer. Rendering each strand with its own
colour and its own shading normal defeats that: wherever two strands or ribbons
cross, they disagree on both, and the crossing reads as a hard line. The
normal is what gives it away most, since the diffuse lighting changes abruptly
where the front strand changes. Thick semi-transparent ribbons soften the
colour but not the normal, and their width is fixed when the groom is
simulated, so trying another look means simulating again.

A field built from the strands themselves answers both: two strands crossing
at a point read the same colour and the same normal there, so the
interpenetration has nothing left to show, and the look is controlled by the
field's parameters rather than by the simulated geometry. The mechanism is
prior art — point-cloud and brickmap baking, density fields and their signed
distance — applied to curves.

## What Changes

- **A mass field per groom.** At import, a `BasisCurves` / `NurbsCurves` prim
  (or a group of them) that opts in is baked into a 3D field around it: the
  strands' coverage density, splatted with an authored radius; the signed
  distance to the density's iso-surface at an authored threshold; and,
  optionally, a colour splatted from the strands, weighted by coverage.
- **Lookups for materials.** New MaterialX nodes read the field at a hit:
  the mass colour, the mass normal (the gradient of the signed distance, the
  normal of the nearest point of the mass's surface, defined inside the mass
  too), the depth into the mass, and the coverage density.
- **A mass diffuse leaf.** A diffuse lobe that shades a curve hit around a
  normal it is given — the mass normal — rather than the tube's: its
  hemisphere is that normal's, and its light, like a hair fibre's, passes out
  of the strand it starts in, so a strand lit from behind its own tube still
  lights as the mass's surface would.
- **Blending is the material's.** How much of the mass colour and normal a
  strand takes — everywhere, along `v`, by depth, by density — is authored in
  the material graph with ordinary `mix` nodes, so the engine exposes the
  field and no look.
- **The specular is untouched**: a hair lobe in the same material keeps the
  strand's own tangent.
- Curves that do not opt in, and every mesh, render bit-identically to today.

Related future work, not in this change: an artist-friendly Marschner-family
hair lobe (independent per-lobe shift, width and colour, and glints, after
Marschner et al. 2003 and Sadeghi et al. 2010); shadow transmittance read from
the field's density instead of from individual strands; ribbons.

## Capabilities

### New Capabilities

- `curve-mass-field`: baking curves into a mass field (density, signed
  distance, colour), the `crust:mass:*` attributes that control it, its
  lookups, and its rebuild on edit.

### Modified Capabilities

- `materials`: the mass lookup nodes and the mass diffuse leaf, and their
  place among the hair and curve rules (light passing out of the strand,
  mixed-leaf vertices).
- `usd-scene-import`: curves gain the opt-in mass attributes and their
  grouping.

## Impact

- **Depends on** `curve-attributes-to-materials`: the colour bake evaluates a
  graph at strand samples that reads the position along the strand, the curve
  index and primvars.
- **crust-core**: a bake pass after the USD read (splat, smooth, signed
  distance), parallel and deterministic; a field store beside the world; the
  lookups in the shading context; the mass diffuse leaf in the closure tree;
  `Signature` entries so an MCP edit of a mass attribute or of the colour
  graph rebuilds the field.
- **crust-mtlx / crust-jit**: the lookup nodes and the new leaf, interpreted
  and compiled, pinned bit-identical.
- **crust-rt**: unchanged; the leaf reuses `Ray::ignore_curve_exits`.
- **Memory and time**: a field of a few to a few tens of MB per groom at
  production radii, and a bake per frame; both reported by `--stats`. Nothing
  for a scene without an opted-in groom.
- **Docs**: a `site/content/docs/usd/` page for the `crust:mass:*` attributes
  and the nodes, the materials and usd-scene-import design records, and a new
  `curve-mass-field` design record.
