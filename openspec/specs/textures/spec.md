# textures Specification

## Purpose

Supply texture values to materials: UV (UDIM) textures, Ptex per-face textures,
their filtering, and their residency (preloaded or streamed). crust-core decodes
nothing; textures cross the `AssetLoader` seam as samplers owned by
`crust-assets`. The reasoning and measurements behind these requirements are in
`design.md`.
## Requirements
### Requirement: Colour space travels with the texture

Every texture request SHALL carry the colour space the binding declares and the
working space the render is in, and lookups SHALL return linear values in the
working space. A declared space SHALL be converted with its transfer curve and
its change of primaries, as the OCIO config defines them. For MaterialX an
absent `colorspace` (on the input, its node, its nodegraph and the document)
SHALL mean the values are already in the working space, and a non-colour image
SHALL never be converted; for `UsdUVTexture` a `colorSpace` metadatum on
`inputs:file` SHALL win over `sourceColorSpace`, and an absent
`sourceColorSpace` means `auto` (8-bit RGB / RGBA is sRGB, anything else
unconverted). A conversion the renderer cannot split into a per-channel curve
and a matrix SHALL be refused with a warning and the values used as stored.

#### Scenario: A greyscale roughness PNG under UsdUVTexture

- **WHEN** a single-channel 8-bit PNG is bound with no `sourceColorSpace`
- **THEN** it is read as raw data, with no transfer curve applied

#### Scenario: An sRGB albedo rendered in ACEScg

- **WHEN** an 8-bit texture tagged `srgb_texture` is bound in a render whose
  working space is ACEScg
- **THEN** a lookup returns the sRGB-decoded colour converted from Rec.709 to
  AP1 primaries, and a streamed `.tx` of it returns bit-identical values

### Requirement: UDIM addressing

UV samplers SHALL take unwrapped coordinates and select the tile themselves:
`<UDIM>` expands to `1001 + u + 10·v` and `<UVTILE>` to `u<u+1>_v<v+1>`.

#### Scenario: A lookup in the fourth tile

- **WHEN** a UDIM texture is evaluated at `u = 3.4`, `v = 0.2`
- **THEN** the sample comes from tile 1004 at local `u = 0.4`

### Requirement: Filtered minification

Each lookup SHALL receive a footprint width derived from a ray cone, and
textures with a mip pyramid SHALL select levels trilinearly from it. Pyramids
SHALL be reduced in linear light. A width of zero SHALL mean point sampling at
the finest level.

#### Scenario: Ray cones disabled

- **WHEN** a render runs with `CRUST_RAY_CONES=0`
- **THEN** every lookup reads the finest level, bit-identical to a render with
  no mip pyramids

### Requirement: Streaming is an A/B of preloading

A UV texture with a `.tx` beside it SHALL stream through a bounded tile cache
(`CRUST_TEX_CACHE_MB`); `CRUST_TEX_STREAM=0` SHALL preload instead. For 8-bit
textures within the preload cap, streamed and preloaded renders SHALL be
bit-identical, alpha included: a `.tx` converted from a source with alpha SHALL
carry it. A `.tx` whose recorded colour space differs from the binding SHALL be
refused and the texture preloaded.

#### Scenario: A stray .tx

- **WHEN** a `.tx` cannot be read or is refused
- **THEN** the texture is preloaded from its source and the render still succeeds

#### Scenario: A streamed cutout

- **WHEN** an 8-bit RGBA texture is converted with `--auto-tx` and sampled
  streamed and preloaded at any footprint
- **THEN** the two lookups return the same four values, bit for bit

### Requirement: Ptex

Ptex textures SHALL be addressed by the mesh's authored face index (the base
cage for subdivided meshes), preloaded mip-reduced under `CRUST_PTEX_MAX_LOG2`
by default, or streamed through the reader's cache with `CRUST_PTEX_STREAM=1`.
A mipmapped `.ptx` SHALL NOT stream unless `CRUST_PTEX_STREAM_MIPSPACE=file`,
because its stored levels were reduced in the file's encoding.

#### Scenario: Streaming requested on a mipmapped file

- **WHEN** `CRUST_PTEX_STREAM=1` is set without `CRUST_PTEX_STREAM_MIPSPACE=file`
  and the `.ptx` carries mip levels
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
mip levels SHALL average it independently of the colour. A texture's colour
SHALL read the same whether or not its file carries alpha.

#### Scenario: A leaf card's alpha

- **WHEN** an RGBA PNG whose left half has alpha 0 and right half alpha 255 is
  sampled at texel centres
- **THEN** the left half reads alpha 0 and the right half 1, and the colour
  reads what the same image saved without alpha reads

#### Scenario: An sRGB texture's alpha

- **WHEN** an RGBA PNG with alpha 128 everywhere is bound as `srgb_texture`
- **THEN** every lookup, at every footprint, reads alpha 128/255

#### Scenario: A texture without alpha

- **WHEN** an RGB PNG is sampled
- **THEN** its alpha reads 1.0

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
