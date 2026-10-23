# Spec Delta

## MODIFIED Requirements

### Requirement: AOVs never change the beauty

Requesting AOVs SHALL NOT change the beauty image, and neither SHALL authoring sample or
display filters (`precomp-filters`). With any set of RenderVars requested and any
filters authored, the beauty SHALL be bit-identical to the same render with no
RenderVars and no filters. A render that requests no AOV beyond the beauty and authors
no filter SHALL take the same code path as a render with no products.

#### Scenario: Beauty unchanged by AOVs

- **WHEN** a scene is rendered at `-s 16` once with no products, and once with
  a product requesting `depth`, `normal` and three LPE vars
- **THEN** the beauty channels of the two outputs are bit-identical

#### Scenario: Beauty unchanged by filters

- **WHEN** the same scene is rendered once more with a sample filter over the three LPE
  vars and a display filter blurring its output
- **THEN** the beauty channels are bit-identical to the render with no products

#### Scenario: Tiles and scanlines agree for every AOV

- **WHEN** the same AOV request is rendered with and without `--scanline`
- **THEN** every channel of the two outputs is bit-identical, filter outputs included

## ADDED Requirements

### Requirement: A RenderVar may read a filter output

A RenderVar that authors `rel crust:filter` SHALL write the output of the sample or
display filter it targets: the output named by `token crust:filter:output`, or the
filter's only output when that is unauthored. Its `sourceType` and `sourceName` SHALL
then be ignored. Its `dataType` SHALL fit the output, under the same rule as any var
(`aov.type_mismatch`). Sample-filter outputs SHALL be scaled by camera exposure unless
the var authors `bool crust:filter:radiance = 0`. A variance var SHALL NOT be bound as a
filter input (`precomp.unbindable_input`).

#### Scenario: A filter output in a product

- **WHEN** a product holds a var `look` with `rel crust:filter =
  </Render/Precomp/look>` and `dataType = "color3f"`
- **THEN** the product's EXR has `look.R`, `look.G` and `look.B` channels holding the
  filter's accumulated output

#### Scenario: A non-radiance output ignores exposure

- **WHEN** a sample filter outputs a matte, its var authors `crust:filter:radiance = 0`,
  and the camera has `exposure = 2`
- **THEN** the matte channel is the same as with `exposure = 0`
