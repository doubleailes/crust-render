# Spec Delta

## MODIFIED Requirements

### Requirement: Output paths expand frame variables

Before any file is written, the tool SHALL expand husk's frame variables in every output path:
the `-o` value, each product's `productName`, and so the PNG beside them. With `--pass`, `-o` is the pass's file pattern instead, expanded
only with the pattern's specifiers (`render-passes`). `$F` is the integer
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

#### Scenario: -o with a pass is not expanded as husk's

- **WHEN** the user runs `crust render --pass /Render/Passes/render -f 12 -o 'out/$n.$04f.$x'`
- **THEN** the path is expanded only as a pass pattern, and no `[product.unknown_variable]`
  warning names `$n`, `$04f` or `$x`
