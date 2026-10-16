# Spec Delta

## ADDED Requirements

### Requirement: Curves read the crust:mass attributes

`BasisCurves` and `NurbsCurves` prims SHALL read `float crust:mass:radius`
(world units; a positive value opts the prim in), `float
crust:mass:threshold` (default 0.5), `token crust:mass:group` and `int
crust:mass:maxResolution` (default 512). A value outside its domain SHALL be
refused with one `mass.invalid_setting` warning: a bad radius bakes no field,
a bad threshold or cap takes its default.

#### Scenario: A negative radius is refused

- **WHEN** a curves prim authors `crust:mass:radius = -0.1`
- **THEN** no field is baked for it, and one `mass.invalid_setting` warning
  names the prim and the attribute

#### Scenario: A zero cap takes the default

- **WHEN** an opted-in prim authors `crust:mass:maxResolution = 0`
- **THEN** its field bakes with the default cap of 512, and one
  `mass.invalid_setting` warning names it

### Requirement: Mass attributes on other prims are not read

A `crust:mass:*` attribute authored on a prim that is not `BasisCurves` or
`NurbsCurves` SHALL bake nothing, and SHALL be reported with one
`mass.not_curves` warning per prim.

#### Scenario: A mesh with a radius

- **WHEN** a `Mesh` authors `crust:mass:radius = 0.05`
- **THEN** it renders as before, and one `mass.not_curves` warning names it

### Requirement: A material's mass colour terminal is read

A `Material` whose `outputs:crust:mass:color` terminal connects to a MaterialX
shader node's output SHALL provide that node's graph as the mass colour graph
of every opted-in curve it is bound to. A terminal connected to anything else
SHALL be ignored with one `mass.color_unsupported` warning. The terminal does
not change how the material shades.

#### Scenario: A colour graph beside the surface

- **WHEN** a hair material authors `outputs:mtlx:surface` and an
  `outputs:crust:mass:color` connected to an `ND_mix_color3` driven by
  `curve:v`, and is bound to an opted-in groom
- **THEN** the groom's field holds that ramp's colour, and the surface shades
  as it does without the terminal

#### Scenario: A terminal on a mesh's material

- **WHEN** a material with an `outputs:crust:mass:color` terminal is bound only
  to meshes
- **THEN** nothing is baked, and the meshes render bit-identically to the
  material without the terminal
