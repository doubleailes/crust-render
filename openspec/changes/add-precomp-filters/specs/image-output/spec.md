# Spec Delta

## MODIFIED Requirements

### Requirement: Tone-mapped sRGB PNG conversion next to the EXR

After writing the EXR, the tool SHALL produce a viewable PNG by encoding the
displayed output through the OCIO config's display and view (`--display`, `--view`;
default `sRGB - Display` / `Un-tone-mapped`, which clamps to [0,1] and applies
the sRGB curve) and quantizing to 8-bit. The displayed output is the beauty
unless the render settings author `crust:display` (`precomp-filters`); a float
displayed output SHALL be written as grey. The PNG is saved next to the EXR at
the same path with a `.png` extension. With products, the PNG SHALL be made
from the first product's beauty var, or from its displayed output when one is
authored, and saved beside that product. A first product without a beauty var
and no authored displayed output SHALL produce no PNG. A display or view the
config cannot make SHALL be an error before the render.

#### Scenario: PNG is produced from the render

- **WHEN** the render's EXR has been written at `-o` path `renders/foo.exr`
- **THEN** a tone-mapped sRGB PNG is saved at `renders/foo.png`

#### Scenario: PNG follows the first product

- **WHEN** the first product is `renders/beauty.exr` and holds a `color` var
- **THEN** the PNG is saved at `renders/beauty.png`

#### Scenario: PNG shows the displayed output

- **WHEN** the first product is `renders/beauty.exr` and the settings author
  `crust:display = "look"`
- **THEN** `renders/beauty.png` is the tone-mapped `look` output, not the beauty

## ADDED Requirements

### Requirement: EXRs record the precomp

Every EXR `crust render` writes SHALL carry `crust:sampleFilters` and
`crust:displayFilters` (the filter prim paths in evaluation order, comma-separated, and
empty when none) and `crust:display` (the displayed output's name, `beauty` when
unauthored). Crust's values SHALL win over authored product attributes of the same name.

#### Scenario: A render without a precomp

- **WHEN** `samples/cornellbox.usda` is rendered
- **THEN** its EXR has empty `crust:sampleFilters` and `crust:displayFilters`, and
  `crust:display = beauty`

#### Scenario: A render with a precomp

- **WHEN** the settings author `crust:sampleFilters = [</Render/Precomp/look>]`,
  `crust:displayFilters = [</Render/Precomp/glow>]` and `crust:display = "look"`
- **THEN** every product's EXR has `crust:sampleFilters = /Render/Precomp/look`,
  `crust:displayFilters = /Render/Precomp/glow` and `crust:display = look`
