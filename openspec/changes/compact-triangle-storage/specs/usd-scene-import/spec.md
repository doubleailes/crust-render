# Spec Delta

## ADDED Requirements

### Requirement: Per-corner mesh tables are derived, not stored

The importer SHALL NOT keep a per-triangle copy of anything a hit can recompute from
the kernel's shared vertices and the triangle's corner texture coordinates. Tangents
and texture-footprint densities SHALL be computed at the hit, with the formulas the
stored tables used, and SHALL match those tables bit for bit. A face-varying chart
SHALL be kept as its values plus one index per triangle corner. A subdivided mesh's
Ptex sub-faces SHALL be kept as one 8-byte patch parameter per refined face (base
cage face, dyadic origin, depth, rotation), from which a triangle's corner
coordinates in its cage face are reconstructed exactly, so the synthetic Ptex
face-varying channel and the second refinement pass for a `none` chart are no longer
needed. Importer-side positions and normals SHALL be stored as three `f32`s, not
padded to four.

#### Scenario: A refined Ptex mesh keeps its face ids

- **WHEN** `samples/ptex_quads.usda` is rendered at `--subdiv-level 2` before and after
  this change
- **THEN** the images are bit-identical, and the importer's face table costs 8 bytes per
  refined face plus the face id and fan slice per triangle, rather than 24 bytes of
  corner coordinates per triangle on top of those

#### Scenario: A textured subdivided mesh's tables

- **WHEN** a UV-textured `catmullClark` mesh is refined to level 2 and rendered
- **THEN** the image is bit-identical to the stored-table render, and the traverse
  phase's resident memory per refined triangle is at most 20 bytes above the same mesh
  without a chart

### Requirement: Instanced meshes have tangents

A mesh placed more than once SHALL shade normal maps with a tangent frame, computed
at the hit from the prototype's vertices and the placement's transform, exactly as a
baked mesh does.

#### Scenario: A normal map on an instanced mesh

- **WHEN** a normal-mapped material is bound to geometry placed more than once
- **THEN** each placement shades with a perturbed normal, and a placement whose
  transform is the identity matches the baked render of the same mesh bit for bit
