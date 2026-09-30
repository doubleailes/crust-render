# Spec Delta

## ADDED Requirements

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
both SHALL remain bit-identical to the scalar intersector. `commit()` SHALL choose
indexed packets for trees above a triangle-count threshold; the `CRUST_TRI_PACKETS`
switch (`gathered` | `indexed` | `auto`, default `auto`) SHALL force either layout on
every tree, `gathered` being the behaviour before this change.

#### Scenario: Forcing a layout

- **WHEN** the same scene is committed under `CRUST_TRI_PACKETS=gathered` and
  `CRUST_TRI_PACKETS=indexed` and rendered at 16 spp
- **THEN** the two images are bit-identical and the footprint reports all packets under
  the forced layout

#### Scenario: Indexed packets cost half of gathered ones

- **WHEN** a tree is committed under `indexed`
- **THEN** its packet bytes are half those of the same tree under `gathered`, and
  every other footprint row is unchanged

### Requirement: Leaves are sized for packets

With `CRUST_BVH_PACKET_SAH` on (the default), the builder SHALL charge an all-triangle
range one intersection cost per packet of four rather than per triangle when deciding
whether to make it a leaf, so that leaves fill their packet lanes. With it off, the
builder SHALL use the per-primitive cost it used before this change. Either setting
SHALL build deterministically. The two settings MAY differ in which of two triangles
at exactly the same hit distance is reported, and in nothing else; the difference
between their renders SHALL fall as 1/√N with the sample count.

#### Scenario: Eight coplanar triangles

- **WHEN** eight triangles that no split separates well are committed with the switch on
- **THEN** they form one leaf of two full packets; with it off they form more than one leaf

#### Scenario: Lane fill is reported

- **WHEN** a scene is rendered with `--stats`
- **THEN** the report gives the share of packet lanes that hold a triangle, and on the
  `scripts/gen_subdiv_stress.py` scene at level 3 it is at least 90 % with the switch on

## MODIFIED Requirements

### Requirement: Deterministic acceleration structure

`commit()` SHALL build the same acceleration structure for the same input on
every run and thread count, so a render's output does not depend on build
scheduling. The build SHALL read triangle vertices through the shared vertex table
rather than from a per-triangle copy, and SHALL hold at most 32 bytes per reference
and per binary node while it runs, writing subtrees into one pre-sized arena.

#### Scenario: Building twice

- **WHEN** the same geometry is committed twice
- **THEN** both builds produce identical node layouts

#### Scenario: The commit transient

- **WHEN** the subdivision stress scene is committed at level 3 with `--stats`
- **THEN** the commit phase's peak exceeds the traverse phase's resident memory by at
  most two thirds of what it did before this change (260 MiB against 390 MiB)
