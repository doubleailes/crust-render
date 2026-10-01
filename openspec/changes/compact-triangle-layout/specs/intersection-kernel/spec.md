## ADDED Requirements

### Requirement: Selectable compact triangle layout

A committed scene SHALL store its triangles in one of two layouts, chosen when it is
committed:

- **packed** (the default): each triangle reference carries its own vertex positions
  in SIMD packets.
- **compact**: each mesh's vertex positions are held once. Leaves reference
  per-triangle records that hold mesh-local vertex indices, and no triangle packets
  are stored.

The compact layout SHALL report the same query results as the packed layout, bit for
bit: hit distance, barycentrics, ids, reported normal and occlusion. That includes the
watertight tie-break on shared edges. The layout SHALL be reported by the kernel's
memory report, and in the compact layout the report SHALL list vertex positions and no
triangle packets.

#### Scenario: Both layouts answer identically

- **WHEN** the same geometry is committed once packed and once compact, and the same
  rays are cast against both
- **THEN** every closest-hit and occlusion result is bit-identical

#### Scenario: Compact per-triangle cost

- **WHEN** a triangle mesh with per-vertex normals is committed in the compact layout
  with no instances
- **THEN** the kernel memory reported, excluding BVH nodes, leaves and leaf
  references, is at most 24 bytes per triangle plus 24 bytes per vertex (position and
  normal), and no triangle packets are reported

#### Scenario: Layout does not change the image

- **WHEN** a checked-in sample scene is rendered at 16 spp with
  `--geometry-layout packed` and with `--geometry-layout compact`
- **THEN** the two output EXRs are bit-identical
