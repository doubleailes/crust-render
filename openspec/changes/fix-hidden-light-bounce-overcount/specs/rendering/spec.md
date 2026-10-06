## ADDED Requirements

### Requirement: A pass-through crosses each surface once

A path segment or shadow ray that passes hidden light sources, cutouts or thin walls
SHALL count each surface it crosses exactly once. A hit on the same primitive, from the
same side, within `1e-4 · t` of a crossing of it is a numerical re-hit and SHALL NOT add
emission, opacity or transmittance a second time. Two distinct surfaces, however close,
SHALL both be crossed.

#### Scenario: A small hidden light far away

- **WHEN** a diffuse plane under one camera-invisible sphere light, at a
  distance-to-radius ratio of 40, 160 or 200, is rendered with BSDF sampling alone and
  with light sampling alone
- **THEN** the two estimates of the plane agree within noise that falls as 1/√N, and no
  pixel's BSDF-only value is twice that of the same light made solid

#### Scenario: Two cards close together

- **WHEN** two half-opaque cutout cards on different primitives stand 0.0005 apart
  between a floor and a light
- **THEN** shadow rays are attenuated by both cards, as `(1 − 0.5)²`
