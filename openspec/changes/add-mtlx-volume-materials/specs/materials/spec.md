## ADDED Requirements

### Requirement: A MaterialX volume terminal is the medium inside the bound geometry

A material's volume terminal — a MaterialX `volumematerial`, a `volume` shader,
a `mix` of two volume shaders, or a VDF network of `anisotropic_vdf`,
`absorption_vdf`, `mix`, `add` and `multiply` — SHALL describe a homogeneous
medium with absorption and scattering coefficients per unit length and a
Henyey–Greenstein anisotropy. `mix` and `add` SHALL combine the coefficients
linearly and the anisotropy weighted by each branch's scattering; `multiply`
SHALL scale both coefficients. A `volume` with no `vdf` SHALL describe vacuum.
A `volume`'s `edf` SHALL be reported and not rendered.

When the material also has a surface, the volume SHALL be the interior a ray
refracting into a thick, transmitting surface carries, replacing the interior
the surface's own transmission inputs describe. A thin-walled or
non-transmitting surface SHALL ignore it.

When the material has no surface, it SHALL be a medium boundary: its surface
scatters, emits and occludes nothing, and the medium is entered and left by
crossing it.

#### Scenario: A volume replaces the surface's transmission interior

- **WHEN** an `open_pbr_surface` with `transmission_weight = 1`,
  `transmission_color = (0.2, 0.2, 0.2)` and `transmission_depth = 1` is paired
  with a volume of absorption 0.5 and scattering 2
- **THEN** a ray refracted into it carries σₐ = 0.5 and σₛ = 2

#### Scenario: Mixed VDFs weight the anisotropy by scattering

- **WHEN** a `mix` blends an `anisotropic_vdf` (scattering 1, anisotropy 0.6)
  with an `absorption_vdf` at 0.25 toward the scattering one
- **THEN** the medium's coefficients are the linear blend and its anisotropy is
  0.6

#### Scenario: A volume-only material is a medium boundary

- **WHEN** a material has a volume terminal and no surface terminal
- **THEN** the geometry it is bound to reports itself a medium boundary with the
  volume's coefficients, and has no emission

### Requirement: Inline MaterialX networks are read as MaterialX documents

A `Shader` whose `info:id` names a MaterialX nodedef (`ND_…`) SHALL be read as
the MaterialX node that nodedef defines, its inputs' values meaning what the
same literal means in a `.mtlx`, and its connections followed through
`NodeGraph` outputs and `Material` / `NodeGraph` interface inputs. A material
whose surface is such a network SHALL render as the same graph in a `.mtlx`
does. A decodable universal or `glslfx` surface SHALL take precedence over an
`mtlx` one.

#### Scenario: An inline OpenPBR surface is a MaterialX material

- **WHEN** a material's `outputs:mtlx:surface` connects to an
  `ND_open_pbr_surface_surfaceshader`
- **THEN** the material is a MaterialX material, not the default grey

#### Scenario: A preview surface keeps winning

- **WHEN** a material authors both a universal `UsdPreviewSurface` and an
  `mtlx` surface
- **THEN** it renders through the `UsdPreviewSurface`
