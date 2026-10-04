# intersection-kernel Specification

## Purpose

Answer ray queries against scene geometry. The `crust-rt` crate is a
standalone kernel behind an Embree-shaped API: geometries attach to a
`SceneBuilder`, `commit()` builds the acceleration structure, and
`Scene::intersect` / `Scene::occluded` answer closest-hit and any-hit queries.
It knows nothing of materials, lights or USD. The reasoning and measurements
behind it are in `design.md`.

## Requirements

### Requirement: Embree-shaped geometry API

The kernel SHALL accept triangle meshes (with optional per-vertex shading
normals), analytic spheres, disks and open cylinders, round (linear) and cubic
curve segments, and
instances of committed scenes, each with a per-geometry visibility mask, and
SHALL report hits as plain `Copy` values carrying `geom_id`, `prim_id`, the hit
distance and barycentrics. A curve hit SHALL also carry its span parameter as
`u` and the curve's tangent at that parameter, carried into world space through
every instance level, including a motion-interpolated one. Every other hit SHALL
carry a zero tangent. An instanced hit SHALL report the instance's
top-level `geom_id` with the inner `prim_id`, unless the instance was attached
with an `InstanceHitId` label: `As(id)` SHALL report `id`, and `Offset(base)`
SHALL report `base` plus the id the inner scene reported, composing through
nesting.

#### Scenario: A ray hits an instanced mesh

- **WHEN** a ray hits a triangle of a mesh placed through an `Instance`
- **THEN** the hit carries the instance's `geom_id` and the triangle's index
  within the inner scene as `prim_id`

#### Scenario: A labelled instance forwards which part was hit

- **WHEN** a scene of parts labelled `As(0..n)` is placed through an instance
  attached with `Offset(base)`, and a ray hits part `k`
- **THEN** the hit carries `geom_id = base + k` and the part's inner `prim_id`

#### Scenario: A mask hides a geometry from a ray category

- **WHEN** a geometry's mask does not share a bit with the ray's mask
- **THEN** neither `intersect` nor `occluded` reports that geometry

#### Scenario: A curve hit reports where along the curve it is

- **WHEN** a ray hits a linear segment from (0, 0, 0) to (0, 2, 0) at height 0.5
- **THEN** the hit carries `u` = 0.25, and a tangent parallel to +y

#### Scenario: A bent cubic span reports its own tangent

- **WHEN** a ray hits a quarter-circle cubic Bézier span near its midpoint
- **THEN** the tangent is the Bézier derivative at the reported `u` (within
  the subdivision's flatness tolerance), not the direction of the chord

#### Scenario: An instanced curve's tangent is in world space

- **WHEN** a curve is placed through a rotating instance, an `Offset`-labelled
  instancer group, or a motion-blurred instance, and a ray hits it
- **THEN** the tangent is the object-space tangent mapped by that placement's
  linear transform at the ray's time

#### Scenario: Reporting curve data does not move a hit

- **WHEN** any sample scene is intersected
- **THEN** every hit's distance, normal, `geom_id` and `prim_id` are
  bit-identical to the kernel's before this change, under every SIMD codegen

### Requirement: Watertight triangle intersection

Triangle intersection SHALL be watertight (Woop et al. 2013): a ray crossing a
shared edge of two triangles SHALL hit at least one of them. The 4-wide packet
intersector and the scalar intersector SHALL return bit-identical results.

#### Scenario: A ray through a shared edge

- **WHEN** a ray passes exactly through the edge shared by two triangles
- **THEN** the query reports a hit (no pinhole)

### Requirement: Deterministic acceleration structure

`commit()` SHALL build the same acceleration structure for the same input on
every run and thread count, so a render's output does not depend on build
scheduling. The build SHALL read triangle vertices through the shared vertex table
rather than from a per-triangle copy, and SHALL hold at most 32 bytes per reference
and per binary node while it runs, merging each right subtree into its left
sibling's arrays rather than into a third allocation.

#### Scenario: Building twice

- **WHEN** the same geometry is committed twice
- **THEN** both builds produce identical node layouts

#### Scenario: The commit transient

- **WHEN** the subdivision stress scene is committed at level 3 with `--stats`
- **THEN** the commit phase's peak exceeds the traverse phase's resident memory by at
  most two thirds of what it did before this change (measured: 285 MiB against 524)

### Requirement: Triangles are stored once

A committed scene SHALL hold each triangle mesh's vertices and per-vertex shading
normals once, as shared tables of three `f32`s per entry, and each triangle as a
record of its geometry id, primitive id, three vertex indices and visibility mask.
The acceleration structure's triangle packets SHALL reference those tables and
records; no committed structure SHALL hold a second copy of a vertex except the
gathered packet layout below. A hit's shading normal SHALL be interpolated from the
per-vertex table by the record's indices, and a geometry without normals SHALL report
the geometric normal of its lane's vertices. The stored values SHALL be exactly the
attached `f32`s: nothing is quantized.

#### Scenario: A subdivided mesh's normals

- **WHEN** a mesh of `V` vertices and `T` triangles with per-vertex normals is committed
- **THEN** the footprint reports `12·V` bytes of vertex normals and `24·T` bytes of
  triangle records, and a hit interpolates the same normal, bit for bit, as
  interpolating the three attached normals by the hit barycentrics

#### Scenario: The hit triangle's vertices are available

- **WHEN** the application asks the scene for the vertices of a hit's
  `(geom_id, prim_id)`
- **THEN** it receives the three attached vertices of that triangle, in the scene's own
  (local, for an instanced scene) space

### Requirement: Two packet layouts, bit-identical

A tree's triangle packets SHALL be either gathered (each lane holds its vertices) or
indexed (each lane holds vertex indices and gathers from the shared table at test
time). The two layouts SHALL return bit-identical hits, tie-break lanes included, and
both SHALL remain bit-identical to the scalar intersector. The default (`auto`) SHALL
be the gathered layout: measured in and out of cache, the indexed layout is slower at
every tree size (1–8 % in cache, 30 % out of cache), so it is the explicit memory trade for a scene that
otherwise does not fit. The `CRUST_TRI_PACKETS` switch (`gathered` | `indexed` |
`auto`, default `auto`) SHALL select the layout on every tree, `gathered` being the
behaviour before this change.

#### Scenario: Forcing a layout

- **WHEN** the same scene is committed under `CRUST_TRI_PACKETS=gathered` and
  `CRUST_TRI_PACKETS=indexed` and rendered at 16 spp
- **THEN** the two images are bit-identical and the footprint reports all packets under
  the forced layout

#### Scenario: Indexed packets cost half of gathered ones

- **WHEN** the subdivision stress scene is committed at level 3 under `indexed`
- **THEN** each packet costs 92 bytes against the gathered layout's 192, every other
  footprint row is unchanged, and the kernel holds 78.6 bytes per triangle against
  95.5

### Requirement: Leaves are sized for packets

With `CRUST_BVH_PACKET_SAH` on (the default), the builder SHALL charge an all-triangle
range one intersection cost per packet of four rather than per triangle when deciding
whether to make it a leaf, so that leaves fill their packet lanes. With it off, the
builder SHALL use the per-primitive cost it used before this change. Either setting
SHALL build deterministically. The two settings MAY differ in which of two triangles
at exactly the same hit distance is reported, and in nothing else; the difference
between their renders SHALL fall as 1/√N with the sample count.

#### Scenario: Overlapping triangles

- **WHEN** six overlapping triangles that no split separates well are committed with the
  switch on
- **THEN** they form one leaf of two packets, the second with two inactive lanes; with
  it off they form more than one leaf

#### Scenario: Lane fill is reported

- **WHEN** a scene is rendered with `--stats`
- **THEN** the report gives the share of packet lanes that hold a triangle, and on the
  `scripts/gen_subdiv_stress.py` scene at level 3 it is at least 90 % with the switch on

### Requirement: Nested instancing and transform motion blur

Instances SHALL nest to arbitrary depth, composing transforms and masks at each
level. An instance MAY carry an end-of-shutter transform, in which case the
placement SHALL be interpolated per ray from the ray's time.

#### Scenario: A moving instance

- **WHEN** an instance authors an end-of-shutter transform
- **THEN** rays at different times see the geometry at the interpolated
  placement

### Requirement: Instances and cubic curve spans are stored inline

After `commit()`, the kernel SHALL store instances and cubic curve spans in arrays of
their own kind, inline, with no generic per-primitive slot pointing at them and no
per-primitive heap allocation except a moving instance's motion record.

- A static instance SHALL hold one cached transform, world-to-local.
- A moving instance SHALL additionally hold its two endpoint local-to-world transforms,
  in one heap-allocated motion record, so static instances do not pay for them.
- The kernel's memory report (`--stats` "kernel memory") SHALL list instances and
  cubic curve spans as their own lines.

Every query result SHALL be bit-identical to the boxed layout: hit distance,
barycentrics, ids, reported normal, and occlusion.

#### Scenario: Per-instance cost

- **WHEN** a scene of `n` static instances of one committed scene is committed
- **THEN** the instance line of the kernel memory report is at most 96 bytes per
  instance, and the scene's other primitive storage does not grow with `n`

#### Scenario: Per-span cost

- **WHEN** a scene of `n` cubic curve spans is committed
- **THEN** the cubic curve span line of the kernel memory report is at most 96 bytes
  per span

#### Scenario: The layout does not change the image

- **WHEN** the checked-in sample scenes are rendered at 16 spp before and after the
  layout change
- **THEN** every output EXR is bit-identical

### Requirement: A ray can pass out of curve tubes

A ray SHALL be able to ask the kernel to ignore every hit at which it leaves a
curve's tube (its direction pointing away from the outward normal), and every
surface inside the tube: the part of an end cap buried in the body, and a
joint's cap for a ray starting at that joint. It SHALL keep every other entry,
and every hit on other geometry, through every instance transform, for
`intersect` and `occluded` alike.

#### Scenario: A ray starting inside a tube leaves it unseen

- **WHEN** a ray that asks to pass out of curve tubes starts inside a round
  curve and points out of it
- **THEN** neither `intersect` nor `occluded` reports that curve

#### Scenario: Other tubes still stop the ray

- **WHEN** the same ray then meets a second curve from outside
- **THEN** it reports the second curve's entry hit

#### Scenario: Other rays are unchanged

- **WHEN** a ray does not ask to pass out of curve tubes
- **THEN** it reports exit hits exactly as before

#### Scenario: Crossing a strand near its end or a joint

- **WHEN** a ray that asks to pass out of curve tubes starts on a strand's
  surface within a radius of a segment's end, of a joint between segments, or
  of a joint between a cubic span's subdivision pieces, and crosses the strand
- **THEN** it does not stop on the end cap's half inside the body, nor on the
  joint's cap

#### Scenario: Joints stay closed to rays from elsewhere

- **WHEN** a ray that asks to pass out of curve tubes comes from elsewhere and
  meets a strand at a joint, including through the wedge outside a sharp bend
  that only the joint's cap covers
- **THEN** it reports the strand's entry hit

### Requirement: Known gaps

The kernel SHALL be documented as lacking: deformation motion blur, quaternion
(rotation-exact) motion, vector widths above 128 bits, and SIMD packets for
non-triangle primitives.

#### Scenario: A cubic curve is imported

- **WHEN** a cubic `BasisCurves` prim is imported
- **THEN** each span is kept as one cubic segment, and a query adaptively
  subdivides it into rounded cones only as deep as that ray's bounds tests
  require
