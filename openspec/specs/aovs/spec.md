# aovs Specification

## Purpose

The arbitrary output variables (AOVs) crust renders alongside the beauty when a
stage authors UsdRender RenderProducts and RenderVars: raw first-hit data
(depth, position, normals, UVs, alpha, sampling statistics), light path
expression splits of the beauty, and albedo for denoisers. Every AOV is a
by-product of the beauty's own samples and never changes the beauty.

## Requirements

### Requirement: AOVs never change the beauty

Requesting AOVs SHALL NOT change the beauty image. With any set of RenderVars
requested, the beauty SHALL be bit-identical to the same render with no
RenderVars. A render that requests no AOV beyond the beauty SHALL take the
same code path as a render with no products.

#### Scenario: Beauty unchanged by AOVs

- **WHEN** a scene is rendered at `-s 16` once with no products, and once with
  a product requesting `depth`, `normal` and three LPE vars
- **THEN** the beauty channels of the two outputs are bit-identical

#### Scenario: Tiles and scanlines agree for every AOV

- **WHEN** the same AOV request is rendered with and without `--scanline`
- **THEN** every channel of the two outputs is bit-identical

### Requirement: Canonical raw sources and aliases

A RenderVar with `sourceType = "raw"` SHALL be matched by its `sourceName`
(or, if that is empty, by its channel name) against a fixed table of canonical
names and aliases. The table's definitions are listed below; aliases appear in
parentheses.

**Sources:**

- `color` (`Ci`, `C`, `RGBA`, `beauty`, `HdrColor`, `Combined`): the beauty.
- `alpha` (`a`, `A`, `opacity`): filtered geometric coverage of the primary
  ray. A visible dome or sky contributes colour and 0 alpha.
- `depth` (`cameraDepth`, `z`, `Z`, `Depth`): camera-space distance along the
  view axis, in scene units. This is not clip-space.
- `distance` (`DistanceToCameraSD`): Euclidean distance from the camera to the
  first hit.
- `P` (`Pworld`, `__Pworld`, `Position`) and `Peye` (`Pcam`, `__Pcam`):
  first-hit position, in world space and camera space respectively.
- `normal` (`N`, `Nworld`, `__Nworld`, `Normal`) and `Neye` (`Nn`): the
  shading normal, facing the ray, in world space and camera space
  respectively. Values are in [-1, 1].
- `primvars:st` (`st`, `uv`, `UV`): the first hit's UV.
- `sampleCount` (`__sampleCount`): the samples the pixel took.
- `variance` (`crust:variance`): the variance of the pixel's luminance mean.
- `albedo` (`DiffuseAlbedoSD`): the albedo for denoising (see "Albedo for
  denoising").
- `diffuse_albedo` (`DiffuseFilter`, `diffuseFilter`): the diffuse colour of
  the camera ray's first hit (see "Diffuse filter AOV"). It is no longer an
  alias of `albedo`.
- `rawLight` (`RawLighting`, `rawLighting`), `rawGI` (`RawGI`) and
  `rawTotalLight` (`RawTotalLighting`): diffuse light without the surface
  colour (see "Raw light AOVs").
- `motionvector` (no aliases): the first hit's forward 2D screen-space
  displacement over the shutter, in pixels (see "Motion vector AOV").

**Refused as not yet supported:** the geometric normal `Ng`, and the identity
sources `primId` (`id`, `ID`, `Object Index`), `instanceId` (`id2`) and
`elementId` (`faceindex`), and the Cryptomatte names (`crypto_object`,
`crypto_material`, `crypto_asset`, `CryptoObject`, `CryptoMaterial`,
`CryptoAsset`). Each is refused with a warning saying it is not supported yet.
ID mattes are planned through OpenEXRId, not Cryptomatte, in the change
`add-identity-aovs-openexrid` (see "Identity and ID mattes").

A name not in the table SHALL be refused with one warning per var. A refused
var SHALL produce no channel. Data AOVs SHALL keep their clear value where the
primary ray escapes.

#### Scenario: Alias resolves to the canonical source

- **WHEN** a RenderVar authors `sourceName = "Z"` and `dataType = "float"`
- **THEN** the product contains a channel `Z` holding camera-space depth, and
  `+inf` where nothing was hit

#### Scenario: Unknown source is refused, not zero-filled

- **WHEN** a RenderVar authors `sourceName = "diffuse_direct"` with
  `sourceType = "raw"`
- **THEN** a warning names the var, and the product has no channel for it

#### Scenario: World normal in [-1, 1]

- **WHEN** a RenderVar requests `normal` on a sphere facing the camera
- **THEN** the channel holds world-space unit normals whose components span
  negative and positive values, not remapped to [0, 1]

### Requirement: Accumulation modes

Each AOV SHALL be accumulated in one of two modes:

- **filtered:** weighted by the same pixel-filter weights, and normalised by
  the same weight sum, as the beauty;
- **closest:** the value of the sample nearest the camera among the pixel's
  samples that fall inside the pixel's own box.

The default mode SHALL come from the source:

- colour, LPE, alpha, normal, albedo and UV are filtered;
- depth, distance, position, motion vectors and IDs are closest.

The default SHALL be overridden by `driver:parameters:aov:multiSampled`
(true → filtered, false → closest), and then by Arnold `arnold:filter`, Karma
`driver:parameters:aov:filter` or RenderMan `zmin` rules when one of these is
authored. An authored `driver:parameters:aov:clearValue` SHALL replace the
default clear value.

#### Scenario: Depth is never blended across an edge

- **WHEN** a pixel straddles a foreground object at depth 2 and a background
  at depth 10
- **THEN** the `depth` channel holds 2 or 10, never a value in between

#### Scenario: Forcing filtered depth

- **WHEN** a depth var authors `driver:parameters:aov:multiSampled = true`
- **THEN** that pixel's depth is the filter-weighted average of its samples

### Requirement: Light path expression sources

A RenderVar with `sourceType = "lpe"` SHALL be parsed as an OSL Light Path
Expression. An `lpe:` prefix SHALL be stripped first.

**Supported syntax:** event syntax `<type scatter 'label'…>`, the `D`/`L`/
`'label'` shorthands, and the operators `.`, `*`, `+`, `{n}`, `{n,m}`, `{n,}`,
`[…]`, `[^…]`, `(…)` and `|`.

**Events:**

- `C` is the camera.
- `R`/`T` with `D`/`G`/`S`/`s` are surface reflection and transmission,
  classified by lobe (diffuse, glossy, singular, straight).
- `V` is a volume scatter.
- `L` is emission from a light-list entry: an area, dome or distant light, or
  any NEE sample.
- `O` is emission from geometry or volumes that are not lights.

**Labels:** lobes carry the labels `'diffuse'`, `'specular'`, `'coat'`,
`'sheen'`, `'transmission'`, `'subsurface'` and `'translucent'`. Lights carry
their `crust:light:lpeTag`.

**Correctness:** every LPE AOV SHALL be an unbiased estimate of the light it
selects. A set of LPEs that partitions the paths SHALL sum to the beauty
within floating-point tolerance. The expression `C.*[LO]` SHALL reproduce the
beauty bit-for-bit.

**Refused input:** unsupported syntax (RenderMan lobe tokens or prefixes, `!`)
SHALL be refused with a warning that quotes the expression.

#### Scenario: Full-path LPE equals the beauty

- **WHEN** a product requests `sourceName = "C.*[LO]"`, `sourceType = "lpe"`
- **THEN** that channel is bit-identical to the beauty

#### Scenario: Direct and indirect diffuse and glossy partition the beauty

- **WHEN** a product requests `C<RD>[LO]`, `C<RD>.+[LO]`, `C<RG>[LO]`,
  `C<RG>.+[LO]`, `C<T.>.*[LO]`, `C<RS>.*[LO]`, `C<V.>.*[LO]` and `C[LO]` on a
  scene
- **THEN** the per-pixel sum of those channels matches the beauty within
  floating-point tolerance

#### Scenario: Lobe split is unbiased

- **WHEN** an OpenPBR surface with both diffuse and specular lobes is rendered
  with `C<RD>L` and `C<RG>L` at increasing sample counts
- **THEN** each channel converges to the corresponding lobe's reference
  (differences fall as 1/√N and do not plateau)

#### Scenario: Light group

- **WHEN** two lights author `crust:light:lpeTag = "key"` and `"fill"`, and a
  product requests `C.*<L.'key'>`
- **THEN** that channel contains only the key light's contribution

### Requirement: Albedo for denoising

The `albedo` source SHALL be the filtered albedo of the first non-delta hit.
The value SHALL follow perfect-specular chains from the camera, be built from
the lobes' tints and layer weights, and be clamped to [0, 1]. Where the chain
escapes, the value SHALL be the Fresnel-blended albedo of the last delta
interface, or 1 when there is none.

#### Scenario: Albedo through glass

- **WHEN** a textured wall is seen through a thin-walled, zero-roughness glass
  pane
- **THEN** the `albedo` channel shows the wall's texture, scaled by the pane's
  transmission

### Requirement: Diffuse filter AOV

The `diffuse_albedo` source (aliases `DiffuseFilter`, `diffuseFilter`) SHALL be
the diffuse colour of the surface the camera ray hits, after cutouts are passed
through: the sum, over the surface's diffuse reflection lobes, of each lobe's
colour times its weight in the material, with no lighting. It SHALL be
filtered with the beauty's pixel-filter weights, written as colour, and be 0
where the camera ray hits no surface or a surface with no diffuse lobe. It
SHALL NOT follow mirror or glass interfaces: it describes the first hit, the
same surface the raw light AOVs divide by.

`diffuse_albedo` SHALL no longer be an alias of `albedo`, which keeps its own
definition (all lobes, through delta interfaces).

#### Scenario: A textured diffuse wall

- **WHEN** a product asks for `diffuse_albedo` on a diffuse wall with a
  texture in its base colour
- **THEN** the channel shows the texture, independent of how the wall is lit

#### Scenario: Glass in front of the wall

- **WHEN** the camera sees the wall through a clear glass pane
- **THEN** `diffuse_albedo` is the pane's diffuse colour (0 for clear
  glass), while `albedo` shows the wall through the pane

### Requirement: Raw light AOVs

The raw sources SHALL hold diffuse light without the diffuse surface colour:
each camera sample's diffuse light divided, per sample and per channel, by
that sample's diffuse filter (the value `diffuse_albedo` accumulates for the
sample), then filtered with the beauty's weights:

- `rawLight` (aliases `RawLighting`, `rawLighting`): direct diffuse light,
  the paths of `C<RD>[LO]`;
- `rawGI` (alias `RawGI`): indirect diffuse light, the paths of
  `C<RD>.+[LO]`;
- `rawTotalLight` (alias `RawTotalLighting`): both, the paths of
  `C<RD>.*[LO]`.

Where a sample's diffuse filter is below 1e-4 in a channel (black to 8-bit
precision), that channel of the raw sample SHALL be 0: dividing by a
near-black colour would turn noise into fireflies. A raw source SHALL be
colour (`color3f` or `color4f`); a `color4f` raw var carries the beauty's
alpha.

Per camera sample, in every channel where the diffuse filter is at least
1e-4, a raw value times the diffuse filter SHALL equal the matching non-raw
expression's value, to floating-point rounding. Per pixel, the identity SHALL
hold wherever the diffuse filter is constant over the pixel's samples.

#### Scenario: Raw light times the filter is the lighting

- **WHEN** a product asks for `rawLight`, `diffuse_albedo` and `C<RD>[LO]` on
  an untextured diffuse surface
- **THEN** in every pixel covered by the surface, `rawLight × diffuse_albedo`
  equals `C<RD>[LO]` to floating-point rounding

#### Scenario: Raw light ignores the texture

- **WHEN** a uniformly lit wall has a checkerboard base-colour texture
- **THEN** `rawLight` shows no checkerboard inside the checks, while
  `C<RD>[LO]` does

#### Scenario: Nothing diffuse, nothing raw

- **WHEN** a pixel sees only a mirror, glass or the sky
- **THEN** every raw channel of that pixel is 0

### Requirement: Raw modifier on light path expressions

A RenderVar with `sourceType = "lpe"` that authors `bool crust:aov:raw = true`
SHALL be a raw AOV: its expression's value, divided per camera sample by that
sample's diffuse filter, as for the raw sources.

The expression SHALL accept only paths whose first event after the camera is
a diffuse reflection; an expression that can accept any other path SHALL be
refused with a warning naming the var, and no channel SHALL be written for
it.

#### Scenario: A light group's raw diffuse light

- **WHEN** an `lpe` var authors `sourceName = "C<RD>.*<L.'key'>"` and
  `crust:aov:raw = true`
- **THEN** the channel holds the key light's diffuse contribution divided by
  the diffuse filter

#### Scenario: An expression that does not start with a diffuse reflection

- **WHEN** an `lpe` var authors `sourceName = "C.*[LO]"` and
  `crust:aov:raw = true`
- **THEN** a warning names the var, and the product has no channel for it

### Requirement: Identity and ID mattes

*On hold: OpenEXRId needs a deep EXR writer, which the `exr` crate does not
have.*

`primId` SHALL be a stable integer per prim path. ID mattes SHALL be written
as [OpenEXRId](https://github.com/MercenariesEngineering/openexrid) deep EXRs,
not as Cryptomatte layers. Until a deep writer exists, a var asking for an
identity source SHALL be refused with a warning, as today.

#### Scenario: Identity sources are refused while on hold

- **WHEN** a RenderVar asks for `primId` or a Cryptomatte name such as
  `crypto_object`
- **THEN** a warning names the var and no channel is written

#### Scenario: Stable IDs across frames

- **WHEN** two frames of an animated scene are rendered with `primId`
- **THEN** a given prim has the same ID in both frames
