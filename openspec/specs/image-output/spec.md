# image-output Specification

## Purpose

Persist the rendered pixel buffer to disk. The renderer writes a linear
high-dynamic-range EXR, then converts it to a tone-mapped sRGB PNG for viewing.
Both steps live in the `crust-render` CLI (`main.rs`, and `products.rs` for
authored `RenderProduct`s); the engine crate only produces
the `Buffer`.

## Requirements

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

### Requirement: PNG path always tracks the EXR output path

The PNG conversion step SHALL derive its path from the path of the EXR it
previews, swapping the extension to `.png`, so the two stay in sync: the
`-o/--output` value when the stage authors no RenderProduct, and the first
product's path — which `-o` replaces when given — when it does.

#### Scenario: Custom output name keeps EXR and PNG in sync

- **WHEN** the user renders with `-o some_other_name.exr`
- **THEN** that EXR is written, and the PNG is written to
  `some_other_name.png` alongside it

#### Scenario: Products without -o

- **WHEN** the stage's first product is `renders/beauty.exr` and holds a
  beauty var, and no `-o` is given
- **THEN** the PNG is written to `renders/beauty.png`

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

### Requirement: Refused products

A `deepRaster` product, or a product whose resolved camera or resolution
differs from the render's, SHALL be skipped with one warning naming it.

#### Scenario: Deep product

- **WHEN** a product authors `productType = "deepRaster"`
- **THEN** a warning is logged and no file is written for it, while the other
  products are written

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
  `crust:samplingStrategy`, `crust:lightSelection`, `crust:pixelFilter`,
  `crust:pixelFilterRadius` and `crust:sampler`

#### Scenario: A fixed-budget render

- **WHEN** `samples/cornellbox.usda` is rendered with `-s 16
  --indirect-clamp 0`
- **THEN** its EXR has `crust:spp = 16`, `crust:minSpp = 32`, `crust:sppTaken`
  of 16 and 16, and `crust:indirectClamp = 0`

#### Scenario: The filter used

- **WHEN** the render runs with `--filter gaussian --filter-radius 2`
- **THEN** its EXR has `crust:pixelFilter = gaussian` and
  `crust:pixelFilterRadius = 2`

#### Scenario: The sampler used

- **WHEN** the render runs with `--sampler latticeBn`
- **THEN** its EXR has `crust:sampler = latticeBn`; a render with no sampler setting has
  `crust:sampler = sobol`

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

### Requirement: EXRs record an interrupted render

Every EXR written from a cancelled render (the single beauty EXR and each product)
SHALL carry a string header attribute `crust:renderStatus = "interrupted"`. EXRs from
a completed render SHALL NOT carry it, and SHALL stay byte-identical to those
written before this change.

#### Scenario: An interrupted render's EXR

- **WHEN** Ctrl-C cuts a render short and its EXR is written
- **THEN** the EXR's header has `crust:renderStatus` set to `interrupted`

#### Scenario: A Ctrl-C after the last sample

- **WHEN** Ctrl-C arrives after the render traced its last sample, before its
  outputs are being written
- **THEN** its EXRs are complete and carry no `crust:renderStatus`

#### Scenario: A completed render's EXR

- **WHEN** a render completes
- **THEN** its EXR has no `crust:renderStatus` attribute and is byte-identical to
  the output before this change

### Requirement: EXRs record the exposure scale

Every EXR the render writes SHALL record the exposure scale its radiance channels were
multiplied by, as a float attribute `crust:exposureScale` in its header, `1` when no
exposure applied.

#### Scenario: An exposed render

- **WHEN** a stage whose camera authors `exposure = 2` is rendered
- **THEN** each EXR written records `crust:exposureScale = 4`

#### Scenario: No exposure

- **WHEN** a stage whose camera authors no exposure is rendered
- **THEN** each EXR written records `crust:exposureScale = 1`

### Requirement: EXRs record the lens and the layer stack

Every EXR `crust render` writes SHALL record what shaped its pixels beyond the
sampling settings:

- the render camera's lens at the frame, under its USD attribute names:
  `crust:focalLength`, `crust:horizontalAperture`, `crust:verticalAperture` (as
  authored, in millimetres), `crust:aspectRatioConformPolicy`, `crust:fStop` and
  `crust:focusDistance` — absent for the procedural camera;
- `crust:depthOfField` and `crust:motionBlur`, `1` or `0`: whether the lens was
  open (an `fStop` above 0, depth of field not disabled), and whether moving
  geometry was blurred (motion blur on, over a shutter that opens an interval);
- `crust:layerStack`: the files of the stage's root layer stack in strength order
  (the session layer, the root layer, its sublayers), one per line as the
  16-digit hex FNV-1a hash of its bytes as the scene was loaded (`-` when they
  could not be read), a space, and its absolute path, printable ASCII as is and
  every other UTF-8 byte (and `%`) as `%XX` — absent when no layer is known;
- `crust:invisibleOverrides`: the prims the strongest layer of that stack hides
  with an `over` authoring `visibility = "invisible"` (as a default or a time
  sample) — absent for a hand-assembled scene.

#### Scenario: An override layer that opens the lens

- **WHEN** a layer that sublayers `samples/cornellbox.usda`, sets the camera's
  `fStop = 0.02` and `focusDistance = 360` and hides one prim with an `over` is
  rendered
- **THEN** its EXR has `crust:fStop = 0.02`, `crust:focusDistance = 360`,
  `crust:depthOfField = 1`, `crust:invisibleOverrides = 1`, and a
  `crust:layerStack` listing the override layer and then the shot, each with
  its hash

#### Scenario: A pinhole

- **WHEN** `samples/cornellbox.usda` is rendered as it is
- **THEN** its EXR has `crust:depthOfField = 0` and `crust:invisibleOverrides = 0`
