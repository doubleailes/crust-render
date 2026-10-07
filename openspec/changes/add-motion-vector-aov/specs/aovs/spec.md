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

#### Scenario: Another renderer's motion-vector name is not an alias

- **WHEN** a RenderVar authors `sourceName = "velocity"` with
  `sourceType = "raw"`
- **THEN** a warning names the var, and the product has no channel for it

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

#### Scenario: Motion vectors are never blended across an edge

- **WHEN** a pixel straddles a moving foreground object and a static
  background, and its `motionvector` var authors no accumulation override
- **THEN** the pixel holds either the foreground's vector or `(0, 0)`, never
  a value in between

## ADDED Requirements

### Requirement: Motion vector AOV

The `motionvector` source SHALL be the first hit's forward 2D screen-space
displacement from shutter open to shutter close, in pixels of the rendered
image. `u` SHALL be positive to the right and `v` positive upwards, as Nuke
expects. A hit on geometry authoring `crust:motion:translate` SHALL report
where that translation moves the hit's shutter-open point on screen. Units
SHALL be per shutter interval. The source SHALL have two components and a
clear value of 0.

#### Scenario: Object moving right

- **WHEN** a sphere authors `crust:motion:translate` along the camera's
  right axis, and a `motionvector` var is requested
- **THEN** pixels on the sphere hold a positive `u`, equal to the screen
  distance in pixels between the projections of the hit's shutter-open
  point and that point plus the translation, and a `v` of 0 within
  tolerance where the sphere is centred vertically

#### Scenario: Object moving up

- **WHEN** a sphere authors `crust:motion:translate` along the camera's up
  axis
- **THEN** pixels on the sphere hold a positive `v`

#### Scenario: Static geometry and escapes

- **WHEN** a pixel's chosen sample hits geometry with no
  `crust:motion:translate`, hits a volume, or escapes to a dome or the sky
- **THEN** the pixel holds `(0, 0)`

#### Scenario: Perspective varies the vector across an object

- **WHEN** a long plane, receding from the camera, translates parallel to the
  image plane
- **THEN** its near pixels hold longer vectors than its far pixels

#### Scenario: Wrong component count is refused

- **WHEN** a `motionvector` var authors `dataType = "float"`
- **THEN** a warning names the var, and the product has no channel for it

### Requirement: Motion vectors do not depend on the beauty's blur

A sample's motion vector SHALL be the same whether the beauty is motion
blurred or not. A hit made at a later shutter time SHALL be measured from
where the same point was at shutter open. Turning motion blur off SHALL NOT
remove motion vectors.

#### Scenario: Blurred and sharp renders agree

- **WHEN** a scene with a moving object is rendered once with motion blur
  and once with `disableMotionBlur = true`, both requesting `motionvector`
- **THEN** pixels whose chosen sample hits the moving object's interior in
  both renders hold the same vector, within floating-point tolerance

#### Scenario: Vectors survive disabled blur

- **WHEN** the render settings author `disableMotionBlur = true` and a
  moving object is in view
- **THEN** the beauty shows the object sharp at its shutter-open position,
  and its pixels' `motionvector` is non-zero

### Requirement: Moving points that cross the camera plane

When part of a hit point's path over the shutter lies behind the camera's
near limit, the motion vector SHALL be measured over the part in front of
it only. The visible hit is always on that part. The result SHALL always be
finite.

#### Scenario: Object moving through the camera

- **WHEN** an object in front of the camera translates to a position behind
  it during the shutter
- **THEN** its pixels' `motionvector` values are finite, and point along the
  direction its projection moves before it crosses

#### Scenario: Object arriving from behind the camera

- **WHEN** blur is on and an object translates from behind the camera to in
  front of it during the shutter
- **THEN** pixels where it is hit hold finite vectors, pointing along the
  direction its projection moves once in front
