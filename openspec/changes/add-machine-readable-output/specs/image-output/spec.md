# Spec Delta

## MODIFIED Requirements

### Requirement: EXR output

When the stage authors no RenderProduct, the tool SHALL write the rendered
buffer as an RGB EXR image to the `-o/--output` path (default `output.exr`),
with pixels byte-identical to the output before AOV support and a header that
differs from it only by the `crust:*` stamp ("EXRs record how they were
sampled", "EXRs record what they were rendered from"). When the stage
authors products, the tool SHALL write each product as described in "One
multi-channel EXR per render product".

#### Scenario: EXR is written

- **WHEN** a render of a stage without products completes
- **THEN** an EXR file is written at the requested output path with the
  rendered resolution

#### Scenario: Products replace the single EXR

- **WHEN** a render of a stage with products completes and `-o` is not given
- **THEN** each product is written to its `productName`, and no `output.exr`
  is written

### Requirement: EXRs record the working space

Every product EXR SHALL carry `colorInteropID` set to the working space's ASWF
Color Interop ID. When the working space is not linear Rec.709, every EXR —
products and the single beauty — SHALL also carry the space's
`chromaticities` where its interop ID has standard primaries. In linear
Rec.709 the single beauty EXR SHALL keep the pixels of the output before AOV
support, and its header except for the sampling stamp.

#### Scenario: An ACEScg render

- **WHEN** a stage without products renders with `--working-space acescg`
- **THEN** the EXR's header has `chromaticities` of AP1 with the ACES white
  and `colorInteropID = lin_ap1_scene`

### Requirement: Cropped renders record their place in the frame

When the render has a region smaller than the frame, every EXR it writes
(the single beauty EXR, and each product) SHALL have:

- a display window covering the full resolution;
- a data window covering the region;
- pixel data only for the region.

The tone-mapped PNG SHALL contain only the region's pixels, at the region's
size. When the region is the full frame, the PNG and the EXR's pixels SHALL be
byte-identical to the output before this change, and the EXR's header SHALL
differ from it only by the sampling stamp.

#### Scenario: Compositor placement

- **WHEN** a 640×360 render with `--region 320,0,640,360` is read into Nuke
- **THEN** the image has format 640×360, and its bounding box covers the
  right half

#### Scenario: PNG of a crop

- **WHEN** a render with `--region 0,0,64,32` completes
- **THEN** the PNG next to the EXR is 64×32 pixels

#### Scenario: No region, unchanged files

- **WHEN** a render without a region completes
- **THEN** its PNG, and its EXR's pixels, display and data windows, are
  byte-identical to those written before this change

## ADDED Requirements

### Requirement: EXRs record how they were sampled

Every EXR `crust render` writes, the single beauty and each product alike,
SHALL carry as `crust:*` header attributes every setting that changes how its
pixels were sampled or what they converge to, named as the CLI flags and USD
`crust:*` attributes that set them, plus the fewest and most samples any pixel
took.

#### Scenario: The attribute set

- **WHEN** any render writes an EXR
- **THEN** it carries `crust:spp`, `crust:minSpp`, `crust:sppTaken`,
  `crust:varianceThreshold`, `crust:indirectClamp` (`0` when off),
  `crust:maxDepth`, `crust:lightSamples`, `crust:lightSamplesIndirect`,
  `crust:samplingStrategy`, `crust:lightSelection`, `crust:pixelFilter` and
  `crust:pixelFilterRadius`

#### Scenario: A fixed-budget render

- **WHEN** `samples/cornellbox.usda` is rendered with `-s 16
  --indirect-clamp 0`
- **THEN** its EXR has `crust:spp = 16`, `crust:minSpp = 32`, `crust:sppTaken`
  of 16 and 16, and `crust:indirectClamp = 0`

#### Scenario: The filter used

- **WHEN** the render runs with `--filter gaussian --filter-radius 2`
- **THEN** its EXR has `crust:pixelFilter = gaussian` and
  `crust:pixelFilterRadius = 2`

#### Scenario: Every product is stamped

- **WHEN** a stage authors two RenderProducts
- **THEN** both EXRs carry the same `crust:*` attributes

### Requirement: EXRs record what they were rendered from

Every EXR `crust render` writes SHALL carry `crust:version`, and
`crust:frame` and `crust:camera` holding the time code the stage was evaluated
at and the camera actually rendered through, after any fallback. Without a
time code, `crust:frame` SHALL be absent; for the procedural camera,
`crust:camera` SHALL be absent.

#### Scenario: A subframe

- **WHEN** the render runs with `-f 10.5`
- **THEN** `crust:frame` is `10.5`, not the sampler seed `10`

#### Scenario: Camera fallback

- **WHEN** `RenderSettings.camera` names a missing camera and the render
  falls back to `/cams/first`
- **THEN** `crust:camera` is `/cams/first`

#### Scenario: No time code, procedural camera

- **WHEN** the procedural fallback scene renders without `-f`
- **THEN** the EXR has no `crust:frame` and no `crust:camera`

### Requirement: Crust's stamp wins over authored product attributes

When a RenderProduct authors an EXR attribute whose name starts with
`crust:`, the written EXR SHALL carry crust's own value for that name, and a
warning naming the product and the attribute SHALL be logged.

#### Scenario: An authored stamp

- **WHEN** a product authors `driver:parameters:crust:spp = "1024"` and the
  render runs at `-s 16`
- **THEN** the EXR's `crust:spp` is 16 and a warning is logged
