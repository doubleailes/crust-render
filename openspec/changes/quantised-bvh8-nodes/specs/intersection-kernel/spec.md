## ADDED Requirements

### Requirement: A tree's node format is chosen per tree

The kernel SHALL support two node formats for the committed acceleration structure: a
4-wide node with `f32` child bounds, and an 8-wide node whose child bounds are quantised
to 8 bits on a per-node power-of-two grid. Under the default setting the kernel SHALL
choose the format of each tree at `commit()` from the tree's primitive-reference count:
a tree at or above the kernel's size threshold SHALL use the 8-wide quantised node, and
any other tree SHALL use the 4-wide node. The choice SHALL be per tree, so an
instanced scene and the scene that places it MAY use different formats. The
`CRUST_BVH_NODES` switch (`auto` | `wide4` | `quant8`, default `auto`) SHALL force one
format on every tree, `wide4` being the format the kernel used before this change, and a
bad value SHALL warn once and mean `auto`. Both formats SHALL answer every query through
the same API.

#### Scenario: A small tree keeps the 4-wide node

- **WHEN** a scene below the size threshold is committed with `auto`
- **THEN** its tree uses the 4-wide node, and the footprint reports no quantised-node
  bytes

#### Scenario: A large tree uses the quantised node

- **WHEN** a scene at or above the size threshold is committed with `auto`
- **THEN** its tree uses the 8-wide quantised node

#### Scenario: A prototype and its placement choose separately

- **WHEN** a small prototype is instanced into a scene whose top level is above the
  threshold
- **THEN** the top level uses the quantised node and the prototype uses the 4-wide node,
  and a ray crossing both is answered as it would be under either single format

#### Scenario: The switch forces a format

- **WHEN** `CRUST_BVH_NODES=quant8` is set and a small scene is committed
- **THEN** its tree uses the quantised node
- **AND WHEN** `CRUST_BVH_NODES=wide4` is set and a large scene is committed
- **THEN** its tree uses the 4-wide node and every query is bit-identical to the kernel
  before this change

#### Scenario: A bad value

- **WHEN** `CRUST_BVH_NODES=wide` is set
- **THEN** one warning is logged and the kernel behaves as under `auto`

### Requirement: Quantised bounds are conservative and hits are unchanged

A quantised child box SHALL contain the exact box of the child it stands for, on every
axis: the grid rounds a child's minimum down and its maximum up. A quantised tree SHALL
return, for every ray, a closest hit at the same distance as the 4-wide tree over the
same primitives, with the same primitive, barycentrics and normal except where two
distinct primitives are hit at exactly the same distance, in which case either MAY be
reported. Every `occluded` query SHALL return the same answer as on the 4-wide tree.
Building the same input twice, on any number of threads, SHALL produce the same
quantised tree, node for node.

#### Scenario: Quantised boxes contain the exact ones

- **WHEN** a mesh of 6 400 triangles is committed with the quantised node
- **THEN** every lane's dequantised box contains the exact child box on all three axes,
  and every lane points at the same child as before conversion

#### Scenario: Random rays agree with the 4-wide tree

- **WHEN** random rays are cast at a triangle soup committed under each format, with
  `intersect` and with `occluded`
- **THEN** every closest-hit distance is equal, every occlusion answer is equal, and the
  primitive differs only where two primitives share that distance

#### Scenario: A render is unchanged

- **WHEN** the checked-in cornellbox and veach_mis samples and the generated stress
  scene are rendered at 16 spp with every tree forced to `quant8` and with every tree
  forced to `wide4`
- **THEN** `crust diff` reports 0 differing pixels for each scene

#### Scenario: Committing twice at different thread counts

- **WHEN** the same scene is committed with `RAYON_NUM_THREADS=1` and with `8`
- **THEN** the two quantised trees are identical, node for node

#### Scenario: A degenerate extent

- **WHEN** a node's children all lie in a plane, or a child's box has zero extent on an
  axis
- **THEN** its grid has a valid exponent, the boxes still contain the exact ones, and a
  ray in that plane is neither lost nor turned into NaN

### Requirement: The footprint reports node bytes per format

`MemoryFootprint` SHALL count node bytes separately for each format. The sum of the
node rows SHALL be part of the kernel-memory total, and a scene with no tree of a
format SHALL report zero bytes for it.

#### Scenario: A quantised tree is smaller

- **WHEN** the same large mesh is committed forced to `wide4` and forced to `quant8`
- **THEN** the `quant8` footprint's node bytes are lower, and the total kernel memory is
  lower by the difference

#### Scenario: Mixed formats in one scene

- **WHEN** a scene commits one tree of each format
- **THEN** the footprint reports a non-zero byte count on both node rows, and their sum
  is inside the total
