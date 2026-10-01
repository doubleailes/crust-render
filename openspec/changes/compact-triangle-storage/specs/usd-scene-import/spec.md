# Spec Delta

## ADDED Requirements

### Requirement: Per-corner mesh tables are derived, not stored

The importer SHALL NOT keep a per-triangle copy of anything a hit can recompute from
the kernel's shared vertices and the triangle's corner texture coordinates. Tangents
SHALL be computed at the hit from the kernel's vertices, with the formula the stored
table used, and for an unmirrored placement SHALL match that table bit for bit (a
mirrored baked placement's stored tangent paired swapped vertices with unswapped
corners; the derived one pairs them correctly). Texture-footprint densities stay a
4-byte table per triangle, because they are defined in the mesh's local frame, which
a baked mesh no longer has. A face-varying chart SHALL be kept as its values plus one
index per triangle corner. A subdivided mesh's Ptex sub-faces SHALL be kept as one
8-byte dyadic cell per triangle (origin, depth and the corner at the origin, in the
base cage face) from which the triangle's corner coordinates are reconstructed
exactly. Importer-side positions and normals SHALL be stored as three `f32`s, not
padded to four.

#### Scenario: A refined Ptex mesh keeps its face ids

- **WHEN** `samples/ptex_quads.usda` is rendered at `--subdiv-level 2` before and after
  this change
- **THEN** the images are bit-identical, and the importer's face table costs 8 bytes of
  cell per triangle beside the face id, fan slice and density, rather than 24 bytes of
  corner coordinates

#### Scenario: A textured subdivided mesh's tables

- **WHEN** a UV-textured `catmullClark` mesh is refined to level 2 and rendered
- **THEN** the image is bit-identical to the stored-table render, and the traverse
  phase's resident memory per refined triangle is at most 20 bytes above the same mesh
  without a chart

### Requirement: Directly instanced meshes have tangents

A mesh placed more than once through direct, static instances SHALL shade normal
maps with a tangent frame, computed at the hit from the prototype's vertices and the
placement's transform, exactly as a baked mesh does. A prototype part placed through
an instancer's group (a `PointInstancer` or native-instancing prototype), whose hit
id is forwarded rather than its own, and a motion-blurred instance still shade with
the geometric normal; those are the remaining gap.

#### Scenario: A normal map on a directly instanced mesh

- **WHEN** a normal-mapped material is bound to a mesh prim authored twice
- **THEN** each placement shades with a perturbed normal, and a placement whose
  transform is the identity matches the baked render of the same mesh bit for bit
