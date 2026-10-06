## ADDED Requirements

### Requirement: Analytic spheres and cylinders report accurate hit distances

A ray's hit on an analytic sphere, or on an analytic cylinder's wall, SHALL report its
distance with an error below `1e-6 · t` (t the reported distance) for any radius of at
least `1e-4 · t`, in single precision. A ray restarted just short of such a hit, as the
pass-through walks restart it, SHALL NOT find that same hit again.

#### Scenario: A small sphere seen from far away

- **WHEN** rays aimed across a sphere of radius 0.05 from 8 units away are intersected,
  and their reported distances compared with a double-precision reference
- **THEN** every distance is within `1e-6 · t` of the reference

#### Scenario: A small cylinder seen from far away

- **WHEN** rays aimed across the wall of a cylinder of radius 0.05 from 8 units away are
  intersected, and their reported distances compared with a double-precision reference
- **THEN** every distance is within `1e-6 · t` of the reference

#### Scenario: No re-hit after a restart

- **WHEN** each such ray is restarted at `t − 0.001 + 1e-5 · t` and intersected again
  with the tracer's minimum distance
- **THEN** the next hit is the sphere's far side, never its near side again
