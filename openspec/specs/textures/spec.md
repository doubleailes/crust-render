# textures Specification

## Purpose

Supply texture values to materials: UV (UDIM) textures, Ptex per-face textures,
their filtering, and their residency (preloaded or streamed). crust-core decodes
nothing; textures cross the `AssetLoader` seam as samplers owned by
`crust-assets`. The reasoning and measurements behind these requirements are in
`design.md`.
## Requirements
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

### Requirement: UDIM addressing

UV samplers SHALL take unwrapped coordinates and select the tile themselves:
`<UDIM>` expands to `1001 + u + 10·v` and `<UVTILE>` to `u<u+1>_v<v+1>`.

#### Scenario: A lookup in the fourth tile

- **WHEN** a UDIM texture is evaluated at `u = 3.4`, `v = 0.2`
- **THEN** the sample comes from tile 1004 at local `u = 0.4`

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

### Requirement: Ptex

Ptex textures SHALL be addressed by the mesh's authored face index (the base cage for
subdivided meshes).

A `.ptx` that would preload in less than `CRUST_PTEX_STREAM_MIN_MB` SHALL be preloaded,
mip-reduced under `CRUST_PTEX_MAX_LOG2`. A larger one SHALL stream through the reader's
cache by default. `CRUST_PTEX_STREAM=0` SHALL preload every texture.

A streamed texture's mip chain SHALL follow `CRUST_PTEX_STREAM_MIPSPACE`:

- **`capped`** (default): levels finer than the preload cap are read from the file;
  the cap level is the file's level at the cap resolution; coarser levels are reduced
  in linear light from the cap level, by the same reduction the preloaded pyramid
  uses.
- **`file`**: every level is read from the file.
- **`linear`**: a mipmapped `.ptx` SHALL NOT stream, and is preloaded.

Under `capped`, every texel a lookup reads at a resolution the preloaded texture holds
SHALL be bit-identical to the preloaded texel.

#### Scenario: Streaming is the default for a large file

- **WHEN** a render binds a mipmapped `.ptx` larger than `CRUST_PTEX_STREAM_MIN_MB`
  with no Ptex environment variables set
- **THEN** the texture streams, and `--stats` reports it as streamed under the
  `capped` chain

#### Scenario: Small files still preload

- **WHEN** a render binds only `.ptx` files smaller than `CRUST_PTEX_STREAM_MIN_MB`
- **THEN** every texture is preloaded, and the image is bit-identical to a render with
  `CRUST_PTEX_STREAM=0`

#### Scenario: Capped streaming matches the preload where both hold the texels

- **WHEN** a mipmapped `.ptx` whose faces exceed the cap is evaluated streamed under
  `capped` and preloaded, with the mip pyramid on, at footprints no finer than one cap
  texel
- **THEN** every evaluated value is bit-identical

#### Scenario: Capped streaming resolves authored detail

- **WHEN** a lookup's footprint is finer than one cap texel on a face authored above
  the cap
- **THEN** the streamed texture reads a stored file level finer than the cap, where the
  preloaded texture reads its cap level

#### Scenario: Without a pyramid the footprint is ignored

- **WHEN** `CRUST_PTEX_MIP=0` is set and a streamed `capped` texture is evaluated at
  different footprints
- **THEN** every lookup reads the same level: the authored face, or the face at
  `CRUST_PTEX_MAX_LOG2` when that is set

#### Scenario: The linear policy still refuses

- **WHEN** `CRUST_PTEX_STREAM_MIPSPACE=linear` is set and the `.ptx` carries mip
  levels
- **THEN** the texture is preloaded and `--stats` names the reason

### Requirement: Ptex requests carry a colour space

Every Ptex request SHALL carry a colour space, as UV texture requests do. A Ptex file
read as a displacement map SHALL be decoded raw, with no transfer curve. A Ptex file read
as a colour map SHALL keep the gamma-2.2 decode. The preloaded and streamed paths SHALL
agree bit for bit on 8-bit data in either colour space.

#### Scenario: An 8-bit displacement Ptex

- **WHEN** a `u8` `.ptx` holding the value 128 is read as displacement
- **THEN** the lookup returns 128/255, not (128/255)^2.2

#### Scenario: The island's colour Ptex

- **WHEN** a material's `inputs:surfaceMap` Ptex is rendered before and after this change
- **THEN** the images are bit-identical

### Requirement: Streaming holds a bounded number of open files

Streamed UV textures SHALL keep at most `CRUST_TEX_MAX_OPEN_FILES` idle open files
(default 256) across all files. At any moment the open count SHALL NOT exceed that
cap plus the number of render threads. A render thread SHALL never wait on the cap.
`0` SHALL keep every reader open, as before. Images SHALL NOT change with the cap.
Every streamed file SHALL be closed before any output image is written.

#### Scenario: More streamed files than the cap

- **WHEN** a render streams tiles from more `.tx` files than `CRUST_TEX_MAX_OPEN_FILES`
- **THEN** the process's open texture files stay within the cap plus the thread count,
  and `--stats` reports the peak open count, the cap and the number of reopens

#### Scenario: The cap does not change the image

- **WHEN** the same scene is rendered at `-s 16` with `CRUST_TEX_MAX_OPEN_FILES=0` and
  with `CRUST_TEX_MAX_OPEN_FILES=1`
- **THEN** the two EXRs are bit-identical

#### Scenario: The output write after a long textured render

- **WHEN** a render that streamed thousands of `.tx` files finishes
- **THEN** its texture files are closed before the EXR and PNG are written, and the
  write does not fail for lack of file descriptors

### Requirement: Thread-held streamed tiles count against the budget

The tiles of streamed UV textures that render threads hold SHALL count against
`CRUST_TEX_CACHE_MB`:
- All threads together SHALL hold at most half the budget. A thread's capacity SHALL
  shrink to fit that limit when there are many threads or the budget is small, down to
  holding nothing.
- A tile held by both the shared cache and a thread SHALL be counted once.
- A tile the shared cache has evicted while a thread still holds it SHALL count until no
  thread holds it. While such tiles push the total over the budget, the shared cache
  SHALL evict to make room.
- As before, the budget SHALL remain a target that concurrent inserts may exceed
  briefly.

`--stats` SHALL report the peak bytes that threads held after the shared cache evicted
them, beside the shared cache's peak resident bytes and budget. The size of the
per-thread caches SHALL NOT change the image.

#### Scenario: A scene that fits the budget

- **WHEN** every tile a render streams fits within `CRUST_TEX_CACHE_MB`
- **THEN** no tile is evicted, and `--stats` reports no bytes held by threads after
  eviction

#### Scenario: A budget smaller than the working set

- **WHEN** a scene is rendered with `CRUST_TEX_CACHE_MB` below the size of the tiles it
  streams
- **THEN** the shared cache's resident bytes plus the bytes threads still hold after
  eviction stay within the budget, apart from what concurrent inserts briefly add, and
  `--stats` reports both

#### Scenario: The budget does not change the image

- **WHEN** the same streamed scene is rendered at `-s 16` with the default
  `CRUST_TEX_CACHE_MB` and with a budget small enough that no thread can hold a tile
- **THEN** the two EXRs are bit-identical, and the smaller budget's render still
  succeeds

### Requirement: A failed tile read is reported

When a streamed tile cannot be read, the lookup SHALL use the texture's fallback, and
the failure SHALL NOT be silent. If an open fails because the process or system is out
of file descriptors, idle streamed files SHALL be closed and the open retried once
before the read counts as failed. Each failing file SHALL be named once at WARN. If any
tile read failed, the end of the render SHALL log one WARN with the count.

#### Scenario: Descriptors run out during the render

- **WHEN** opening a `.tx` fails with "too many open files" while idle streamed files
  are open
- **THEN** those idle files are closed, the open is retried, and the tile is read with
  no warning

#### Scenario: A file that cannot be read

- **WHEN** a `.tx` file becomes unreadable after the texture was bound
- **THEN** its lookups use the fallback, the file is named in one WARN line however many
  of its tiles fail, and the render ends with one WARN counting the failed tile reads

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

### Requirement: Known gaps

The following SHALL be documented as unsupported: anisotropic filtering,
filtering across Ptex face boundaries, Ptex alpha, `UsdTransform2d`, UV sets
other than `st` and its fallbacks, and an authored colour space for Ptex colour maps:
`half` / `float` samples keep their full range, but every `.ptx` read as colour is
decoded by gamma 2.2, so linear colour Ptex data is mis-decoded. Ptex read as
displacement is decoded raw. Tangents on instanced geometry narrow to prototype
parts placed through an instancer's group and to motion-blurred instances (normal
maps fall back to the geometric normal there); UV charts on subdivided meshes are no
longer a gap.

#### Scenario: A normal map on a directly instanced mesh

- **WHEN** a normal-mapped material is bound to a mesh prim authored twice
- **THEN** it shades with the mapped normal, using a tangent computed at the hit from
  the prototype's vertices and the placement's transform

#### Scenario: A normal map on a prototype placed through an instancer

- **WHEN** a normal-mapped material is bound to a prototype part placed through a
  `PointInstancer` or native-instancing group, or to a motion-blurred instance
- **THEN** it shades with the geometric normal

#### Scenario: A UV texture on a subdivided mesh

- **WHEN** a UV-textured material is bound to a mesh with an authored
  subdivision scheme
- **THEN** the texture is sampled through the refined UV chart rather than
  dropped

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
