## MODIFIED Requirements

### Requirement: Channel naming and precision

Channel names SHALL follow `<layer>.<component>`. The layer is the var's
`driver:parameters:aov:name` if authored, else the RenderVar prim name. An
authored `driver:parameters:aov:channel_prefix` SHALL replace the layer.

Components:

- colour vars use `R`, `G`, `B` (plus `A` for 4-component types);
- vector data uses `X`, `Y`, `Z`;
- UVs use `U`, `V`;
- motion vectors use lowercase `u`, `v`, Nuke's names for its `forward` and
  `backward` layers' channels;
- a scalar var is one channel named after the layer.

The first var resolving to the beauty SHALL be written unprefixed (`R`, `G`,
`B`[, `A`]).

Sample precision:

- `half` / `*h` types → HALF;
- `float` / `*f` types → FLOAT;
- `int` → UINT.

An authored `driver:parameters:aov:format` SHALL override `dataType`. The
header SHALL carry the software name and `colorInteropID = "lin_rec709_scene"`.

#### Scenario: Beauty, depth and normal channels

- **WHEN** a product's vars are `color` (color4f), `Z` (float) and `N`
  (normal3f)
- **THEN** the EXR has channels `R`, `G`, `B`, `A`, `Z`, `N.X`, `N.Y`, `N.Z`

#### Scenario: Half precision on request

- **WHEN** a colour var authors `driver:parameters:aov:format = "half3"`
- **THEN** its channels are stored as HALF

#### Scenario: Motion vectors on Nuke's forward layer

- **WHEN** a RenderVar named `forward` authors `sourceName = "motionvector"`
  and `dataType = "float2"`
- **THEN** the EXR has channels `forward.u` and `forward.v`, stored as FLOAT
