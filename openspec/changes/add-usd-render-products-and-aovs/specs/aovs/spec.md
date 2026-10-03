## ADDED Requirements

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

**Phase 1 sources:**

- `color` (`Ci`, `C`, `RGBA`, `beauty`, `HdrColor`, `Combined`): the beauty.
- `alpha` (`a`, `A`, `opacity`): filtered geometric coverage of the primary
  ray. A visible dome or sky contributes colour and 0 alpha.
- `depth` (`cameraDepth`, `z`, `Z`, `Depth`): camera-space distance along the
  view axis, in scene units. This is not clip-space.
- `distance`: Euclidean distance from the camera to the first hit.
- `P` (`Pworld`, `__Pworld`, `Position`) and `Peye` (`Pcam`, `__Pcam`):
  first-hit position, in world space and camera space respectively.
- `normal` (`N`, `Nworld`, `__Nworld`, `Normal`) and `Neye` (`Nn`): the
  shading normal, facing the ray, in world space and camera space
  respectively. Values are in [-1, 1].
- `Ng`: the geometric normal, in world space.
- `primvars:st` (`st`, `uv`, `UV`): the first hit's UV.
- `sampleCount`: the samples the pixel took.
- `variance`: the variance of the pixel's luminance mean.

**Later phases:**

- `albedo` (Phase 2).
- `primId`, `instanceId`, `elementId` (Phase 3).
- ID mattes through OpenEXRId (Phase 3, on hold; not Cryptomatte).

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
- depth, distance, position and IDs are closest.

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

*Phase 2.*

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

*Phase 2.*

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

### Requirement: Identity and ID mattes

*Phase 3, on hold: OpenEXRId needs a deep EXR writer, which the `exr` crate
does not have.*

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
