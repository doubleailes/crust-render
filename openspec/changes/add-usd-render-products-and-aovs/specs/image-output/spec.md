## ADDED Requirements

### Requirement: One multi-channel EXR per render product

For every `raster` RenderProduct, the tool SHALL write one single-part,
scanline, ZIP-compressed EXR at the product's resolved path. The path is the
`productName` evaluated at the render's time code. A relative path resolves
against the working directory. Parent directories SHALL be created. The EXR
SHALL contain one layer per accepted RenderVar, in `orderedVars` order.

#### Scenario: Two products

- **WHEN** settings list products `renders/beauty.exr` (vars `color`,
  `alpha`) and `renders/data.exr` (vars `depth`, `normal`)
- **THEN** both files are written, and `renders/` is created if missing

#### Scenario: Time-sampled product name

- **WHEN** `productName` is time-sampled to `shot.0001.exr` at frame 1 and
  `shot.0002.exr` at frame 2, and the render uses `-f 2`
- **THEN** the product is written to `shot.0002.exr`

### Requirement: Channel naming and precision

Channel names SHALL follow `<layer>.<component>`. The layer is the var's
`driver:parameters:aov:name` if authored, else the RenderVar prim name. An
authored `driver:parameters:aov:channel_prefix` SHALL replace the layer.

Components:

- colour vars use `R`, `G`, `B` (plus `A` for 4-component types);
- vector data uses `X`, `Y`, `Z`;
- UVs use `U`, `V`;
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

### Requirement: Refused products

A `deepRaster` product, or a product whose resolved camera or resolution
differs from the render's, SHALL be skipped with one warning naming it.

#### Scenario: Deep product

- **WHEN** a product authors `productType = "deepRaster"`
- **THEN** a warning is logged and no file is written for it, while the other
  products are written

## MODIFIED Requirements

### Requirement: EXR output

When the stage authors no RenderProduct, the tool SHALL write the rendered
buffer as an RGB EXR image to the `-o/--output` path (default `output.exr`),
byte-identical to the output before AOV support. When the stage authors
products, the tool SHALL write each product as described in "One
multi-channel EXR per render product".

#### Scenario: EXR is written

- **WHEN** a render of a stage without products completes
- **THEN** an EXR file is written at the requested output path with the
  rendered resolution

#### Scenario: Products replace the single EXR

- **WHEN** a render of a stage with products completes and `-o` is not given
- **THEN** each product is written to its `productName`, and no `output.exr`
  is written

### Requirement: Tone-mapped sRGB PNG conversion next to the EXR

After writing the EXR, the tool SHALL produce a viewable PNG. It does this by
clamping the beauty's linear values to [0,1], applying the sRGB transfer
curve, and quantizing to 8-bit. The PNG is saved next to the EXR at the same
path with a `.png` extension. With products, the PNG SHALL be made from the
first product's beauty var and saved beside that product. A first product
without a beauty var SHALL produce no PNG.

#### Scenario: PNG is produced from the render

- **WHEN** the render's EXR has been written at `-o` path `renders/foo.exr`
- **THEN** a tone-mapped sRGB PNG is saved at `renders/foo.png`

#### Scenario: PNG follows the first product

- **WHEN** the first product is `renders/beauty.exr` and holds a `color` var
- **THEN** the PNG is saved at `renders/beauty.png`
