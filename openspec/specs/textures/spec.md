# textures Specification

## Purpose

Supply texture values to materials: UV (UDIM) textures, Ptex per-face textures,
their filtering, and their residency (preloaded or streamed). crust-core decodes
nothing; textures cross the `AssetLoader` seam as samplers owned by
`crust-assets`. The reasoning and measurements behind these requirements are in
`design.md`.
## Requirements
### Requirement: Colour space travels with the texture

Every texture request SHALL carry the colour space the binding declares, and
lookups SHALL return linear values. For MaterialX an absent `colorspace` means
raw; for `UsdUVTexture` an absent `sourceColorSpace` means `auto` (8-bit RGB /
RGBA is sRGB, anything else raw).

#### Scenario: A greyscale roughness PNG under UsdUVTexture

- **WHEN** a single-channel 8-bit PNG is bound with no `sourceColorSpace`
- **THEN** it is read as raw data, with no transfer curve applied

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
bit-identical. A `.tx` whose recorded colour space differs from the binding
SHALL be refused and the texture preloaded.

#### Scenario: A stray .tx

- **WHEN** a `.tx` cannot be read or is refused
- **THEN** the texture is preloaded from its source and the render still succeeds

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

### Requirement: Known gaps

The following SHALL be documented as unsupported: anisotropic filtering,
filtering across Ptex face boundaries, texture alpha, `UsdTransform2d`, UV sets
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
