## MODIFIED Requirements

### Requirement: Tone-mapped sRGB PNG conversion next to the EXR

After writing the EXR, the tool SHALL produce a viewable PNG by encoding the
beauty through the OCIO config's display and view (`--display`, `--view`;
default `sRGB - Display` / `Un-tone-mapped`, which clamps to [0,1] and applies
the sRGB curve) and quantizing to 8-bit. The PNG is saved next to the EXR at
the same path with a `.png` extension. With products, the PNG SHALL be made
from the first product's beauty var and saved beside that product. A first
product without a beauty var SHALL produce no PNG. A display or view the
config cannot make SHALL be an error before the render.

#### Scenario: PNG is produced from the render

- **WHEN** the render's EXR has been written at `-o` path `renders/foo.exr`
- **THEN** a tone-mapped sRGB PNG is saved at `renders/foo.png`

#### Scenario: PNG follows the first product

- **WHEN** the first product is `renders/beauty.exr` and holds a `color` var
- **THEN** the PNG is saved at `renders/beauty.png`

## ADDED Requirements

### Requirement: EXRs record the working space

Every product EXR SHALL carry `colorInteropID` set to the working space's ASWF
Color Interop ID. When the working space is not linear Rec.709, every EXR —
products and the single beauty — SHALL also carry the space's
`chromaticities` where its interop ID has standard primaries. In linear
Rec.709 the single beauty EXR SHALL keep the header and pixels of the output before
AOV support.

#### Scenario: An ACEScg render

- **WHEN** a stage without products renders with `--working-space acescg`
- **THEN** the EXR's header has `chromaticities` of AP1 with the ACES white
  and `colorInteropID = lin_ap1_scene`
