# Spec Delta

## ADDED Requirements

### Requirement: Output paths expand frame variables

Before any file is written, the tool SHALL expand husk's frame variables in every output path:
the `-o` value, each product's `productName`, and so the PNG beside them. `$F` is the integer
frame; `$F2`–`$F9` are that frame zero-padded to the width, sign included; `$FF` is the frame
with any fraction kept and no trailing `.0`. `${F}`, `${F4}` and `${FF}`, and husk's UDIM-style
`<F>`, `<F2>`–`<F9>` and `<FF>`, are the same variables. `\$` writes a literal `$`; any other `<…>`
is left as written.

#### Scenario: Padded frame in -o

- **WHEN** the user runs `crust render -i shot.usda -f 12 -o renders/beauty.$F4.exr` on a stage
  without products
- **THEN** the EXR is written to `renders/beauty.0012.exr` and the PNG to `renders/beauty.0012.png`

#### Scenario: Frame variable in an authored productName

- **WHEN** a product authors `productName = "renders/data.$F4.exr"` and the render uses `-f 7`
- **THEN** the product is written to `renders/data.0007.exr`

#### Scenario: Unpadded, float and braced forms

- **WHEN** the render uses `-f 12` and `-o` is `a.$F.${F3}.$FF.exr`
- **THEN** the file is `a.12.012.12.exr`

#### Scenario: UDIM-style forms in a Solaris productName

- **WHEN** a product authors `productName = "renders/beauty.<F4>.exr"`, another
  `"renders/raw.<F>.<FF>.exr"`, and the render uses `-f 12`
- **THEN** they are written to `renders/beauty.0012.exr` and `renders/raw.12.12.exr`

#### Scenario: Other angle-bracket tokens

- **WHEN** `-o` is `a.<UDIM>.<F4>.exr` and the render uses `-f 2`
- **THEN** the file is `a.<UDIM>.0002.exr` and no warning is logged

#### Scenario: Negative frame

- **WHEN** the render uses `-f -3` and `-o` is `a.$F4.exr`
- **THEN** the file is `a.-003.exr`

#### Scenario: Escaped dollar

- **WHEN** `-o` is `cost\$F4.exr` and the render uses `-f 1`
- **THEN** the file is `cost$F4.exr`

### Requirement: The frame a path expands without -f

Without `-f`, frame variables SHALL expand as frame 1, husk's default frame. Attributes still
read their default values, as they do today.

#### Scenario: No -f

- **WHEN** a product authors `productName = "beauty.$F4.exr"` and the render passes no `-f`
- **THEN** the product is written to `beauty.0001.exr`

### Requirement: Subframes round integer frame variables

On a fractional frame, `$F` and `$F2`–`$F9` (and `<F>`, `<F2>`–`<F9>`) SHALL round to the nearest frame, with halves rounded
away from zero, and SHALL raise one `product.subframe_frame` warning (kind `approximated`) per path
that names the path and suggests `$FF`. `$FF` and `<FF>` SHALL keep the fraction and raise nothing.

#### Scenario: A subframe with $F4

- **WHEN** the render uses `-f 12.5` and `-o` is `a.$F4.exr`
- **THEN** the file is `a.0013.exr` and a `[product.subframe_frame]` warning is logged

#### Scenario: A subframe with $FF

- **WHEN** the render uses `-f 12.5` and `-o` is `a.$FF.exr`
- **THEN** the file is `a.12.5.exr` and no warning is logged

### Requirement: Unknown path variables are left as written

A `$` followed by a name that is not a frame variable (`$SHOT`, `${HIP}`, `$F4x`) SHALL be left in
the path as written. Each such path SHALL raise one `product.unknown_variable` warning (kind
`refused`) naming the variable and the product, or `-o`. A `$` followed by no name SHALL be left
as written and SHALL raise nothing.

#### Scenario: An environment-style variable

- **WHEN** a product authors `productName = "$SHOT/beauty.$F4.exr"` and the render uses `-f 2`
- **THEN** the product is written to `$SHOT/beauty.0002.exr` and a
  `[product.unknown_variable]` warning names `$SHOT`

#### Scenario: A lone dollar

- **WHEN** `-o` is `price$.exr`
- **THEN** the file is `price$.exr` and no warning is logged

## MODIFIED Requirements

### Requirement: One multi-channel EXR per render product

For every `raster` RenderProduct, the tool SHALL write one single-part,
scanline, ZIP-compressed EXR at the product's resolved path. The path is the
`productName` evaluated at the render's time code, with its frame variables
then expanded ("Output paths expand frame variables"). A relative path resolves
against the working directory. Parent directories SHALL be created. The EXR
SHALL contain one layer per accepted RenderVar, in `orderedVars` order. Two
products whose expanded paths are the same file SHALL be treated as sharing a
path: the later one is refused with `product.shared_path`.

#### Scenario: Two products

- **WHEN** settings list products `renders/beauty.exr` (vars `color`,
  `alpha`) and `renders/data.exr` (vars `depth`, `normal`)
- **THEN** both files are written, and `renders/` is created if missing

#### Scenario: Time-sampled product name

- **WHEN** `productName` is time-sampled to `shot.0001.exr` at frame 1 and
  `shot.0002.exr` at frame 2, and the render uses `-f 2`
- **THEN** the product is written to `shot.0002.exr`

#### Scenario: Products that collide only once expanded

- **WHEN** one product authors `out.$F4.exr`, another authors `out.0003.exr`, and
  the render uses `-f 3`
- **THEN** the first is written to `out.0003.exr` and the second is refused with a
  `[product.shared_path]` warning
