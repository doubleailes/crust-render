## Context

See proposal.md (Why). The pieces this builds on:

- `crust_rt::RayHit` reports, for a curve hit, `prim_id` (one per linear
  segment or cubic span, within the instanced scene for an instance hit), the
  span-local `u` in [0, 1], and `dpdu`, the strand direction carried through
  every placement. The kernel knows nothing of strands, only of spans.
- `usd_import/shapes.rs::curve_segments` walks each curve's spans in order and
  already knows, per span, its curve index and its position among that curve's
  spans; it throws both away. `NurbsCurves` produce spans the same way, one per
  non-empty knot interval.
- `rt_world.rs` maps a triangle hit to its authored polygon through a
  per-geometry `FaceMap` / `SideTables`, built only for geometry that needs it.
  That is the pattern for a span → strand map.
- `crust_mtlx::ShadeCtx` carries `uv`, `normal`, `tangent`, `view`, `position`
  and `uv_width`. `geompropvalue` compiles only as the default chart (`st`); any
  other `geomprop` is an unsupported node, reported and compiled to its
  default.
- The JIT is pinned bit-identical to the interpreter (`crust-jit/tests`), and
  `resolve` to per-query shading (`crust-core/tests/resolve.rs`). Both pins
  extend to every new operator and context field.

## Goals / Non-Goals

**Goals:**

- A material can read, on a curve hit: the position along the strand, the
  curve index, a per-curve random number, and any authored curve primvar.
- Nothing is paid by a scene whose curve materials read none of it: no table
  built, no hit-record field filled, and a bit-identical image.

**Non-Goals:**

- Ribbons (`normals`), `wrap = periodic` and `pinned` curves.
- Primvars on meshes beyond what is read today (`st`, a second UV set): the
  `geompropvalue` path built here is general, but meshes adopting it is its own
  change.
- `faceVarying` primvars on curves (USD defines it per segment end; no groom
  tool we know of authors it).

## Decisions

### `v` is along the strand, mapped by the host

A span keeps `(curve_index, v0, v1)`; a hit's `v = v0 + u · (v1 − v0)`.

- **`v0, v1` are the span's interval of the curve's parameter**, normalised so
  the curve's first authored point is 0 and its last is 1. For a cubic curve
  with `n` spans that is `[k/n, (k+1)/n]`; for a linear curve, `[k/(cnt−1),
  (k+1)/(cnt−1)]`; for NURBS, the knot interval over the curve's (narrowed)
  domain.
- **Alternative: arc length.** A ramp would then be uniform in distance along
  a strand whose control points are unevenly spaced. Rejected for now: USD
  interpolates `vertex` primvars over the curve parameter, not arc length, so
  `v` in parameter space is what makes a `v`-driven ramp agree with an authored
  `vertex` ramp on the same strand. Arc length can be a second property later.
- **Alternative: carry `v` in the kernel.** Rejected: `RayHit` stays free of
  host notions, and the map is one table lookup at a hit that is about to run a
  material anyway.

### Curve properties have fixed `geomprop` names

`geompropvalue` with `geomprop` = `curve:v`, `curve:index` or `curve:random`
reads the hit's properties. These names are crust's and are documented with
the `crust:*` attributes. A USD primvar name may itself be namespaced
(`primvars:curve:v` is legal), so the `curve:` prefix is reserved: an authored
primvar under it is refused with a warning naming it, and the property wins.

- **`curve:random`** is a hash of `(prim path hash, curve index)` to [0, 1),
  not the index itself: neighbouring indices get uncorrelated values, and two
  prims do not repeat each other's pattern. The hash is the workspace's
  existing integer hash (no RNG outside openqmc: this is a deterministic
  function of the curve, not a sample).
- **Its stability is the curve order's.** A strand keeps its value across
  frames and renders as long as the prim keeps its curves in the same order,
  which a simulation cache with constant topology does. Reordering the curves
  of a prim reassigns the values. A groom that needs order-independent
  variation authors a stable per-curve id as a `uniform` primvar and feeds it
  to `randomfloat`'s `seed` through `geompropvalue`; the documentation says so
  where it describes `curve:random`.
- **Alternative: hash the strand's root position.** Order-independent, but it
  changes whenever the root moves (a groom edit, a simulated scalp), which is
  the more common change. Rejected.
- **Alternative: MaterialX's own `geomcolor` / `geompropvalue` defaults.** They
  have no notion of a curve, so a name is needed either way.

### Primvars are read on demand, per bound material

At import, the material's compiled program lists the `geomprop` names it reads.
Only those primvars are decoded, and only on the curve prims bound to it.

- **The default chart is a need of its own.** `texcoord` (index 0) and an
  image reading the shading point's chart name no `geomprop`
  (`compile.rs`, `is_default_chart`), so the named list alone would miss
  them. Curves ask the same two questions meshes already do —
  `Material::uses_uv()` and `Material::uv_primvar()`, as `MeshNeeds::of`
  (`usd_import/mesh.rs`) — and decode `primvars:st` (or the named chart) when
  either says so, independently of the `geomprop` list.
- **Storage**: `constant` and `uniform` values per curve. `varying` values per
  span end, interpolated linearly in `u` within the span, which is what USD
  specifies for `varying`. `vertex` values follow the curve's own basis: per
  cubic span, its four control values converted to Bézier form by the same
  `basis_to_bezier` the span's points go through, and evaluated as a cubic in
  `u` at the hit; per linear segment, its two end values. A `vertex` ramp is
  then exactly what USD's interpolation gives, inside a span too, not only at
  its ends. That is four values per span per channel instead of two; the
  tables exist only for primvars a bound material reads.
- **Alternative: span-end values, linear between.** What widths do today
  (`span_end_values`), and half the memory. Rejected for primvars: a colour
  ramp authored on a few control points spans a strand in one or two spans,
  where linear-between-ends visibly flattens the authored shape.
- **Alternative: decode every primvar.** Rejected: a dense archive has tens of
  millions of spans, and a groom authors many primvars a given material never
  reads.
- **Types**: `float`, `float2`, `float3`, `color3f` (with its colour space, per
  `docs/color_management.md`), `int` read as float. Anything else is refused
  with a warning naming the primvar.

### `ShadeCtx` gets one optional curve record

`ShadeCtx` gains `curve: Option<CurveCtx>`: `v`, `index`, `random`, and a slice
of the hit's decoded primvar values in the order the program asked for them. A
mesh hit leaves it `None`; `geompropvalue` of a curve property then returns the
node's `default`, as MaterialX specifies for an absent property.

- `primvars:st` on a curve fills `ShadeCtx::uv`, so an image read by
  `texcoord` works on curves as on meshes. It is decoded whenever the material
  uses the default chart (see "Primvars are read on demand"); a curve material
  that uses the chart on a prim authoring no `st` reads (0, 0), as a mesh
  without one does today.
- The JIT reads the record through the same accessor layout as the interpreter,
  and the bit-identity test gains a curve fixture.

### The new operators follow MaterialX's stdlib

`rgbtohsv`, `hsvtorgb`, `ifgreater`, `ifgreatereq`, `ifequal`, `randomfloat`
and `randomcolor` are implemented to MaterialX's own definitions and pinned by
the OSL oracle (`scripts/osl_oracle.py`, `tests/osl_oracle.rs`) where OSL
implements them. The random nodes take a `seed`; their exact hash is checked
against MaterialX's reference implementation when the oracle is regenerated,
and the design record says so if it cannot be matched bit for bit.

## Risks / Trade-offs

- **[Hit-record growth costs every ray]** → the curve record is filled only
  when the geometry carries a curve table, tested before any work, as the
  tangent's normalisation already is; re-measure `cornellbox` by callgrind and
  hold it to the tangent's `+0.34%` or better.
- **[Memory on dense archives]** → tables are built only for curves bound to a
  material that reads a curve property; `--stats` reports their size.
- **[A `v` ramp meets the span kinks of a non-uniform curve]** → documented:
  `v` is the curve parameter, matching USD's primvar interpolation; arc length
  is a later property.
- **[MCP live edits]** → a material edit that starts reading a new primvar
  changes what the import must decode, so the set of read primvars belongs in
  `Signature` (`rt_world.rs`) and triggers the re-import path.
- **[Instanced grooms]** → a prototype's spans carry their own tables; a hit's
  `prim_id` is already prototype-local, so the map is per prototype geometry,
  scoped by the stage epoch like every prototype-keyed cache.
