# Spec Delta

## ADDED Requirements

### Requirement: Filters are authored on the render settings

Crust SHALL read sample filters from the ordered relationship `crust:sampleFilters`, and
display filters from the ordered relationship `crust:displayFilters`, both on the
`RenderSettings` prim the render uses (after the active render pass is applied). The
filters SHALL run in list order: sample filters first, then display filters. A stage that
authors neither relationship SHALL render exactly as it did before this capability
existed.

#### Scenario: A stage with no filters

- **WHEN** `samples/cornellbox.usda` is rendered at `-s 16` before and after this change
- **THEN** every channel of the two EXRs is bit-identical

#### Scenario: Filters follow the active pass

- **WHEN** two render passes point at settings with different `crust:displayFilters`
- **THEN** the render of each pass runs that pass's filters

### Requirement: A sample filter is a MaterialX graph over the sample's AOVs

A target of `crust:sampleFilters` SHALL be a `NodeGraph` of MaterialX standard-library
nodes. An interface input `inputs:<name>` SHALL read, on every camera sample, that
sample's value of the RenderVar whose channel name is `<name>`. The reserved name
`beauty` SHALL read the sample's beauty radiance before exposure. The graph's outputs
SHALL be accumulated with the beauty's pixel-filter weights, so they are filtered
exactly like a filtered AOV.

#### Scenario: A per-sample recombination is antialiased

- **WHEN** a sample filter outputs `matte > 0.5 ? key : fill`, and a pixel straddles the
  matte's edge with half its samples on each side
- **THEN** that pixel of the filter's output is the weighted mean of `key` and `fill`
  over its samples, not either value alone

#### Scenario: A closest var is read per sample

- **WHEN** a sample filter's input is bound to `depth` (accumulated `closest`)
- **THEN** each sample's evaluation reads that sample's own first-hit depth

#### Scenario: An unbound input

- **WHEN** an interface input names no RenderVar of the render
- **THEN** the import raises `precomp.unbound_input` naming the input, and the input
  reads its authored default

#### Scenario: A node a sample cannot evaluate

- **WHEN** a sample filter contains an `image` or `geompropvalue` node, or a BSDF
- **THEN** the import raises `precomp.unsupported_node`, and the filter's outputs read
  their clear value while the render proceeds

### Requirement: A display filter is a fixed image operation

A target of `crust:displayFilters` SHALL be a `Shader` prim whose `info:id` is one of
`crust:convolve`, `crust:blur`, `crust:vectorBlur`, `crust:merge` or `crust:grade`. Its
image inputs SHALL name a RenderVar, a sample-filter output (`<graph>.<output>`) or an
earlier display filter. Display filters SHALL run on the exposed, accumulated image, and
their result SHALL be bitwise independent of the thread count and of the render mode
(tiles or scanlines).

#### Scenario: A radial blur driven by a direction

- **WHEN** a `crust:vectorBlur` with a `center` and `strength` reads the beauty
- **THEN** its output is the beauty blurred along lines through the centre, and the same
  render with `RAYON_NUM_THREADS=1` gives a bit-identical output

#### Scenario: Motion-driven blur

- **WHEN** a `crust:vectorBlur` reads its vectors from the `motion` RenderVar
- **THEN** a static pixel (zero motion) is unchanged and a moving one is blurred along
  its motion vector

#### Scenario: An unknown operation

- **WHEN** a display filter's `info:id` is `crust:glare`
- **THEN** the import raises `precomp.unknown_operation`, and the filter passes its `in`
  input through unchanged

#### Scenario: A filter reading a later one

- **WHEN** a display filter names a filter that comes after it in the list
- **THEN** the import raises `precomp.cycle`, and the input reads its clear value

### Requirement: The displayed output

`token crust:display` on `RenderSettings` SHALL name the output a host shows as the
image: the tone-mapped PNG, `--checkpoint` snapshots, and an MCP session's `render` and
`snapshot` results. Unauthored, the displayed output SHALL be the beauty. A name that
matches no output SHALL raise `precomp.unknown_display`, and the beauty is then shown.

#### Scenario: The PNG shows the look

- **WHEN** a stage authors `crust:display = "look"` and a RenderVar `look` reading a
  filter output
- **THEN** the PNG beside the first product is the tone-mapped `look`, and the EXR's
  beauty channels are still the beauty

#### Scenario: A progressive snapshot shows the look

- **WHEN** a render with a displayed filter output is snapshotted halfway, through
  `--checkpoint` or an MCP `snapshot`
- **THEN** the snapshot is the display chain applied to the units finished so far, with
  unfinished units read as the clear value

### Requirement: Filters never change the beauty or how it is sampled

No sample or display filter SHALL change the beauty channels, the samples any pixel
takes, or the adaptive stop decision. The beauty of a render with filters SHALL be
bit-identical to the same render without them.

#### Scenario: Beauty unchanged by a precomp

- **WHEN** a scene is rendered at `-s 16` with and without a sample filter and two
  display filters
- **THEN** the beauty channels are bit-identical, and so is `crust:sppTaken`

#### Scenario: A stage with no beauty

- **WHEN** every surface of a stage writes only AOVs and no emission or reflection
  reaches the beauty
- **THEN** the import raises `precomp.black_beauty`, which recommends
  `crust:minSamplesPerPixel = crust:samplesPerPixel`
