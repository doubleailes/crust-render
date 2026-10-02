## ADDED Requirements

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
