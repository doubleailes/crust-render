# Spec Delta

## ADDED Requirements

### Requirement: A subsurface walk ends without bias

A subsurface random walk SHALL end in exactly one of four ways: at an exit through
its own object's surface, which continues the path weighted by the walk's
throughput; as absorbed, when its throughput is below 1e-6 in every channel; by
roulette, once its throughput is below 0.05 in every channel, where the walk
survives with probability `peak / 0.05` (never below 0.05) and its throughput is
divided by that probability on survival; or at 256 steps, as absorbed. The
roulette SHALL NOT change the expected radiance of any pixel: a render with it is
an unbiased estimate of the same image as a render without it, and its noise at a
given sample count SHALL be the same within measurement error. The 256-step cap
is a known bias: on an object many mean free paths thick a near-white medium loses
several percent of its energy to it (measured 7.5% at α → 1 and 4% in the red
channel of a skin-like medium on a semi-infinite slab; `docs/subsurface_walk.md`),
and this change does not remove it.

#### Scenario: The roulette costs no noise

- **WHEN** `samples/materialx_subsurface.usda` is rendered at 16 and at 32 samples
  per pixel and compared against a 2048-sample reference of the same scene with
  `exr_diff`
- **THEN** the relative MSE at each sample count equals the walk's without roulette
  to three significant digits, and halves from 16 to 32 samples

#### Scenario: A slab still reflects its colour

- **WHEN** 8192 walks with a cosine-weighted entry enter a semi-infinite slab of
  colour (0.8, 0.5, 0.2), radius 0.1 and zero anisotropy
- **THEN** the mean walk weight, absorbed walks counted as zero, is within 0.05 of
  the colour in every channel

#### Scenario: A dim chromatic walk stops early

- **WHEN** a walk's throughput has fallen below 0.05 in every channel and it has
  not exited
- **THEN** it either ends there or continues with its throughput divided by its
  survival probability, so that a scene's `--stats` mean steps per walk falls for
  a medium whose channels decay at different rates (skin) and is unchanged for a
  grey one (marble)
