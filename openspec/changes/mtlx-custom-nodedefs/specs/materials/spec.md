## ADDED Requirements

### Requirement: The MaterialX UsdPreviewSurface is its nodegraph

A MaterialX `UsdPreviewSurface` node (`ND_UsdPreviewSurface_surfaceshader`, inline or in a `.mtlx`) SHALL render as MaterialX 1.39's `IMP_UsdPreviewSurface_surfaceshader`: the same leaves and combinators, with each input taking its connection, then its authored value, then the vendored nodedef's default. `occlusion` and `displacement` SHALL be reported when authored away from their defaults. At an `opacityThreshold` that folds to 0 in `opacityMode` 0, the surface SHALL NOT be a cutout.

#### Scenario: The defaults

- **WHEN** a `UsdPreviewSurface` authors only `diffuseColor`
- **THEN** it has two leaves: an Oren–Nayar diffuse of that colour, and a
  generalized-Schlick specular with F0 0.04, F90 1 and α 0.25

#### Scenario: Connected opacity refracts

- **WHEN** its `opacity` is connected and `opacityThreshold` is 0
- **THEN** the surface transmits where the opacity is below 1, and is not a cutout

#### Scenario: A threshold cuts out

- **WHEN** `opacityThreshold` is 0.5
- **THEN** the surface is absent where the opacity is below 0.5, and present elsewhere

### Requirement: Channel, compositing, switch and place2d nodes

`separate2`, `separate3`, `separate4`, `switch`, `over`, `in`, `out`, `difference`, `premult` and `place2d` SHALL compile and evaluate as MaterialX 1.39's standard library defines them: `switch` as `NG_switch_*`'s `ifgreater` chain, `place2d` as `NG_place2d_vector2`, and the compositing nodes as their genosl expressions.

#### Scenario: A layered texture's blend mode

- **WHEN** a `switch` of `color4` inputs has a literal `which` of 1
- **THEN** it evaluates to its `in2`, and none of its other inputs is compiled

#### Scenario: A which past the last input

- **WHEN** a `switch`'s `which` is 10 or more
- **THEN** it evaluates to zero, as the stdlib graph does, not to `in10`

### Requirement: transformmatrix by a constant matrix

`transformmatrix` SHALL compute as MaterialX's GLSL does: a row vector times the authored row-major matrix, a `vector3` by a `matrix44` (and a `vector2` by a `matrix33`) homogeneous, so the last row translates. Its matrix SHALL be a compile-time constant; any other is reported and the input passes through. A `position` whose `space` is not `world` SHALL be reported.

#### Scenario: A camera projection

- **WHEN** a `transformmatrix` of type `vector3` multiplies a point by a
  constant `matrix44` whose last row is a translation
- **THEN** the result is the point, as a row vector `(x, y, z, 1)`, times the
  matrix, translation included

#### Scenario: A varying matrix

- **WHEN** a `transformmatrix`'s `mat` is connected to anything but a constant
- **THEN** the material reports it, and the node passes its `in` through

### Requirement: Custom nodedefs from the MaterialX plugin search path

A node whose category is defined by a `<nodedef>` with a `<nodegraph nodedef=…>` implementation, in the document or in a `.mtlx` file under `PXR_MTLX_PLUGIN_SEARCH_PATHS`, SHALL render as that graph, with its interface inputs bound to the instance's inputs or to the nodedef's defaults. A node in the graph that crust does not compile SHALL be reported by name.

#### Scenario: A custom node renders

- **WHEN** `PXR_MTLX_PLUGIN_SEARCH_PATHS` names a directory whose `.mtlx`
  defines a node as a `<nodegraph>` over standard nodes, and a material uses it
- **THEN** it renders as that graph, with no `mtlx.*` warning
