# Spec Delta

## ADDED Requirements

### Requirement: BasisCurves import as round curves shaded along the strand

A `UsdGeomBasisCurves` prim SHALL be imported as round curves of its authored
widths: `linear` as tapered segments, and `cubic` (`bezier`, `bspline` or
`catmullRom`) as cubic spans. A hit on a curve SHALL shade with the curve's
direction as its tangent, at every placement: top-level, instanced, placed by a
`PointInstancer`, and motion-blurred. Authored `normals` (ribbons) and `wrap`
are not yet read.

#### Scenario: A strand's tangent follows the curve

- **WHEN** a bent cubic `BasisCurves` strand is hit, and its material reads the
  tangent (a `chiang_hair_bsdf` with `curve_direction` unconnected)
- **THEN** the shading tangent at each hit is the strand's direction at that
  point, so the highlight runs across the strand, perpendicular to it, all the
  way along the bend

#### Scenario: Instanced fur keeps its direction

- **WHEN** a `PointInstancer` places a prototype clump of hair curves with
  differing rotations
- **THEN** each placement's highlight is oriented by that placement's own strand
  directions

#### Scenario: Curves without a hair material are unchanged

- **WHEN** `samples/curves.usda` is rendered at 16 spp
- **THEN** the image is bit-identical to the one rendered before this change

#### Scenario: Ribbon normals are not yet read

- **WHEN** a `BasisCurves` prim authors `normals`
- **THEN** it still renders as round tubes
