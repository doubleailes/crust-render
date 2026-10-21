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

#### Scenario: Passes composite with over

- **WHEN** a character pass holds out the props and a props pass holds out the
  character, both rendered from the same stage, camera and sample count
- **THEN** compositing the character pass over the props pass matches, within
  sampling noise, the render with no pass active

#### Scenario: Depth shows the visible surface

- **WHEN** a pixel straddles a holdout at depth 2 and a visible background at
  depth 10, and `depth` is closest-accumulated
- **THEN** the pixel's depth is 10

#### Scenario: A pixel covered only by a holdout

- **WHEN** every sample of a pixel ends at a holdout
- **THEN** its `depth`, `normal` and `P` hold their clear values, as for a pixel
  that sees nothing
