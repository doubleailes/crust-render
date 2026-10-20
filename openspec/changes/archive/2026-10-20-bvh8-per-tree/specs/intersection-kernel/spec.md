# Spec Delta

## ADDED Requirements

### Requirement: Node width chosen per tree

A tree's wide nodes SHALL be either 4 wide (128 bytes) or 8 wide (256 bytes), chosen
per tree at commit by `CommitOptions::width` (`crust_rt::NodeWidth`: `Four`, `Eight`,
`Auto`). Both widths SHALL be compiled into the stable build, in safe code. The two
widths collapse the same binary tree, and SHALL return bit-identical hits (`t`, ids,
barycentrics, normals) and occlusion verdicts for every query except an exact tie: two
different primitives hit at the same `t` MAY resolve differently, since a tie goes to
the leaf visited first. `Auto` (the default) SHALL collapse a tree of at least 2^18
primitives 8 wide and a smaller one 4 wide; each instanced scene SHALL choose for itself.
The `CRUST_BVH_WIDTH` switch (`four` | `eight` | `auto`, default `auto`) SHALL select
the width on every tree, `four` being the behaviour before this change.

#### Scenario: Forcing a width on a scene without coplanar contact

- **WHEN** a scene with no two primitives meeting coplanar (such as
  `samples/cornellbox.usda`) is committed under `CRUST_BVH_WIDTH=four` and
  `CRUST_BVH_WIDTH=eight` and rendered at 16 spp
- **THEN** the two images are bit-identical

#### Scenario: A small scene keeps its 4-wide tree

- **WHEN** a scene whose every tree holds fewer than 2^18 primitives is committed
  under `auto`
- **THEN** every tree is collapsed 4 wide, exactly as before this change, and the
  render is bit-identical to `CRUST_BVH_WIDTH=four`

#### Scenario: An instance field splits its widths

- **WHEN** a scene of 2^18 or more instances of small prototypes is committed under
  `auto`
- **THEN** its top-level tree is 8 wide and every prototype's tree is 4 wide
