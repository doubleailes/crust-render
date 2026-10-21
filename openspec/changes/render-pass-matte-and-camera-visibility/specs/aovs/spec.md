# Spec Delta

## ADDED Requirements

### Requirement: Holdout samples are empty in every AOV

A sample whose camera ray ended at a holdout SHALL contribute to every RenderVar
what an escaped camera ray that met no light contributes: 0 to the beauty, alpha
and every light path expression source, and the clear value to every data source
(depth, distance, P, normal, st, motion vector, primvars, albedo, diffuse
filter). It SHALL still count toward the pixel's sample count and filter weight.

#### Scenario: Premultiplied coverage at a holdout edge

- **WHEN** a pixel is half covered by a holdout in front of a white emissive wall
  of radiance 1, rendered as `color4f`
- **THEN** both the beauty and its alpha converge to 0.5, so the pixel is a
  correctly premultiplied half-covered white

#### Scenario: Passes that hold each other out add up

- **WHEN** a character pass holds out the props, and a props pass holds out the
  character and the dome, both rendered from the same stage and camera at `-s 16`
- **THEN** the sum of the two passes' beauty, alpha and LPE channels matches the
  render with no pass active, to float rounding, edge pixels included

#### Scenario: Geometry visible in both passes is counted twice

- **WHEN** the same two passes are rendered but neither holds out the dome
- **THEN** their sum shows the dome's colour twice where the camera sees it, so
  passes recombine by addition only when each camera-visible object is visible
  in exactly one of them

#### Scenario: Depth shows the visible surface

- **WHEN** a pixel straddles a holdout at depth 2 and a visible background at
  depth 10, and `depth` is closest-accumulated
- **THEN** the pixel's depth is 10

#### Scenario: A pixel covered only by a holdout

- **WHEN** every sample of a pixel ends at a holdout
- **THEN** its `depth`, `normal` and `P` hold their clear values, as for a pixel
  that sees nothing
