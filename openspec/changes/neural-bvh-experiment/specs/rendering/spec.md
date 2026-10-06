## ADDED Requirements

### Requirement: Neural shadow-ray occlusion switch

*Phase 2: implemented only if the Phase 1 measurement gate passes; otherwise this
requirement is withdrawn from the change.* `CRUST_NEURAL_OCCLUSION` (`off` | `on`,
default `off`) SHALL select whether the importer requests occlusion proxies for
instanced prototypes. With `off`, every render SHALL be bit-identical to the renderer
before this change. The switch SHALL be parsed once into `Config`. A bad value SHALL
warn once and fall back to `off`.

#### Scenario: Off is the old renderer

- **WHEN** the checked-in samples are rendered at 16 spp with `CRUST_NEURAL_OCCLUSION`
  unset and with it set to `off`, on a build with the `neural-bvh` feature
- **THEN** every output EXR is bit-identical to the goldens recorded before this change

#### Scenario: Switch set on a build without the feature

- **WHEN** `CRUST_NEURAL_OCCLUSION=on` is set and the binary was built without
  `neural-bvh`
- **THEN** one warning says the switch has no effect in this build, and the render is
  exact

#### Scenario: A bad value

- **WHEN** `CRUST_NEURAL_OCCLUSION=fast` is set
- **THEN** one warning names the variable and the render proceeds as `off`

### Requirement: Only shadow rays use proxies

*Phase 2.* With the switch on, a proxy SHALL be requested only for an instanced
prototype that holds at least `CRUST_NEURAL_MIN_TRIS` triangles (default 250 000) and
has no cutout material bound. Only shadow-ray visibility SHALL consult it: NEE and the
learned light cache's training. Camera rays, BSDF bounces and cutout walks SHALL stay
exact.

#### Scenario: Camera rays stay exact

- **WHEN** a scene with a proxied prototype is rendered with the switch on, and its
  camera rays are compared with an exact render
- **THEN** every primary hit (position, normal, material and ids) is identical

#### Scenario: A cutout prototype

- **WHEN** an instanced prototype above the threshold has a cutout material bound
- **THEN** no proxy is requested for it, and its shadow rays are exact

### Requirement: Neural occlusion is a documented bias

*Phase 2.* A render with the switch on SHALL be treated as a biased approximation of
the exact render. NEE sees learned visibility while BSDF-sampled emission stays exact,
so the error does not vanish with more samples. The render SHALL log one INFO line
saying the proxies are in use and how many were built.

#### Scenario: The error plateaus

- **WHEN** a proxied scene is rendered with `--indirect-clamp 0` at 16, 64 and 256 spp,
  with the switch on and off
- **THEN** the relmse between the two does not fall as 1/√N, and the plateau value is
  recorded in the design record

#### Scenario: One line says the render is approximate

- **WHEN** a render with the switch on builds at least one proxy
- **THEN** exactly one INFO line reports the number of proxies and that shadow-ray
  visibility is approximate
