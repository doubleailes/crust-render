## ADDED Requirements

### Requirement: The .tx beside a source follows Arnold's name

For a UV texture `<stem>.<ext>` bound in space S, the `.tx` used SHALL be
`<stem>_raw.<ext>.tx` when S is unset, `raw`, or the target space, and otherwise
`<stem>_<C>_<T>.<ext>.tx`, where C is S's canonical name in the OCIO config (never an
alias) and T the target space (`--tx-color-space`, default `ACEScg`). `<stem>.tx` SHALL
NOT be looked for. UDIM tokens SHALL stay in `<stem>`.

#### Scenario: An sRGB albedo bound through an alias

- **WHEN** `chair.1001.tif` is bound with `colorspace="srgb_tx"` and the target space
  is `ACEScg`
- **THEN** the `.tx` looked for is
  `chair.1001_sRGB Encoded Rec.709 (sRGB)_ACEScg.tif.tx`, the name Arnold 7.5 gives
  the same binding

#### Scenario: A texture already in the target space

- **WHEN** `wall.png` is bound as `ACEScg` with the target space `ACEScg`
- **THEN** the `.tx` looked for is `wall_raw.png.tx`

#### Scenario: Arnold's raw keyword and the OCIO Raw space

- **WHEN** `mask.png` is bound once with UsdUVTexture `sourceColorSpace = "raw"` and
  once with MaterialX `colorspace = "Raw"`, target `ACEScg`
- **THEN** the first looks for `mask_raw.png.tx` and the second for
  `mask_Raw_ACEScg.png.tx`, as Arnold 7.5 names them

#### Scenario: A UsdUVTexture tagged sRGB

- **WHEN** `chair.png` is bound with UsdUVTexture `sourceColorSpace = "sRGB"`, target
  `ACEScg`
- **THEN** the `.tx` looked for is `chair_sRGB Encoded Rec.709 (sRGB)_ACEScg.png.tx`,
  and Arnold's `chair_sRGB_ACEScg.png.tx`, which holds the bytes unconverted, is not
  read

#### Scenario: A crust .tx from before this change

- **WHEN** only `albedo.tx` stands beside `albedo.png`
- **THEN** it is not used, and the texture is preloaded or, under `--auto-tx`,
  converted to the Arnold-named file

### Requirement: A .tx found beside its source is read in the space its name gives

A `.tx` found by the lookup SHALL be read in the target space T when its name carries a
conversion (`_<C>_<T>`). A `_raw` file SHALL be read in the binding's space S. Either
way the lookup SHALL return linear values in the render's working space, converting
from that space with its curve and primaries.

#### Scenario: An Arnold .tx of an sRGB albedo in a Rec.709 render

- **WHEN** a render whose working space is `lin_rec709` binds `chair.1001.tif` as
  sRGB and finds Arnold's `chair.1001_sRGB Encoded Rec.709 (sRGB)_ACEScg.tif.tx`
- **THEN** the values are read as linear ACEScg and converted to Rec.709 primaries,
  with no sRGB curve applied

#### Scenario: The same look as the source

- **WHEN** a texture is rendered from its source (`CRUST_TEX_STREAM=0`) and from a
  `.tx` Arnold's `maketx` wrote for the same binding
- **THEN** the two lookups agree within the tolerance stated in `design.md` at
  every footprint

### Requirement: A directly bound .tx is read in the binding's space

A UV texture whose bound file is itself a `.tx` SHALL be read as stored, in the
binding's declared colour space, with that space's transfer curve and primaries
applied whatever the file's sample type, name or metadata. This SHALL match Arnold's
reading of the same binding.

#### Scenario: An already-converted .tx bound as sRGB

- **WHEN** a `.tx` holding half values converted to ACEScg is bound directly with
  `colorspace="sRGB Encoded Rec.709 (sRGB)"`
- **THEN** the sRGB curve and the Rec.709 to working-space change are applied to the
  stored values, and the result matches Arnold 7.5's within tolerance (which converts
  twice and reads about four times too dark)

#### Scenario: An already-converted .tx bound with no colour space

- **WHEN** the same `.tx` is bound with `colorspace` unset in a render whose working
  space is ACEScg
- **THEN** the stored values are returned unchanged

### Requirement: --auto-tx writes what Arnold's auto-tx writes

Under `--auto-tx`, a missing or stale `.tx` SHALL be written under the name above, as
Arnold 7.5's auto-tx would write it for the same binding and target. That means the
same file type, sample type and compression, the same conversion and mip chain (OIIO
box filter, levels rounded down), and the same detections (opaque, constant-colour,
monochrome) and NaN fixing. Values SHALL be unpremultiplied around the conversion and
stored premultiplied.

#### Scenario: An 8-bit sRGB source

- **WHEN** `--auto-tx` converts an 8-bit PNG bound as sRGB with the target `ACEScg`
- **THEN** it writes a 64x64-tiled, mip-mapped half EXR with DWAA compression and
  levels rounded down, holding linear ACEScg, with `oiio:ColorSpace="Linear"`,
  `textureformat="Plain Texture"`, `wrapmodes="black,black"` and `autotx=1`

#### Scenario: A linear or data source

- **WHEN** `--auto-tx` converts an 8-bit PNG bound as `Linear Rec.709 (sRGB)` or as
  `Raw`
- **THEN** it writes a tiled, mip-mapped TIFF at the source's bit depth with zip
  compression, converted to the target space

#### Scenario: A float or 16-bit source

- **WHEN** `--auto-tx` converts a float EXR or a 16-bit PNG, whatever its colour space
- **THEN** it keeps the source's sample type: a float EXR with zip compression, or a
  16-bit TIFF with zip compression

#### Scenario: A greyscale source

- **WHEN** the source's three channels are equal at every texel
- **THEN** the `.tx` holds a single channel, and a lookup reads it as grey

#### Scenario: A source that is already tiled and mip-mapped

- **WHEN** a texture is bound to a tiled, mip-mapped EXR (every ALab texture is one),
  with or without `--auto-tx`
- **THEN** it streams from that file as it is, no `.tx` is written beside it, and none
  is looked for — where Arnold's auto-tx converts it to `<stem>_raw.exr.tx` (or the
  converted name), which crust leaves unread

### Requirement: A .tx crust writes is one Arnold keeps

A `.tx` written under `--auto-tx` SHALL carry the OIIO metadata Arnold 7.5 writes for
the same binding, including the `Software` string Arnold would record (its
`maketx` command line, prefix included), and SHALL take the source's modification time
truncated to whole seconds, so that Arnold's own update check finds it current.

#### Scenario: Arnold reads crust's file

- **WHEN** Arnold 7.5 renders a binding whose `.tx` crust wrote with `--auto-tx`
- **THEN** Arnold does not regenerate it, and its render matches its render of its own
  `.tx` within tolerance

### Requirement: A .tx is stale when its time differs from its source's

A `.tx` SHALL be stale when it is missing or its modification time differs from its
source's in whole seconds, the resolution Arnold records it at. Under `--auto-tx` a
stale `.tx` SHALL be rewritten. Without it, a `.tx` with a different time SHALL be used
with a warning and a missing one SHALL preload. A `.tx` SHALL NOT be judged by a
crust-specific version marker.

#### Scenario: A source edited after conversion

- **WHEN** a source is saved after its `.tx` was written and the render runs without
  `--auto-tx`
- **THEN** the `.tx` is used and a warning names it and suggests `--auto-tx`

#### Scenario: A .tx Arnold wrote

- **WHEN** Arnold's auto-tx wrote the `.tx`, with its modification time equal to the
  source's
- **THEN** `--auto-tx` does not rewrite it

## MODIFIED Requirements

### Requirement: Colour space travels with the texture

Every texture request SHALL carry the colour space the binding declares, as authored
and as resolved, and the working space the render is in, and lookups SHALL return
linear values in the working space. A declared space SHALL be converted with its
transfer curve and its change of primaries, as the OCIO config defines them. `auto`
SHALL resolve as Arnold resolves it: an image with integer samples (8- or 16-bit, any
channel count) is sRGB, and a half or float image is unconverted. For MaterialX, an
absent `colorspace` (on the input, its node, its nodegraph and the document) SHALL
mean `auto` for a colour image (`color3`, `color4`), and a non-colour image SHALL never
be converted. For `UsdUVTexture`, a `colorSpace` metadatum on `inputs:file` SHALL win
over `sourceColorSpace`, and an absent `sourceColorSpace` means `auto`. A conversion
the renderer cannot split into a per-channel curve and a matrix SHALL be refused with
a warning and the values used as stored.

#### Scenario: A greyscale roughness PNG under UsdUVTexture

- **WHEN** a single-channel 8-bit PNG is bound with no `sourceColorSpace`
- **THEN** it is read as sRGB, its curve decoded, as Arnold 7.5 reads it

#### Scenario: The same roughness through MaterialX

- **WHEN** that PNG is bound through `ND_image_float` with no `colorspace`
- **THEN** it is read as raw data, with no transfer curve applied

#### Scenario: A MaterialX albedo with no colorspace

- **WHEN** an 8-bit RGB PNG is bound through `ND_image_color3` and no `colorspace` is
  authored anywhere above it
- **THEN** it is read as sRGB, its curve decoded

#### Scenario: A float texture under auto

- **WHEN** a float EXR is bound with `sourceColorSpace = "auto"`
- **THEN** its values are used as stored

#### Scenario: An sRGB albedo rendered in ACEScg

- **WHEN** an 8-bit texture tagged `srgb_texture` is bound in a render whose
  working space is ACEScg
- **THEN** a lookup returns the sRGB-decoded colour converted from Rec.709 to
  AP1 primaries, and a streamed `.tx` of it returns the same colour within the
  tolerance stated in `design.md`

### Requirement: Filtered minification

Each lookup SHALL receive a footprint width derived from a ray cone, and
textures with a mip pyramid SHALL select levels trilinearly from it. Pyramids
that crust builds in memory SHALL be reduced in linear light; a `.tx`'s levels
SHALL be read as stored. A width of zero SHALL mean point sampling at the finest
level.

#### Scenario: Ray cones disabled

- **WHEN** a render runs with `CRUST_RAY_CONES=0`
- **THEN** every lookup reads the finest level, bit-identical to a render with
  no mip pyramids

### Requirement: Streaming is an A/B of preloading

A UV texture with a `.tx` found by the lookup above SHALL stream through a bounded
tile cache (`CRUST_TEX_CACHE_MB`); `CRUST_TEX_STREAM=0` SHALL preload from the source
instead. Streamed and preloaded lookups SHALL agree within the tolerance stated in
`design.md`, alpha included: a `.tx` converted from a source whose alpha cuts SHALL
carry it. A UDIM set SHALL stream only when every tile has its `.tx`, and SHALL preload
otherwise.

#### Scenario: A stray .tx

- **WHEN** a `.tx` cannot be read
- **THEN** the texture is preloaded from its source and the render still succeeds

#### Scenario: A streamed cutout

- **WHEN** an 8-bit RGBA texture whose alpha cuts is converted with `--auto-tx` and
  sampled streamed and preloaded at any footprint
- **THEN** the two lookups agree within tolerance on all four components

#### Scenario: A .tx converted before alpha

- **WHEN** an RGBA texture's only `.tx` is a `<stem>.tx` written by an older crust that
  dropped alpha
- **THEN** that file is not looked for: without `--auto-tx` the texture is preloaded
  and its alpha read, and with `--auto-tx` the Arnold-named `.tx` is written and
  streams the alpha

#### Scenario: A partly converted UDIM set

- **WHEN** some tiles of a UDIM set have their `.tx` and others do not, and
  `--auto-tx` is not given
- **THEN** the whole set is preloaded, with a warning naming the count of missing tiles

### Requirement: Texture alpha

A UV texture lookup SHALL return the file's alpha as its fourth component: an
image's authored alpha channel, an EXR's channel whose base name is `A`, or a
`.tx`'s alpha sample. A texture whose file has no alpha SHALL read 1.0 there. A
TIFF extra sample that `ExtraSamples` leaves unspecified SHALL NOT be alpha.
Alpha SHALL be read as coverage: a byte as `a / 255`, a float as stored, with no
transfer curve or change of primaries whatever the texture's colour space, and
mip levels SHALL average it independently of the colour. A lookup's colour SHALL be
premultiplied by its alpha, in linear light, as Arnold's textures are: a source that
stores straight colour (an image with unassociated alpha) SHALL be premultiplied once
when it is loaded or paged in, and a source that stores premultiplied colour (an EXR,
a `.tx` with associated alpha) SHALL be read as stored. Preloaded and streamed lookups
SHALL agree on it.

#### Scenario: A leaf card's alpha

- **WHEN** an RGBA PNG whose left half has alpha 0 and right half alpha 255 is
  sampled at texel centres
- **THEN** the left half reads alpha 0 and colour 0, and the right half alpha 1 and the
  colour the same image saved without alpha reads

#### Scenario: An sRGB texture's alpha

- **WHEN** an RGBA PNG with alpha 128 everywhere is bound as `srgb_texture`
- **THEN** every lookup, at every footprint, reads alpha 128/255 and the sRGB-decoded
  colour times 128/255

#### Scenario: A texture without alpha

- **WHEN** an RGB PNG is sampled
- **THEN** its alpha reads 1.0 and its colour is unchanged
