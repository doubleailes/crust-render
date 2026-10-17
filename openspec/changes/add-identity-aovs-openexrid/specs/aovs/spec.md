## MODIFIED Requirements

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
- `primId` (`id`, `ID`, `Object Index`), `materialId`, `assetId`,
  `instanceId` (`id2`) and `elementId` (`faceindex`): what the first hit is
  (see "Identity and ID mattes").

**Refused as not supported:** the geometric normal `Ng`, refused with a
warning saying it is not supported yet; and the Cryptomatte names
(`crypto_object`, `crypto_material`, `crypto_asset`, `CryptoObject`,
`CryptoMaterial`, `CryptoAsset`), refused with a warning saying crust writes
ID mattes as OpenEXRId instead (see "Identity and ID mattes").

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

#### Scenario: Another renderer's motion-vector name is not an alias

- **WHEN** a RenderVar authors `sourceName = "velocity"` with
  `sourceType = "raw"`
- **THEN** a warning names the var, and the product has no channel for it

#### Scenario: Cryptomatte names are refused

- **WHEN** a RenderVar authors `sourceName = "crypto_object"`
- **THEN** a warning names the var and points at OpenEXRId mattes, and the
  product has no channel for it

## REMOVED Requirements

### Requirement: Identity and ID mattes

**Reason**: It stated the plan and the on-hold refusal. Both halves are now
implemented, and stated as "Identity sources", "Instance and element ids"
and "OpenEXRId ID mattes".

**Migration**: None for scenes: vars that were refused now write channels,
and `deepRaster` products that name ids now write files.

## ADDED Requirements

### Requirement: Identity sources

`primId` SHALL be the MurmurHash3_x86_32 (seed 0) of the hit prim's path, so
a prim has the same id in every frame and run; for instanced geometry the
prim is the instance or `PointInstancer` prim, never a prototype's.
`materialId` SHALL hash the bound material's path, and `assetId` the nearest
`component` or `assembly` ancestor's path, else the prim's. Identity sources
SHALL be closest-sample UINT channels, `-1` written as `0xFFFFFFFF`.

#### Scenario: Stable IDs across frames

- **WHEN** two frames of an animated scene are rendered with `primId`
- **THEN** a given prim has the same ID in both frames

#### Scenario: Ids are never averaged

- **WHEN** a `primId` var authors `driver:parameters:aov:multiSampled = true`
- **THEN** a warning is logged and the channel is still closest-sample

### Requirement: Instance and element ids

`instanceId` SHALL be a hit's placement index within its `PointInstancer`,
and `elementId` the authored (pre-triangulation) face of a mesh the hit lies
in, through subdivision too; both SHALL be `-1` where there is none. Asking
for identity sources SHALL change neither the geometry nor the beauty.

#### Scenario: Instances and faces

- **WHEN** a RenderVar asks for `instanceId` or `elementId`
- **THEN** a hit on a `PointInstancer`'s placement holds its index, a hit on
  a mesh the authored face it lies in, and a hit on anything else `-1`

#### Scenario: The beauty is unchanged

- **WHEN** a subdivided scene is rendered with and without `elementId`
- **THEN** the two beauties are bit-identical

### Requirement: OpenEXRId ID mattes

A `deepRaster` product with a `primId`, `materialId` or `assetId` var SHALL be
written as an [OpenEXRId](https://github.com/MercenariesEngineering/openexrid)
version-3 deep scanline EXR: per pixel one sample per name the camera saw
first, front to back, in the deep "over" form, with `Id` (UINT), `Z` (FLOAT)
and `A` (HALF, coverage by the beauty's pixel filter); with the beauty, also
`R`, `G`, `B` (HALF, its share).

#### Scenario: Coverage sums to the alpha

- **WHEN** a product writes an OpenEXRId matte
- **THEN** in every pixel the coverages of its samples, read back as
  OpenEXRId's readers read them, sum to the beauty's alpha (to 1 where no
  background is visible)

#### Scenario: A coloured matte splits the beauty

- **WHEN** a `deepRaster` product holds `primId` and the beauty
- **THEN** in every pixel geometry covers whole, the colours of its samples
  sum to the beauty

### Requirement: OpenEXRId name table

A matte's header SHALL carry `EXRIdVersion = 3` and `EXRIdNames`: the names
its samples use, sorted, joined by newlines, zlib-compressed and
base64-encoded, `Id` indexing them. A pixel SHALL hold at most 64 names, any
further name's samples going to one entry named `crust:spill`.

#### Scenario: Every id has a name

- **WHEN** a matte is read back
- **THEN** every sample's `Id` indexes a name in its table, and every name in
  the table is some sample's

### Requirement: What a matte holds

A `deepRaster` product SHALL hold one var naming its ids and at most one
beauty var. Any other var SHALL be refused with a warning; a `deepRaster`
product with no var naming its ids SHALL write nothing, with a warning.
Cryptomatte layers SHALL NOT be written.

#### Scenario: A depth var in a matte

- **WHEN** a `deepRaster` product holds `primId` and `depth`
- **THEN** a warning names the `depth` var, and the matte is written without
  it

### Requirement: Primvar sources

A RenderVar with `sourceType = "primvar"` SHALL hold the named primvar's value
at the camera ray's first hit, interpolated as authored, filtered with the
beauty's pixel-filter weights (closest by default for an `int` type), typed
by `dataType` (one to four components). Where the hit surface does not
author the primvar, the channel SHALL hold the var's clear value. A primvar
that does not fit the `dataType` SHALL be refused with a warning.

#### Scenario: Display colour

- **WHEN** a RenderVar authors `sourceName = "displayColor"`,
  `sourceType = "primvar"` and `dataType = "color3f"` on a scene whose meshes
  author `primvars:displayColor`
- **THEN** the channel shows each mesh's display colour, and the clear value
  where the camera sees a surface without it

### Requirement: Primvar lookup

A constant primvar SHALL be inherited from the nearest ancestor that authors
it, and a colour primvar SHALL be converted to the working space. Asking for
primvars SHALL change neither the geometry nor the beauty.

#### Scenario: An inherited constant

- **WHEN** an Xform authors a constant `primvars:displayColor` and its child
  mesh does not
- **THEN** the channel holds the Xform's colour on the mesh
