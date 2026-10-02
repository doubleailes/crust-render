## ADDED Requirements

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
