## ADDED Requirements

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
