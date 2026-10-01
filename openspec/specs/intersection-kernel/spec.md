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
distance and barycentrics. An instanced hit SHALL report the instance's
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
scheduling.

#### Scenario: Building twice

- **WHEN** the same geometry is committed twice
- **THEN** both builds produce identical node layouts

### Requirement: Nested instancing and transform motion blur

Instances SHALL nest to arbitrary depth, composing transforms and masks at each
level. An instance MAY carry an end-of-shutter transform, in which case the
placement SHALL be interpolated per ray from the ray's time.

#### Scenario: A moving instance

- **WHEN** an instance authors an end-of-shutter transform
- **THEN** rays at different times see the geometry at the interpolated
  placement

### Requirement: Known gaps

The kernel SHALL be documented as lacking: deformation motion blur, quaternion
(rotation-exact) motion, vector widths above 128 bits, and SIMD packets for
non-triangle primitives.

#### Scenario: A cubic curve is imported

- **WHEN** a cubic `BasisCurves` prim is imported
- **THEN** each span is kept as one cubic segment, and a query adaptively
  subdivides it into rounded cones only as deep as that ray's bounds tests
  require

### Requirement: Resident geometry is stored once

After `commit()`, the kernel SHALL hold each piece of geometry data in exactly one
resident place:
- A triangle's vertex positions SHALL be held only in the SIMD packets that test it.
  The per-triangle record SHALL hold no positions.
- Per-vertex shading normals SHALL be held once per vertex of the mesh that authored
  them, and reached through a per-triangle index, not copied per triangle.
- Instances and cubic curve spans SHALL be stored inline in arrays of their own kind,
  with no per-primitive heap allocation and no generic per-primitive slot pointing at
  them.
- A static instance SHALL hold one cached transform, world-to-local. A moving instance
  SHALL additionally hold its two endpoint local-to-world transforms.

The kernel's memory report, `--stats` "kernel memory", SHALL break the total down by
these arrays. Every query result, including hit distance, barycentrics, ids and
reported normal, SHALL be bit-identical to the layout that stored them separately.

#### Scenario: Per-triangle cost of a committed mesh

- **WHEN** a triangle mesh with per-vertex normals is committed with no instances
- **THEN** the kernel memory reported, excluding BVH nodes, leaves, leaf indices and
  triangle packets, is at most 24 bytes per triangle plus 12 bytes per
  normal-carrying vertex

#### Scenario: Per-instance cost

- **WHEN** a scene of `n` static instances of one committed scene is committed
- **THEN** the instance payloads account for at most 96 bytes each in the kernel
  memory report, and no other per-instance primitive storage is reported

#### Scenario: The layout does not change the image

- **WHEN** the checked-in sample scenes are rendered at 16 spp before and after the
  layout change
- **THEN** every output EXR is bit-identical

#### Scenario: Tie-break on a shared edge still resolves

- **WHEN** a ray passes exactly through the edge shared by two triangles that sit in
  a SIMD packet
- **THEN** the scalar tie-break re-test, run on the packet's vertex values, reports a
  hit, and the result is bit-identical to the packet's own verdict where the packet
  gives one

