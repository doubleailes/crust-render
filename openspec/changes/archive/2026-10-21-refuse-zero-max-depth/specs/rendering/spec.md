# Spec Delta

## MODIFIED Requirements

### Requirement: Path-traced integration with Multiple Importance Sampling

The renderer SHALL estimate per-pixel radiance by path tracing camera rays
(an iterative two-pass walk: a forward pass recording one vertex per bounce,
then a backward gather), combining direct light sampling (NEE) and BSDF
sampling at each surface interaction via a selectable `SamplingStrategy`
(`crust:samplingStrategy` scene attribute / `--strategy` CLI override):
`power` (β=2 power heuristic, the default), `balance` (balance heuristic), and
the diagnostic single-strategy modes `light` (NEE only) and `bsdf` (BSDF
sampling only, no shadow rays). All four strategies are unbiased.

#### Scenario: Direct and indirect lighting are combined

- **WHEN** a camera ray hits a non-emissive surface from which a light is visible
- **THEN** the pixel radiance includes a direct-lighting term (from sampling the
  light and casting a shadow ray) and an indirect term (from a BSDF-sampled
  bounce), each weighted by the active sampling strategy over the light PDF and
  BSDF PDF

#### Scenario: Recursion is bounded by max depth

- **WHEN** a path has shaded `max_depth` vertices (`max_depth` counts vertices,
  not bounces: 1 shades the camera hit alone, with its visible emission and
  direct lighting)
- **THEN** the path terminates: its last bounce ray collects the emission of
  the surface it hits but no light at infinity, and nothing further

#### Scenario: Russian roulette terminates long paths probabilistically

- **WHEN** a path reaches its 4th vertex or beyond
- **THEN** survival is sampled from the path's throughput (with a minimum
  survival probability floor), and the throughput is divided by the survival
  probability on paths that continue

#### Scenario: Emissive surfaces contribute their emission

- **WHEN** a ray hits a surface whose material emits light
- **THEN** the surface's emitted radiance is added to the path contribution

