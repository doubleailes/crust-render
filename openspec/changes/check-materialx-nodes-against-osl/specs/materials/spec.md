## ADDED Requirements

### Requirement: MaterialX pattern nodes evaluate as the MaterialX reference

Every MaterialX standard-library pattern node signature that `crust-mtlx`
compiles, over the `float`, `vector2`, `vector3`, `vector4`, `color3` and
`color4` value types, SHALL evaluate to the value MaterialX's reference
implementation (its `genosl` code generator, run by OSL) produces for the same
node and inputs, to within 1e-5 relative in every lane and at exactly the
reference's width, including when inputs are left unauthored. The only
exceptions SHALL be these guards, which keep a value finite or physical where
the reference does not:

- `divide` by a zero lane gives 0, and `remap` over an empty input range
  (`inlow == inhigh`) gives `outlow`, instead of ±inf or NaN;
- `modulo` by a zero lane returns the dividend, for every value type, and a
  `modulo` whose quotient overflows gives the exact floored remainder instead
  of ±inf;
- `normalmap` raises the decoded tangent-space z to at least 1e-4;
- `artistic_ior` clamps `edge_color` to [0, 1];
- an unauthored `convert` input is a zero `float`, whatever the signature.

Each exception SHALL be tested as a rule naming the input condition under
which it applies, so it cannot excuse a difference under any other input.

#### Scenario: An unauthored multiply input takes the nodedef default

- **WHEN** a `multiply` node of type `float` authors no inputs
- **THEN** it evaluates to 0 (`in1` defaults to 0 and `in2` to 1)

#### Scenario: modulo floors

- **WHEN** a `modulo` node computes `-0.2` modulo `1`
- **THEN** it evaluates to 0.8, not -0.2

#### Scenario: modulo by a subnormal divisor stays finite

- **WHEN** a `modulo` node computes `1` modulo `1e-40`
- **THEN** it evaluates to a finite value between 0 and `1e-40`

#### Scenario: Unauthored inputs take the node's width

- **WHEN** an `add` node of type `color3` authors no inputs
- **THEN** it evaluates to a three-lane zero, not a `float`

#### Scenario: sign of zero

- **WHEN** a `sign` node's input is 0
- **THEN** it evaluates to 0

#### Scenario: A division by zero stays finite

- **WHEN** a `divide` node's `in2` is 0 in some lane
- **THEN** that lane evaluates to 0, and every other lane to the reference's
  value

#### Scenario: The reference cases pass

- **WHEN** `cargo test -p crust-mtlx --test osl_oracle` runs over the
  committed reference cases
- **THEN** every value has the reference's width, every lane matches the
  reference or falls under one of the listed exceptions, and every signature
  in the fixture has all of its cases
