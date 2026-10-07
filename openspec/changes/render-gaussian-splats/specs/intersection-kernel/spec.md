# Spec Delta

## MODIFIED Requirements

### Requirement: Embree-shaped geometry API

The kernel SHALL accept triangle meshes (with optional per-vertex shading
normals), analytic spheres, disks and open cylinders, round (linear) and cubic
curve segments, Gaussian-ellipsoid particles, and
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

#### Scenario: A particle hit names its particle

- **WHEN** a ray hits particle `k` of a particle geometry
- **THEN** the hit carries that geometry's `geom_id` and `k` as `prim_id`

### Requirement: Known gaps

The kernel SHALL be documented as lacking: deformation motion blur, quaternion
(rotation-exact) motion, vector widths above 128 bits, SIMD packets for
non-triangle primitives (curves, analytic shapes and particles), and a query
returning the k nearest hits in one traversal.

#### Scenario: A cubic curve is imported

- **WHEN** a cubic `BasisCurves` prim is imported
- **THEN** each span is kept as one cubic segment, and a query adaptively
  subdivides it into rounded cones only as deep as that ray's bounds tests
  require

#### Scenario: Particles are intersected one at a time

- **WHEN** a leaf holds several particles
- **THEN** the kernel tests them with scalar code, one particle per test

## ADDED Requirements

### Requirement: Gaussian-ellipsoid particles hit at their peak

A particle SHALL be given as a centre, an orientation and three positive scales,
bounded by its 3σ ellipsoid. A ray SHALL hit it only where its closest approach in
the particle's normalised frame lies within distance 3, at that closest approach
`t*`, and the hit SHALL carry the squared normalised distance `d²` there as `u`. The
kernel SHALL hold no opacity or colour, and nearest-hit SHALL order particles by `t*`.

#### Scenario: Closest approach is the hit

- **WHEN** a ray along +X at height 0.5 crosses a unit-scale particle at the origin
- **THEN** it hits at the ray parameter of x = 0, with `u` = 0.25

#### Scenario: Ordering by peak, not by entry

- **WHEN** a long thin particle and a small round one overlap so that the ray enters
  the long one's support first but reaches the round one's peak first
- **THEN** `intersect` reports the round particle

#### Scenario: Rotated, anisotropic particle

- **WHEN** a particle of scales (2, 0.5, 0.5), rotated 90° about Z, is hit by a ray
  along +X passing through (0, 1.5, 0)
- **THEN** the hit's `u` is 0.5625 (1.5 / 2, squared), matching a dense numerical
  search for the peak to within 1e-5 relative

#### Scenario: Deterministic build with particles

- **WHEN** a scene of 100,000 particles is committed twice, with 1 and with 16 threads
- **THEN** both builds answer every ray of a fixed set identically
