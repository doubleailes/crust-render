## Why

A material on a curve knows only where it was hit and the strand's direction
there. It cannot tell a root from a tip, one strand from its neighbour, or read
any primvar the groom authors: a curve hit carries no UV
(`rt_world.rs`, the curve branch of `intersect`), `geompropvalue` understands
only `st` (`crust-mtlx/src/eval/compile.rs`, `is_default_chart`), and the
`BasisCurves` import reads points, counts and widths and nothing else
(`usd_import/shapes.rs`, `curve_segments`). The materials design record already
lists it as a gap: "curve primvars do not reach materials, so a per-strand colour
cannot be authored yet".

That rules out the most ordinary hair and fur looks — a root-to-tip colour ramp,
a tip that fades out, per-strand hue and value variation, a fraction of strands
in a different colour — and it blocks `curve-mass-field`, whose bake needs each
strand's position along its length and its identity.

## What Changes

- **A curve hit knows where it is on the strand.** Every curve hit carries `v`,
  its position along the whole strand in [0, 1] from the first authored point to
  the last (not along the span the kernel hit), and the index of the curve
  within its prim.
- **Materials can read them.** MaterialX `geompropvalue` gains curve
  properties: the position along the strand, the curve index, and a per-curve
  hash of it in [0, 1) for random variation that does not depend on how the
  curves are ordered.
- **`BasisCurves` / `NurbsCurves` primvars reach materials.** `constant`,
  `uniform` (per curve), `vertex` and `varying` primvars of `float`, `float2`,
  `float3` and colour types are read and interpolated along the strand, and
  `geompropvalue` reads them by name. `primvars:st` on curves fills the chart a
  `texcoord` reads, as it does on a mesh.
- **The MaterialX operators those ramps need.** `rgbtohsv` / `hsvtorgb`,
  the conditionals (`ifgreater`, `ifgreatereq`, `ifequal`), and a hash-based
  `randomfloat` / `randomcolor` on a seed input, so a per-strand variation can
  be written as a graph, the way MaterialX documents spell it, rather than
  approximated with `floor` and `clamp`.
- Meshes, and curves whose material reads none of this, render bit-identically
  to today.

Out of scope: ribbons (`BasisCurves` `normals`, which this change still does not
read: a separate change), `wrap = periodic`, and any new hair lobe.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `usd-scene-import`: the `BasisCurves` / `NurbsCurves` requirements gain the
  position along the strand, the curve index and primvars, read at import and
  carried to the hit.
- `materials`: `geompropvalue` reads curve properties and curve primvars; the
  MaterialX operator set gains the HSV conversions, the conditionals and the
  random nodes.

## Impact

- **crust-rt**: unchanged. `RayHit` already reports a curve hit's span-local
  `u` and its `prim_id` (the span), inside instances too; mapping the span to
  its strand is the host's, as mapping a triangle to its polygon already is.
- **crust-core**: the curve import keeps, per span, its curve index and its
  `v` interval, plus the primvars a bound material asks for; `rt_world` maps a
  hit to them the way `FaceMap` maps a triangle to its polygon. The hit record
  grows a curve field; the `+0.34%` the tangent cost (materials design record)
  is the budget to beat, and the zero-AOV / no-curve instruction counts are
  re-measured.
- **crust-mtlx / crust-jit**: `ShadeCtx` grows the curve properties and a named
  primvar lookup; the new operators are interpreted and compiled, pinned
  bit-identical between the two.
- **Memory**: one curve index and one `v` interval per span. A dense archive
  attaches tens of millions of spans (the import's own comment), so the table
  is only built for curves whose material reads it.
- **Docs**: `site/content/docs/usd/` (curve primvars and the properties
  `geompropvalue` can read), the materials and usd-scene-import design records'
  known-gaps sections.
