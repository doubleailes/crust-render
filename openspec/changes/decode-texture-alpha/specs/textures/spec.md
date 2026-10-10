## ADDED Requirements

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

## MODIFIED Requirements

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
