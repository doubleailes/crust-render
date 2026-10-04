# Spec Delta

## MODIFIED Requirements

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

## ADDED Requirements

### Requirement: A ray can pass out of curve tubes

A ray SHALL be able to ask the kernel to ignore every hit at which it leaves a
curve's tube, that is, where its direction points away from the tube's outward
normal. It SHALL keep every hit at which it enters a tube, and every hit on
another kind of geometry. The test SHALL hold through every instance transform,
mirrored ones included, for `intersect` and `occluded` alike.

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
