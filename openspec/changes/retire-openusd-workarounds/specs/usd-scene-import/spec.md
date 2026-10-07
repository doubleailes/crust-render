## ADDED Requirements

### Requirement: Transform stacks

The importer SHALL compose a prim's local transform from its `xformOpOrder` with
`UsdGeomXformable` semantics, in double precision, for every op kind
`UsdGeomXformOp` defines, including `!invert!` and `:suffix` instances. A leading
`!resetXformStack!` SHALL drop the inherited transform on any prim type. An op whose
kind is not a `UsdGeomXformOp` kind SHALL contribute identity, with a warning.

#### Scenario: Multi-op stack

- **WHEN** a prim authors `xformOpOrder = ["xformOp:translate", "xformOp:scale"]` with
  translate `(1, 2, 3)` and scale `(2, 3, 4)`
- **THEN** its local transform has translation `(1, 2, 3)` and the scale on its diagonal

#### Scenario: Pivot pair

- **WHEN** a prim authors `translate`, `translate:pivot`, `rotateXYZ`, `scale`,
  `!invert!xformOp:translate:pivot`
- **THEN** its local transform equals the one C++ USD computes for the same stack

#### Scenario: Single-axis op on a non-Xform prim

- **WHEN** a `BasisCurves`, `PointInstancer` or UsdLux light prim authors
  `xformOp:translateX = 2`
- **THEN** it is placed 2 units along its parent's X axis, not at its parent's origin

#### Scenario: Reset on any prim type

- **WHEN** a `DiskLight` under a translated `Xform` authors
  `xformOpOrder = ["!resetXformStack!", "xformOp:translate"]`
- **THEN** its world transform is its own translate alone

#### Scenario: Unknown op kind

- **WHEN** a prim lists `xformOp:bogus` in its `xformOpOrder`
- **THEN** that op contributes identity, the rest of the stack still composes, and a
  warning names the prim

#### Scenario: Reset after other ops (known gap)

- **WHEN** `!resetXformStack!` is listed after another op
- **THEN** the prim's local transform is identity, with a warning, where C++ USD would
  drop the ops before the reset and keep the ones after it. This gap closes when the
  importer moves to the openusd release after 0.7.0

#### Scenario: Ops on a prim that is not Xformable (known gap)

- **WHEN** an untyped prim or a `Scope` authors `xformOp:*` and an `xformOpOrder`
- **THEN** the ops apply to it and its descendants, where C++ USD ignores them on a
  prim that is not `Xformable`. This gap closes with the same openusd bump

### Requirement: Native instance nesting

A native instance found inside another native instance's prototype SHALL be imported:
its prototype's geometry SHALL appear in every placement of the outer prototype, under
the composed outer placement, inner instance transform and inner part transforms.
Nesting deeper than the importer's instance-nesting limit SHALL be refused with a
warning.

#### Scenario: Instance inside a prototype

- **WHEN** a prototype `_Outer` holds a sphere at its origin and an `instanceable` prim
  referencing `_Inner` translated by `(3, 0, 0)`, and `_Inner` holds a sphere at its
  origin, and one prim instances `_Outer`
- **THEN** both spheres render: one at the instance's origin and one 3 units along X

#### Scenario: Outer prototype placed several times

- **WHEN** the outer prototype is instanced by two prims at different translations
- **THEN** each placement carries both spheres, and the inner prototype's geometry is
  built once and shared

#### Scenario: Invisible inner instance

- **WHEN** the nested `instanceable` prim is `invisible`
- **THEN** none of its geometry renders, in any placement of the outer prototype
