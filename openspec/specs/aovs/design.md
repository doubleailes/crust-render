# aovs — design record

> Design record for the **aovs** capability: the reasoning, measurements and
> history behind the behaviour `spec.md` states — render products and vars,
> the AOV vocabulary, the film, light path expressions and their routing, the
> albedo. It began as the design of the change `add-usd-render-products-and-aovs`
> (archived 2026-10-03), kept whole: D1–D17 are its decisions, "Phase 1 as
> built" and "Phase 2 as built" what the implementation changed or measured.
> The EXR writer's own record is `image-output/design.md`.
>
> **Status.** Phases 1 (products, first-hit AOVs) and 2 (light path
> expressions, albedo, light groups) are implemented. Phase 3 — identity
> AOVs and ID mattes through OpenEXRId (D14, on hold until crust can write
> deep EXRs), and primvar sources — continues in the change
> `add-identity-aovs-openexrid`. Open question 3 (`diffuse_albedo`) is taken
> up by the change `add-raw-lighting-aovs`.

## Context

### What crust does today

All four points below come from reading the code.

- **Import** (`usd_import/settings.rs`):
  - `render_settings_path` takes the `renderSettingsPrimPath` stage metadatum,
    else `/Render/settings`.
  - From the typed `UsdRenderSettings` schema it reads only `resolution` and
    the first target of `camera`.
  - Everything else comes from `crust:*` custom attributes.
  - `products`, `RenderProduct`, `RenderVar`, `pixelAspectRatio`,
    `aspectRatioConformPolicy`, `dataWindowNDC`, `disableMotionBlur`,
    `disableDepthOfField`, `includedPurposes`, `materialBindingPurposes` and
    `renderingColorSpace` are never read, and no warning is given.
- **Film** (`tracer/mod.rs`):
  - Per-pixel accumulation lives in `PixelState`
    (`sum`, `weight_sum`, `lum_sum`, `lum_sq`, `taken`, adaptive flags), held
    per work unit: a 16×16 tile or a row.
  - Pixel filtering is filter importance sampling (`filter.rs`). Each sample
    belongs to exactly one pixel and carries a weight `wx·wy`, which can be
    negative for Mitchell. Nothing is splatted.
  - A serial gather writes units into a `Buffer` (`Vec<Vec3A>`) in scanline
    order. That is why tiles and scanlines are bit-identical.
  - Per-pixel luminance variance (`var_map`) is computed and then discarded.
  - Guided renders blend whole passes by inverse variance (`blend_passes`).
- **Integrator** (`tracer/path.rs`):
  - `trace_path` returns a bare `Vec3A`.
  - The forward walk records one `VertexRec` per vertex: `atten`,
    `segment_emit`, `emit_here`, `nee`, `factor`, `next_emit`,
    `next_emit_weight`.
  - A backward gather folds the records into the estimate. The primary vertex
    is already split into direct and clamped-indirect parts for
    `--indirect-clamp`.
  - First-hit data (`HitRecord`, `WorldHit{geom_id, prim_id}`, the shading
    normal inside `ShadingPoint`) exists only transiently at vertex 0.
  - No prim path survives import. `LightLinks` keeps a run table from
    `geom_id` to path for emitters only, and drops it at the end of import.
- **Output** (`crust-render/src/main.rs`):
  - `exr::prelude::write_rgb_file` writes one unnamed f32 RGB layer, with the
    crate's default encoding. That default is tiled, and tinyexr crashes on it
    (`docs/material_fidelity.md`).
  - The PNG preview is the beauty, clamped and sRGB-encoded.
  - The only in-repo writer of named channels, half floats and header
    attributes is `crust-assets/src/tiled/exr_write.rs`.

### What `openusd-schemas` 0.7.0 provides

Read from the published crate.

- **Typed views.** `RenderSettings`, `RenderProduct`, `RenderVar`,
  `RenderPass` and `RenderDenoisePass`, the `RenderSettingsBase` trait, and
  token enums `ProductType` and `SourceType`.
- **`compute_render_spec(stage, path, namespaces)`.** A port of
  `UsdRenderComputeSpec`.
- **`compute_namespaced_settings`.**
- **Attribute getters do not apply schema fallbacks.** The global schema
  registry registers no families, so an unauthored attribute reads as `None`.
  The fallbacks exist only as constants inside `compute_render_spec`.
- **No generic prefix query.** There is no "properties in namespace" call.
  `Prim::authored_property_names()` filtered by prefix does the job.

### What the standards say

Research notes are under *Sources* at the end. Tags: **[SPEC]** is the
UsdRender schema; **[HYDRA]** is OpenUSD's imaging layer, a convention;
**[CONV:x]** is renderer x.

- **[SPEC] The schema standardises the graph and resolution, not the
  vocabulary.**
  - The graph is settings → `products` → `orderedVars`.
  - A product inherits `RenderSettingsBase` and overrides only what it
    authors.
  - `RenderVar.dataType` is an Sdf type name; fallback `color3f`.
  - `sourceType` is one of:
    - `raw`: "passed directly to the renderer";
    - `primvar`;
    - `lpe`: "OSL Light Path Expressions … extensions … necessarily
      non-portable";
    - `intrinsic`: "currently unimplemented … future … portable baseline
      RenderVars, such as camera depth".
  - The schema also states:
    - "The name of the RenderVar prim drives the name of the data variable."
    - "USD does not yet enforce a set of universal RenderVar names."
    - With no products, "an application should produce an rgb image according
      to the RenderSettings configuration, to a default display or image name".
- **[HYDRA] The only vocabulary the USD project itself defines.**
  - `HdAovTokens`:
    - `color`, which "should have pre-multiplied alpha";
    - `depth`, defined as clip-space;
    - `cameraDepth`;
    - IDs: `primId`, `instanceId`, `elementId`;
    - geometric: `Peye`, `Neye`, `normal`;
    - prefixes: `primvars:*`, `lpe:*`, `shader:*`.
  - `HdAovDescriptor` has `format`, `multiSampled` and `clearValue`.
  - `multiSampled` means "average" in hdEmbree (last write wins when false).
    It is not formally "unfiltered".
- **[CONV:Houdini]** `driver:parameters:aov:{name, format, multiSampled,
  clearValue, channel_prefix}` on a RenderVar. This comes from Solaris and
  husk, not from pxr core. hdPrman and arnold-usd honour it, and every
  Houdini-exported scene carries it.
- **[CONV:*] `raw` names are not portable.**
  - Beauty is `Ci` (RenderMan), `RGBA` (Arnold), `C`/`beauty` (Karma),
    `color` (Hydra), `HdrColor` (Omniverse) or `Combined` (Blender).
  - Depth means four different things across renderers: clip-space [0,1],
    camera-space z, ray distance, and reversed 1→0.
  - Normals are world-space in Arnold and Karma and camera-space in
    RenderMan `Nn`.
- **[CONV] LPE is the one portable decomposition.**
  - RenderMan, Arnold and Karma accept OSL LPE syntax in `sourceName` with
    `sourceType = "lpe"`. All three agree on the basic subset: `C<RD>L`,
    `C<RD>.+L`, `C<RS>…`, `C<TS>…`, and `<L.'tag'>` light groups.
  - Cycles-Hydra and Omniverse RTX accept no LPE.
  - Extensions are not portable: RenderMan's `U2`, `unoccluded`, and so on;
    Arnold's `A` event; Karma's `'sss'`.
- **[CONV] Data AOVs are not averaged.**
  - RenderMan forces `zmin` on `z`, `id` and integer types.
  - Arnold uses `closest_filter`; Karma uses `["closest",{}]`.
  - Cycles writes depth, ID and position from sample 0 only.
  - Colour and LPE AOVs use the beauty's filter.
- **[CONV:OIDN] Denoiser inputs.**
  - The albedo and normal AOVs must use the beauty's pixel filter.
  - Splatting films are unsupported. Crust's FIS film qualifies.
  - Features "usually work well" taken at the first non-delta hit, following
    perfect-specular chains.
  - Normals may be world-space or view-space, in [-1, 1].
  - Albedo must be in [0, 1].
- **[CONV:EXR / ASWF Color Interop 2026]**
  - "Channel C in layer L is `L.C`".
  - Avoid `R/G/B` suffixes for non-colour data, since they trigger colour
    management.
  - Tag colour with the `colorInteropID` header attribute
    (`lin_rec709_scene` …).
  - Data channels carry no colour tag.
  - Cryptomatte needs float32.

**The conclusion that shapes the design.** There is no single standard for
AOVs. Crust should combine:

1. the **[SPEC]** plumbing, implemented exactly;
2. **OSL LPE** as the light-transport vocabulary;
3. **Hydra's `HdAovTokens`** as the canonical names for geometric data, each
   with a crust definition that is written down and not left implicit;
4. an **alias table** so scenes authored for RenderMan, Arnold and Karma work
   unchanged;
5. the **Houdini `driver:parameters:aov:*`** keys, because that is how real
   files arrive.

## Goals / Non-Goals

**Goals:**

- A scene authored in Solaris (or by hand per the UsdRender docs) asking for
  the usual compositing and denoising AOVs gets them from crust in one EXR per
  product. The channel names should be what Nuke and OIDN expect.
- Every supported source has one written definition: its space, units, clear
  value, and accumulation mode. A var crust cannot honour is refused loudly.
- AOVs never change the beauty, and cost nothing when none are requested.
- Light-transport AOVs are unbiased, and a partition of them sums to the
  beauty.

**Non-Goals:**

- Deep output (`deepRaster`).
- Multiple cameras or resolutions in one render.
- `dataWindowNDC` crop and overscan, `pixelAspectRatio`,
  `aspectRatioConformPolicy`.
- `disableMotionBlur` / `disableDepthOfField`. (`disableMotionBlur` and its
  synonym `instantaneousShutter` were honoured later, by
  `add-motion-vector-aov`; see "Motion vector AOV" below.)
- `includedPurposes` / `materialBindingPurposes`.

These are UsdRender settings worth a separate change ("camera and settings
conformance"). They are listed in Known gaps, not silently dropped.

- `RenderPass`. It is a pipeline concept and is not consumed by renderers.
- `shader:*` / arbitrary shader exports (`sourceType = "raw"` naming an
  OpenPBR or MaterialX output). Possible later, through the MaterialX program.
- An in-process denoiser. OIDN is FFI and `unsafe`, so that is a project
  decision.
- Display drivers and progressive output.
- A CLI for adding AOVs without USD. A follow-up could add `--aov name=source`
  that builds the same request.

## Decisions

### D1. UsdRender is the only way to ask for an AOV

AOVs are requested only through `RenderProduct` / `RenderVar`. There is no
`crust:aovs` attribute and no environment switch.

- **Why.** The schema exists, every DCC exports it, and crust's own fixtures
  already author it. A second, crust-only mechanism would need its own
  resolution rules and would diverge from them.
- **No products.** A stage without `products` keeps today's output exactly:
  RGB EXR at `-o`, plus the PNG. This is the schema's own instruction ("produce
  an rgb image … to a default display or image name"). It also means that, for
  every existing sample, golden and test, the output file is unchanged byte
  for byte.

### D2. Resolve the graph in crust, mirroring `UsdRenderComputeSpec`, at the render's time code

**Rejected: calling `openusd_schemas::render::compute_render_spec`.** It has
three problems:

1. It takes no time code. `productName` is *varying* precisely so that it can
   be time-sampled per frame, which is how Solaris writes `$F4` paths.
2. Its namespaced-settings reader sees only default values, not time samples.
3. Its fallbacks are private constants, and the getters apply none.

**Chosen: a small resolver in `settings.rs` using the typed views**
(`RenderProduct::get`, `RenderVar::get`), evaluated with `eval_time()` like
every other attribute crust reads. It follows `spec.cpp` step by step:

1. Base = the settings prim's `RenderSettingsBase` attributes, with fallbacks.
2. Each product overlays only the attributes it *authors*: `camera`,
   `resolution`, and so on.
3. A product whose camera does not resolve is skipped with a `WARN`.
4. `orderedVars` are followed with `forwarded_targets()`. Order is preserved.
   A var targeted twice in one product is used once.

A test checks that the resolver agrees with `compute_render_spec` on a
time-invariant fixture, so the two cannot drift silently.

**Robustness, from real files:**

- Accept `sourceType` authored as `string` or `token`. The official examples
  use a string.
- Accept `driver:parameters:aov:name` authored as `string` or `token`.
- Treat `instantaneousShutter` as a synonym of `disableMotionBlur` (outside
  this change's scope, but recorded so it is not re-derived; done by
  `add-motion-vector-aov`). Pixar's own `spec.cpp` mis-reads it.
- Accept Houdini's `materialBindingPurposes = ["full","allPurpose"]`, which
  lies outside `allowedTokens`.

**`resolution` default.** Crust's 640×360 default is *not* changed to the
schema's 2048×1080. That would re-size every sample without a `resolution`.
The divergence is noted in Known gaps.

### D3. One render per stage; products share its camera and resolution

The render's camera and resolution are the *first* product's resolved values.
When a product authors no override, these are the settings' values, so current
scenes behave as before. `--camera` still wins over both.

A later product whose resolved camera or resolution differs is refused with a
`WARN` naming the product.

**Rejected:**

- Rendering once per distinct (camera, resolution) group: multiplies render
  time silently.
- Resampling: wrong for data AOVs.

A follow-up can add per-group renders explicitly.

### D4. How each RenderVar becomes a channel

**Channel name.** In priority order:

1. `driver:parameters:aov:name`, as Houdini writes it;
2. otherwise the RenderVar **prim name**, as the schema says.

`driver:parameters:aov:channel_prefix` or `…:aov:husk:channel_prefix`, when
authored, replaces the layer prefix.

**Source.** By `sourceType`:

- `raw`:
  - `sourceName` is looked up in the canonical table (D5), through its
    aliases.
  - An empty `sourceName` falls back to the channel name. This is how Hydra
    1 delegates treat it.
- `lpe`:
  - `sourceName` is parsed as an OSL LPE (D10).
  - An `lpe:` prefix is stripped if present, since Hydra puts it in the AOV
    name.
- `primvar`: `sourceName` names a primvar evaluated at the first hit
  (Phase 3).
- `intrinsic`: refused with a `WARN`. The schema itself calls it
  unimplemented.

**Type.**

- `dataType` gives the component count and kind:
  - `float` / `half` / `int`: 1;
  - `*2*` (`float2`, `texCoord2f` …): 2;
  - `*3*` (`color3f`, `normal3f`, `point3f`, `vector3f`, `float3` …): 3;
  - `*4*` (`color4f`, `float4`): 4.
- `driver:parameters:aov:format` overrides `dataType`; Houdini authors both.
- A type that does not fit the source is refused with a `WARN`. For example,
  `int` for `color`, or `float` for an LPE (LPEs are colour).
- `color4*` on the beauty, or on an LPE, adds the alpha channel (D6).

**Unknown or unsupported** gives one `WARN` per var and no channel.

- **Rejected: writing a black channel.** A black `diffuse_direct` is plausible
  and wrong. This codebase prefers refusing to approximating silently
  (CLAUDE.md, Logging: `WARN` means "something authored was refused").

### D5. The vocabulary: canonical names, aliases and definitions

**This is the "standard" crust adopts.** Hydra's `HdAovTokens` names are
canonical where Hydra defines one. RenderMan, Arnold, Karma and Blender names
are aliases, matched case-sensitively, as Arnold and RenderMan do.

| canonical (`raw`) | aliases | type | accumulation | definition |
|---|---|---|---|---|
| `color` | `Ci`, `C`, `RGBA`, `beauty`, `HdrColor`, `Combined` | color3f / color4f | filtered | The beauty, identical to the EXR crust writes with no products. `color4f` adds alpha. |
| `alpha` | `a`, `A`, `opacity` | float | filtered | Coverage (D6). |
| `depth` | `cameraDepth`, `z`, `Z`, `Depth` | float | closest | **Camera-space depth**: distance from the camera plane along the view axis, in scene units. *Not* Hydra/Storm's clip-space [0,1]; see the trade-off below. Clear `+inf`. |
| `distance` | `DistanceToCameraSD` | float | closest | Euclidean distance from the camera position to the first hit. Clear `+inf`. |
| `P` | `Pworld`, `__Pworld`, `Position` | point3f | closest | World-space first-hit position. Clear `0`. |
| `Peye` | `Pcam`, `__Pcam` | point3f | closest | Camera-space first-hit position (USD camera: looking down −Z, Y up). Clear `0`. |
| `normal` | `N`, `Nworld`, `__Nworld`, `Normal` | normal3f | filtered | World-space **shading** normal (after bump and normal map), facing the camera ray, unit length per sample. Clear `0`. |
| `Neye` | `Nn` | normal3f | filtered | The same normal in camera space. |
| `Ng` | — | normal3f | filtered | World-space geometric normal, facing the ray. |
| `primvars:st` | `st`, `uv`, `UV` | texCoord2f | filtered | The first hit's UV, as the material sees it (`uv_primvar`). |
| `sampleCount` | `__sampleCount` | float | sum (unfiltered) | Samples taken by the pixel. Shows adaptive sampling. |
| `variance` | `crust:variance` | float | per pixel | Variance of the pixel's luminance mean, the quantity adaptive sampling stops on. |
| `albedo` | `diffuse_albedo`*, `DiffuseAlbedoSD` | color3f | filtered | Phase 2. Albedo at the first non-delta hit (D12). |
| `motionvector` | — | float2 / half2 | closest | `add-motion-vector-aov`. The first hit's forward 2D screen-space displacement over the shutter, in pixels, `u` right and `v` up (Arnold's raw `motionvector`, Nuke's `forward.u/.v`). Channels `u`, `v` lowercase. Clear `0`. See "Motion vector AOV" below. |
| `primId` | `id`, `ID`, `Object Index` | int | closest | Phase 3. Stable per-prim integer: hash of the prim path (D14). Clear `-1` (written as u32 `0xFFFFFFFF`). |
| `instanceId` | `id2` | int | closest | Phase 3. Instance index within its instancer, else `-1`. |
| `elementId` | `faceindex` | int | closest | Phase 3. *Authored* face index (pre-triangulation), else `-1`. |
| ID mattes | — | (deep) | coverage-weighted | Phase 3, on hold: OpenEXRId deep EXRs, not Cryptomatte (D14). The Cryptomatte names (`crypto_object` …) stay refused. |

\* `diffuse_albedo` is Arnold's diffuse-only albedo, so it is an alias only
for an OpenPBR surface whose albedo is entirely diffuse. Phase 2 decides
whether it gets its own definition.

**The trade-off on `depth`.**

- Hydra's *spec* says clip-space. Storm and hdEmbree implement it so that
  rasterised overlays can composite.
- The production delegates map Hydra `depth` to their own *camera* depth:
  hdPrman maps `depth`→`z`; Arnold maps `depth`→`Z`.
- Compositors expect a distance (`Z`), not NDC.

Crust follows the production delegates and **documents the divergence** in
the user docs. A scene that really needs clip-space can ask for it with an LPE
on a later `intrinsic` mapping. If this proves wrong, it is one table row.

**Spaces.**

- Camera space is the USD camera's: right-handed, looking down −Z.
- World space is the stage's world after `upAxis` handling, the same frame
  every other crust world-space quantity uses.
- Normals are left in [-1, 1], never remapped to [0, 1]. OIDN wants [-1, 1].

**Where vertex 0 is not a surface.**

- **Escape:** data AOVs keep their clear value; beauty and LPE AOVs get the
  escaped radiance; alpha gets 0.
- **Volume scatter:** depth and P are the scatter point; normal is the
  clear value.
- **Subsurface:** the entry surface owns everything.
- **Cutout pass-through:** resolved before the vertex, as in the beauty.

### D6. Alpha is filtered geometric coverage

Alpha is the filter-weighted fraction of camera samples whose primary ray hits
camera-visible geometry, after cutout pass-through.

- **Domes and the sky.** A visible dome or the sky gradient contributes
  colour but alpha 0. That is the compositing convention (the background is
  not an object), and it matches Hydra's premultiplied `color`, where
  emission with α = 0 is legal ("luminous" pixels).
- **Volumes contribute no alpha in Phase 1.** Alpha is coverage, not
  opacity. A follow-up can define volume alpha as `1 − transmittance` along
  the primary ray. This is listed in Known gaps.
- **Rejected:** OSL's LPE-defined alpha (`!C<Ts>*B`). It needs the inverted
  accumulator and the `B` event. Coverage is what Nuke users expect from `A`.

### D7. Two accumulation modes, chosen per var

**Filtered.** Σ wᵢ·vᵢ / Σ wᵢ, using exactly the beauty's per-sample filter
weight `wx·wy` and the same `weight_sum`.

- It is the default for colour, LPE, alpha, normal, albedo and UV.
- Using the beauty's own weights is what OIDN requires ("same pixel
  reconstruction filter as the beauty"). It also makes LPE sums exact (D11).
- Mitchell's negative lobes can push albedo slightly outside [0, 1], or
  shorten normals. Normals are left as they are (OIDN accepts any length).
  Albedo is clamped to [0, 1] on write, and the clamp is documented.

**Closest.** The value of the sample with the smallest camera-space depth
among the pixel's samples.

- It is the default for depth, distance, P, Peye and IDs.
- It is the RenderMan `zmin` rule, and the same as Arnold `closest_filter` and
  Karma `closest`.
- It is deterministic for a given sample set. It never blends two objects'
  IDs or positions.

**The trap: the default triangle filter has radius 1.** Filter importance
sampling places samples up to one pixel outside the pixel's box. "Closest"
would then pick up a neighbour's foreground object, which gives a fattened
depth and ID matte. So:

- "Closest" considers only samples whose film offset lies inside the pixel's
  own `[-0.5, 0.5]²` box.
- If a pixel has none (possible at very low spp with a wide filter), it falls
  back to the sample nearest the pixel centre.
- With the box filter at radius 0.5 this is all samples.

**Mapping from authored attributes**, first match wins:

1. `driver:parameters:aov:multiSampled`: true → filtered, false → closest.
   This is Hydra's flag; hdEmbree reads false as no averaging.
2. Renderer filter attributes:
   - Arnold `arnold:filter`: `closest_filter` → closest, anything else →
     filtered;
   - Karma `driver:parameters:aov:filter` starting `["closest"`;
   - RenderMan `ri:accumulationRule` (or `ri:displayChannel:filter`) `zmin`
     → closest.

   Other values (`zmax`, `min`, `max`, `average`, `sum`) are refused with a
   `WARN`, falling back to the default.
3. The source's default from D5.

**Clear value.**

- `driver:parameters:aov:clearValue` wins when authored. Houdini authors `0`
  for everything, including depth.
- Otherwise the D5 clear value applies.

**Rejected:**

- **Sample 0 only (Cycles).** It is the noisiest choice, and arbitrary under
  adaptive sampling.
- **"Last write" (hdEmbree, `multiSampled = false`).** It depends on sample
  order. Crust's sample order is deterministic, so it would be stable, but it
  is meaningless.

### D8. Film storage, and the zero-AOV guarantee

**`trace_path` gains a const generic `AOV: bool`, like `PROFILE`.**

- `AOV = false` is the existing function, instruction for instruction.
- `AOV = true` additionally fills a `FirstHit` record (position, depth,
  normals, UV, geom/prim id; written once at vertex 0) and the masked-gather
  inputs (D11) into `PathScratch`.
- Monomorphisation keeps the branch out of the common binary path. The
  comments at `path.rs` about inlining sensitivity apply: the AOV variant is a
  separate instantiation, so it cannot perturb the non-AOV one's inlining.

**Per-unit SoA accumulators sit beside `PixelState`, not inside it.**

- They are one `Vec<f32>` plane per *component*, sized to the unit.
- `PixelState` stays the size it is. The adaptive loop's cache footprint is
  untouched when AOVs are off.
- The serial gather (`mod.rs`, scanline order) copies unit planes into
  full-frame planes, so **tiles and scanlines stay bit-identical for every
  AOV**, by the same argument as for the beauty.

**The engine API.**

- `render_with_stats` keeps its signature.
- A new `render_with_aovs(…) -> (Buffer, AovFilm, RayStats)` returns the
  beauty `Buffer` exactly as before, plus an `AovFilm`: named planes with a
  type and a colour/data tag.
- The CLI calls the new entry point only when a product asks for something
  beyond the beauty.

**Guarantees, each pinned by a test:**

- With no products, or with a single `color3f` beauty var, the beauty buffer
  is bit-identical (`check_images.sh check`). The callgrind instruction count
  on cornellbox at `-s 2` does not change.
- With any set of AOVs, the beauty buffer is bit-identical to the no-AOV
  render. AOVs observe; they never consume a QMC dimension, and never change
  a weight.

### D9. Interactions with adaptive sampling, guiding and the firefly clamp

- **Adaptive sampling** stays driven by the beauty's luminance alone.
  - Every AOV sees exactly the samples the beauty took, which is what OIDN
    and compositing expect.
  - Driving the stop test with AOV variance (RenderMan can) is out of scope.
  - The `variance` AOV *is* the beauty's stopping statistic, so the user can
    see why a pixel stopped.
- **Guided renders** (`render_guided`, several passes blended by inverse
  variance):
  - Filtered AOVs blend with **the same per-pass weights as the beauty**.
    Blending is linear, so an LPE partition still sums to the blended beauty.
  - Closest AOVs take the closest across passes.
  - `sampleCount` sums.
  - `variance` is recomputed from the blend weights:
    Var(Σ wₚ xₚ) = Σ wₚ² Var(xₚ).
- **`--indirect-clamp`** scales the primary vertex's indirect continuation by
  `limit / peak` (`clamp_indirect`).
  - Every LPE AOV's continuation at vertex 0 is scaled by **the same factor,
    computed from the beauty's continuation**. Otherwise the partition would
    not sum to the clamped beauty.
  - The factor is computed once in the beauty gather and reused in the masked
    gathers.
  - This is one more "pair that must change together" entry for
    `docs/architecture.md` § Invariants.

### D10. LPE: the OSL grammar over a crust event alphabet

**Grammar.** The OSL LPE grammar exactly, so expressions copied from
Karma/Arnold/RenderMan docs parse:

- events `<type scatter 'label'…>`;
- the shorthands `D` = `<.D>`, `L` = `<L.>`, `'x'` = `<..'x'>`;
- `.`, `*`, `+`, `{n}`, `{n,m}`, `{n,}`, `[…]`, `[^…]`, `(…)`, `|`.

No `?`, since OSL has none. Not supported in Phase 2: RenderMan's lobe tokens
(`D1`…`U12`), prefixes (`unoccluded`, `shadows`, `holdouts` …) and
`!`-inversion. Each is a parse error with a `WARN` that names the token.

**Event alphabet**, mapping crust's transport onto OSL's:

| event | crust source |
|---|---|
| `C` | the camera (implicit start) |
| `R`/`T` + `D` | OpenPBR `Diffuse` and closure `Diffuse` → `RD`; closure `Translucent` → `TD`; subsurface walk entry → `TD` (the whole walk counts as this one event) |
| `R`/`T` + `G` | rough GGX lobes: OpenPBR `Specular`, `Coat` → `RG`; continuous rough transmission → `TG`; `Sheen`/OpenPBR `Fuzz` → `RG` |
| `R`/`T` + `S` | a lobe at zero roughness, a delta sample (`ScatterSample.delta`): thin-walled transmission → `TS`, mirror → `RS` |
| `T` + `s` | straight pass-through: cutout opacity (`pass_cutouts`), and thin-walled transmission at IOR 1 |
| `V` + `.` | a volume-region scatter (`VolumeEvent::Scatter`) or a medium scatter |
| `L` | emission from a **light-list entry**: area-light geometry hit by a bounce or from the camera, an infinite light (dome, distant) on escape, and every NEE sample |
| `O` | emission from **geometry that is not a light-list entry**: an emissive material reached only through `emitted_at`, and volume emission (`segment_emit`) |

The `L` / `O` split is not invented. CLAUDE.md already states it as an
invariant: "A material that emits only through `emitted_at` must never become a
light-list entry." OSL's `L` versus `O` is exactly that distinction.

There is no `B` event. UsdLux makes the dome a light (`L`), and Karma and
RenderMan treat it the same way. Arnold's `B` "visible background" is not
portable. Crust's sky gradient (procedural fallback) is `L` as well.

**Labels.** Every lobe carries its OpenPBR component name: `'diffuse'`,
`'specular'`, `'coat'`, `'sheen'` (OpenPBR fuzz), `'transmission'`,
`'subsurface'`, `'translucent'`. These are the names Arnold's built-in LPEs
use, so `C<RG'coat'>L` and `C<RS[^'coat']>L` work. A light's label is its tag
(D13).

**Compilation.**

- All LPE vars of the render compile into **one** DFA, as OSL does.
- Each accepting state holds a bitmask of the AOVs that accept there, capped
  at 64 LPE vars per render, with a `WARN` beyond.
- A path carries one `u16` DFA state. Each event is one table lookup, with the
  label set folded into a small symbol index at import.

### D11. Routing contributions exactly under the one-sample-mixture BSDF

**This is the decision most likely to be got wrong.**

Crust's OpenPBR and MaterialX closures pick a lobe with `LobePmf::pick`. A
continuous sample then carries the **full-mixture** value and density:
`value = eval_all(ω)·cos`, `pdf = pdf_all(ω)` (`openpbr/mod.rs`). The textbook
shortcut, "label the bounce with the lobe that was picked", is therefore
**biased per AOV** in crust.

- A diffuse-picked direction's weight `f_all/p_all` includes the specular
  lobe's value toward that direction.
- So `C<RD>.+L` would contain specular energy and `C<RG>.+L` would miss it.
  The sum would still equal the beauty, which hides the bug.
- Renderers that label by picked lobe (Arnold) use a *per-lobe* estimator
  `f_j / (p_j·P(j))` instead. Switching crust to that would change the beauty's
  variance, so it is rejected because of D8.

**Chosen: per-lobe splitting with a per-DFA-state backward gather.**

1. At a vertex, evaluate each live lobe's value toward the direction
   separately. `eval_all` already sums them, so this returns the summands
   rather than adding new work:
   - for the **NEE** direction: `nee_j`, one per lobe;
   - for the **bounce** direction: `f_j`, with `Σ_j f_j = f_all`.
2. Each lobe j advances the path's DFA state by its event, giving `s_j`.
3. Backward gather, per AOV `a` and per reachable state `s` at vertex k:

   `R_a(k, s) = emit_a(k, s) + atten_k · (nee_a(k, s) + Σ_j (f_j·cos/pdf)·(next_emit_a(k+1, s_j)·w + R_a(k+1, s_j)))`

   Each `_a(…, s)` term is the contribution if the event from `s` reaches an
   accepting state containing `a`, else zero.
4. Delta samples (thin transmission, mirror, subsurface entry) come from one
   lobe only. They advance a single state and need no split.

**Properties:**

- **Unbiased per AOV.** It is the beauty estimator with each lobe's share
  routed by its own label.
- **Exact partition per sample.** The per-lobe shares sum to the full value,
  so any partition of LPEs sums to the beauty up to floating-point
  associativity.
- **Bit-identical full-path LPE.** For `C.*[LO]`, every lobe transitions to
  the same state. The gather collapses to the beauty's expression, evaluated
  in the same order. A test pins `C.*[LO]` == beauty bitwise, in the
  repo's bit-identity tradition.
- **Cheap when the LPE does not distinguish the lobes.** If all lobes at a
  vertex map `s` to the same state (the common case), the split is skipped
  and `f_all` is used directly. A DFA-state equality check decides this.
- **Cost.** O(depth × reachable states × lobes). Reachable states per vertex
  are few: a typical set of compositing LPEs compiles to under 20 states.
  This is measured, not assumed (task 7.7).

**Rejected:**

- **Label by picked lobe:** biased, as shown above.
- **Stochastic routing with a posterior correction**
  `(f_j/f_all)·p_all/(pmf_j·p_j)`: unbiased, but the partition no longer sums
  to the beauty per sample, which adds noise to every split AOV.
- **A forward per-state throughput vector:** equivalent, but it duplicates
  the backward gather instead of reusing it, and fights the clamp in D9.

**MIS is untouched.** Routing only partitions contributions. NEE and bounce
keep their existing weights (`next_emit_weight`, `bounce_emission_weight`), so
an NEE share and its bounce-side twin land in the same AOV. Both see the same
lobe label and the same `L`/`O` event, which keeps CLAUDE.md's NEE ↔ bounce
pair intact.

**Guiding.** A guided direction carries `f_all` over the mixture density. The
split is on `f`, which does not depend on how ω was sampled, so it applies
unchanged.

### D12. Albedo

Albedo is the filtered value at the **first non-delta hit**, following
perfect-specular (`ScatterSample.delta`) chains from the camera. This is
OIDN's recommendation and what Cycles does. At the stopping vertex:

- **Value:** Σ over lobes of the lobe's *tint* times its layer weight, using
  what the material already knows:
  - diffuse `base_color·base_weight·(1−metalness)…`;
  - specular F0/F82 tint;
  - coat tint;
  - sheen colour.

  Clamped to [0, 1].
- **Rejected: a directional-albedo integral.** It is exact but needs tables or
  extra samples. OIDN only needs a noise-free feature that tracks texture.
- **A chain that ends in an escape:** the albedo of the last delta interface
  (Fresnel-blended), or 1 when there is none. This is OIDN's rule for a
  camera-visible glass whose background is the dome.
- **Materials with no lobe information:** `UsdPreviewSurface` reaches OpenPBR
  through the adapter, so it has lobes. A `Material` that does not override
  `albedo()` returns 1 (OIDN's documented fallback), and a `DEBUG` line
  counts such materials.

### D13. Light groups

A light group is an LPE custom label on the `L` event. A light declares its
tag with `token crust:light:lpeTag` (the existing `crust:light:*` namespace).
As with `crust:light:cameraVisible` (which falls back to RenderMan's primvar),
other renderers' tag attributes are read as fallbacks, in this order:

1. Karma's `karma:light:lpetag`;
2. RenderMan's light-group attribute;
3. Arnold's `primvars:arnold:aov`.

The exact attribute names and prefixes are to be confirmed against a real
Solaris and RenderMan export before they are hard-coded. This is an open
question.

The user writes `C.*<L.'key'>`. Automatic per-tag splitting (Karma's
`C_key`) is not done; the user asks for each group with its own var.

### D14. Identity and ID mattes (Phase 3) — ID mattes on hold

**A prim-path table.** Import keeps a compact `geom_id → interned prim path`
table for every geometry prim, not only for emitters.

- It extends `LightLinks`' run table, which already records `(first geom_id,
  path)` runs.
- It is kept in `World` only when an identity AOV is requested. Otherwise it
  is dropped at the end of import, as today.
- For an instanced prototype, the instance prim's path is used. This is the
  same rule light linking uses, so the epoch rule applies (`ImportCaches::
  epoch`).

**`primId`.** `primId` is the MurmurHash3 of the prim path, reinterpreted as
an int.

- It is stable across frames and runs, unlike a `geom_id`, which depends on
  traversal order.
- **Rejected: Hydra's dense index.** Compositors key mattes across frames, so
  stability matters more than density.

**ID mattes: OpenEXRId, not Cryptomatte.** The project chose
[OpenEXRId](https://github.com/MercenariesEngineering/openexrid) over
Cryptomatte v1.2 for ID mattes. OpenEXRId stores mattes as a *deep* EXR:
each pixel holds its own list of samples, each with an object id and its
coverage, and the names the ids stand for travel in the file.

- **On hold.** The `exr` crate (1.74) reads and writes flat images only; its
  README lists deep data as not yet supported. Nothing else here is blocked.
- **Unblocking it** means one of: deep scanline writing landing in `exr`
  (upstream contribution), or a deep scanline writer in-tree in safe Rust
  (the OpenEXR deep format is documented). A C++/FFI writer would be
  `unsafe`, which is a project decision.
- **What carries over** from this record: the prim-path table and the
  stable per-path `primId` above, which name the ids; coverage weighted by
  the beauty's pixel filter, as for every filtered AOV; cutout pass-through
  contributing to the surfaces behind it. The exact channel and metadata
  layout is OpenEXRId's, to be taken from its specification when the work
  resumes — not designed here.
- **Superseded:** the Cryptomatte plan (MurmurHash3 names, ranked
  `<name>NN.rgba` layers, header manifests) is dropped, not deferred.

**`primvar` sources.** Primvars named by `sourceType = "primvar"` vars are
added to a "keep" set before the meshes load. `import_render_settings` already
runs first, on the index stage. They are stored per face-varying or vertex
like `st`, and evaluated at the first hit.

### D15. The EXR writer

**Layout.**

- **One part, scanline, ZIP compression (16 scanlines).**
  - **Rejected: multi-part.** Reader support is weaker, and nothing here
    needs per-part compression.
  - **Rejected: tiled.** tinyexr crashes on it. The FLIP wrapper already has
    to rewrite crust's tiled EXRs (`docs/material_fidelity.md`).
- **Channels are built with `exr`'s `AnyChannels::sort`**, which sorts them
  alphabetically, as `exr_write.rs` already notes. Channel names are as
  follows:
  - **Colour:** `<layer>.R/.G/.B[/.A]`.
  - **The beauty is unprefixed.** The first var in the product that resolves
    to the beauty writes bare `R/G/B[/A]`, so every viewer shows it as the
    image, as Nuke expects. An authored `channel_prefix` overrides this.
  - **Data vectors:** `<layer>.X/.Y/.Z`; UV uses `.U/.V`. This follows the
    ASWF Color Interop rule: no `R/G/B` on data.
  - **Scalars:** a single channel named after the layer (`Z`, `depth`,
    `sampleCount`). A var named `Z` therefore gives the conventional `Z`
    channel.
- **Precision:**
  - `half*`/`color3h` → HALF;
  - `float*`/`color3f` → FLOAT;
  - `int` → UINT, with `-1` written as its two's-complement bit pattern.

  Data AOVs default to FLOAT. (ID mattes are deep, OpenEXRId's own layout:
  D14.)
- **Header:**
  - `software` = `crust-render <version>`;
  - `colorInteropID` = `lin_rec709_scene`, crust's rendering space
    (`docs/color_management.md`), applying to the colour channels;
  - `driver:parameters:OpenEXR:*` / `driver:parameters:artist|comment` text
    attributes copied through.

  An authored `renderingColorSpace` other than Rec.709 linear gives a `WARN`.
  Crust renders in one space; the colour-management change owns that.

**Paths.** `productName` is used as authored. A relative name resolves against
the working directory, as husk and usdrecord do. Parent directories are
created.

**PNG preview.** Written from the first product's beauty, beside that
product's file. A product with no beauty var gets no PNG (`DEBUG` line).

**No products.** The no-products case keeps calling `write_rgb_file` unchanged.
The new writer is not used, so the existing EXR stays byte-identical. Moving
the no-products case to the new writer, which gives a scanline encoding and a
`colorInteropID` tag, would be a deliberate, separately-goldened switch. It is
listed under Follow-ups.

### D16. CLI

`-o/--output` becomes optional. Exactly one of these applies:

- **Products authored, `-o` given:** `-o` replaces the first product's
  `productName`, as husk's `-o` does. Other products keep their names.
- **Products authored, no `-o`:** each product goes to its `productName`.
- **No products:** `-o`, defaulting to `output.exr`. This is today's
  behaviour.

No new flag in this change. One `INFO` line lists the products written, once
per render, which stays within the bounded-INFO rule.

### D17. Logging

Each refused var, product or attribute gets **one `WARN` per render**, naming
the prim path and the reason:

- unknown source;
- unsupported `sourceType`;
- type mismatch;
- `deepRaster`;
- a product whose camera or resolution differs;
- an LPE parse error, quoting the expression and the column.

Product and var resolution details are `DEBUG`. Nothing is per pixel or per
sample.

## Risks / Trade-offs

- **[Risk] The hot loop slows when AOVs are off.**
  → The const-generic instantiation (D8) and the zero-instruction-delta gate:
  callgrind on cornellbox, plus `bench_ab.sh` on the scene set.
- **[Risk] The LPE gather cost explodes with many vars.**
  → One shared DFA, the "lobes agree" fast path, and a cap of 64 LPE vars.
  The cost is measured per added LPE on cornellbox and on a Moana subset, and
  recorded in the `aovs` design record.
- **[Risk] Memory at 4K with many AOVs** (about 1 GB for 30 float channels).
  → Planes are allocated only for requested components. Half vars could be
  stored as f32 during accumulation and converted at write. A follow-up could
  stream the gather straight to disk.
- **[Trade-off] `depth` departs from Hydra's clip-space definition.** It is
  documented, and follows hdPrman and Arnold.
- **[Trade-off] A closest-mode AOV ignores samples outside the pixel box.**
  At 1–2 spp with a wide filter, the fallback (nearest to centre) is noisy.
  This is acceptable for data passes.
- **[Risk] Lobe labelling of MaterialX closures that are neither OpenPBR nor
  standard.** `closure::Lobe` covers every leaf crust can resolve (Diffuse,
  Specular, Sheen, Translucent, Subsurface), so the mapping is total. A test
  enumerates the variants so a new lobe cannot be added unlabelled.
- **[Risk] Alias collisions**: a user's custom var named like an alias, for
  example `diffuse`.
  → Aliases apply only to `sourceType = "raw"`, and only to names in the
  table. Anything else is "unknown" and warned. Arnold's built-in LPE names
  (`diffuse_direct` …) are deliberately *not* aliased, since their
  definitions differ subtly. Users ask for an LPE.

## Migration Plan

- **No products authored:** nothing changes.
- **Scenes that author products today** (the `material_fidelity` fixtures):
  crust starts writing to `productName` instead of `-o`, unless `-o` is
  passed. `scripts/material_fidelity/run.py` already passes the output path,
  so it is unaffected. This is checked in task 4.4.
- **Rollback:** the phases land separately. Reverting Phase 2 or 3 leaves
  Phase 1 working.

## Follow-ups

- Camera and settings conformance: `pixelAspectRatio`,
  `aspectRatioConformPolicy`, `disableDepthOfField`, `includedPurposes`,
  `materialBindingPurposes`. (`disableMotionBlur` / `instantaneousShutter`
  are done: "Motion vector AOV" below. `dataWindowNDC` is done by
  `add-render-region`: every product of a cropped render is written with the
  frame as display window and the region as data window; overscan is not.)
- One render per (camera, resolution) group.
- `deepRaster` products. Crust's FIS film would need per-sample depth lists.
- Volume alpha and `C<V.>` volume AOVs with a `ZBack`.
- Moving the no-products EXR to the new writer (scanline, tagged), as a
  goldened switch.
- A `--aov` CLI flag that builds the same request without USD.
- Shader-export AOVs from MaterialX outputs (`shader:*`).
- AOV-driven adaptive stopping. An in-process denoiser (a project decision).

## Open Questions

1. **The `depth` clear value.** `+inf` (chosen; good for `zmin` compositing)
   versus `0` (hdEmbree `cameraDepth`, and Houdini's authored `clearValue`)
   versus `1e10` (Cycles). An authored `clearValue` already wins, so this only
   affects hand-written scenes.
2. **The light-group fallback attribute names** for Karma, RenderMan and
   Arnold (D13). They need verifying against real exports before they are
   hard-coded.
3. **Whether `diffuse_albedo` gets its own diffuse-only definition** or stays
   an alias of `albedo`.
4. **Whether `alpha` should include volume opacity in Phase 1** rather than
   later (D6).

## Phase 1 as built

Where the implementation departs from, or sharpens, the decisions above.

- **The request lives on `Scene`, not `RenderSettings`** (`Scene::aovs`,
  an `AovRequest`). `RenderSettings` is `Copy` and is copied through every
  builder method; a list of products cannot be. The resolver is its own
  sibling, `usd_import/products.rs`, and the vocabulary is `crust-core/src/aov.rs`.
- **`Ng` is refused, not implemented.** The kernel's `RayHit` carries one
  normal — the interpolated one where a mesh has normals — so a geometric
  normal would mean widening the kernel's hit record (a `Tri4` bit-identity
  pair) for one AOV. It is in the "not supported yet" list with `albedo` and
  the IDs.
- **`normal` is the shading point's normal** (`ShadingPoint::normal`, the
  record `Material::resolve` returned). UsdPreviewSurface and OpenPBR apply
  their normal maps there; a MaterialX closure applies its `normal` input per
  leaf (`closure::prepare`), after resolve, so for those the AOV is the
  interpolated normal. Recorded in Known gaps.
- **The zero-AOV gate had a trap.** The first version passed the pixel index
  and the AOV planes as new `advance_pixel` arguments and branched on the film
  per pixel: +3.6M instructions on cornellbox at 2 spp (+0.086%, against a
  run-to-run spread of ±0.2M), all inside `advance_pixel::<false, false>`'s
  sample loop — the extra arguments changed the loop's codegen, not its work.
  `advance_pixel` now has exactly its old signature; the pixel index and the
  camera frame travel in the unit's `UnitAov`, and the film branch is taken
  once per work unit (a `macro_rules!` stamps each unit loop for both `AOV`
  values). Measured after: 4,178.84M–4,178.89M against the base's
  4,179.76M–4,179.96M, and all 33 goldens bit-identical.
- **Guided renders are not repeatable** with two or more training
  iterations, before and after this change: whether the final pass is guided
  depends on the wall-clock efficiency estimate `ΔEff`. The bit-identity
  tests for guided AOVs therefore use one training iteration, which skips the
  estimate. Not an AOV issue, but any future guided golden needs the same.
- **The no-products EXR is not byte-repeatable even before this change**
  (the `exr` crate writes parallel-compressed blocks in completion order).
  Task 3.5's "byte-identical" is checked as identical pixels plus an
  identical PNG; see `openspec/specs/image-output/design.md`.
- **`exr_diff` compares every channel bitwise** (so equal infinities in a
  depth pass match), and keeps its first output line's format for
  `check_images.sh`.

## Phase 2 as built

Light path expressions (`crust-core/src/lpe/`), the per-lobe routing
(`tracer/route.rs`), the albedo and light groups. Where it departs from, or
sharpens, D10–D13:

- **"The lobes agree" is per expression, not per DFA state.** Every
  expression of a render shares one DFA, so a diffuse and a glossy lobe lead
  to different states as soon as any expression tells them apart — and the
  first build, testing state equality, put `C.*[LO]` on the per-lobe sum and
  lost bit-identity. The DFA now carries, per expression, Moore's partition
  of its states (`Lpe::class`); lobes agree for an expression when their
  states share its class, and states in one class compute that expression's
  radiance identically, so the beauty's totals can stand in for the sum.
- **The bounce is split at the local direction drawn.** Re-evaluating the
  world direction a sample returns moves it by an ulp through the frame's
  round trip, and a lobe at zero roughness (GGX α = 1e-4) changes its value
  by 0.2% under that: the design's partition missed the beauty by 0.18% on a
  coated ball. `OpenPBR::scatter_with::<SPLIT>` splits as it samples (the
  `false` instantiation is `scatter_resolved`, instruction for instruction);
  a MaterialX closure splits at the ray's own direction, which it stores
  exactly as it evaluated it. NEE needs neither: the beauty's `eval` and the
  split take the same world direction.
- **Singular is α ≤ 1e-3** (roughness ≈ 0.03), not exactly zero: crust
  floors GGX α at 1e-4, so nothing is exactly a mirror.
- **MaterialX coats are found by shape.** A leaf knows its BSDF, not which
  surface component it came from; a reflecting interface layered over
  another reflecting interface is labelled `'coat'` (`standard_surface` and
  `open_pbr_surface` both expand that way). A reflecting specular over a
  transmission-only interface stays `'specular'`.
- **Cutouts are `Ts` events** on the segment they are passed on (counted from
  `stats.cutout_passes`, so `pass_cutouts` is untouched). Shadow rays through
  a cutout add none: NEE's visibility is not an event in OSL either.
- **A subsurface walk is one `TD 'subsurface'` event**; its exit vertex adds
  none (its NEE and bounce pass the state through).
- **The escape is split per light** by `escaped_split`, and with
  expressions on the beauty's background *is* the split's sum — the same
  additions in the same order, asserted bitwise in debug builds — so the
  lights at infinity are not evaluated twice.
- **Expressions are bounded where they are accepted** (review of #199). An
  expression's bounded repeats may unroll to at most 4096 events (nesting
  multiplies them: `(((.{32}){32}){32}){32}` is a million), the render's
  DFA to at most `lpe::MAX_STATES` = 4096 states (`.*x.{16}` alone needs
  ~2¹⁷), and its alphabet must fit a `u16` below the router's "no event".
  The importer compiles the render's accepted set with each new expression
  and refuses one that breaks a bound, with one `WARN` — so `Lpe::compile`
  returns a `CompileError` instead of asserting, and the renderer's compile
  cannot fail on an imported scene. Only labels an expression names enter
  the alphabet: a light tag nothing names reads as no tag, which no
  expression could tell apart, so the scene's tag count no longer sizes it.
- **Light groups read `crust:light:lpeTag` only.** Karma's, RenderMan's and
  Arnold's attributes stay unread until their names are checked against real
  exports (open question 2). Backdrops carry no tag.
- **Albedo** (D12): the chain multiplies each delta interface's
  `value / pdf`, clamped to [0, 1] per step; a volume scatter before any
  surface reports 1; a `Material` queried directly (not OpenPBR, not a
  closure) reports 1. No `DEBUG` count of such materials was added: which
  materials take that path is only known per hit.
- **Strategy agreement holds for lobes NEE can resolve.** Light-only and
  BSDF-only agree per expression to ~1% at 2048 spp where every lobe is
  rough; on a coat at zero roughness light-only is unbiased but renders it
  black at any affordable sample count (it differs from BSDF-only by 7% in
  the beauty itself, before any routing), so the test uses rough lobes.

**Cost** (task 7.6). Instruction counts, cornellbox at `-s 2`, one thread;
render = `advance_pixel` inclusive:

| request | render | vs beauty | per added expression |
|---|---|---|---|
| no products | 3,686M | — | |
| `N` only | 3,813M | +3.5% | |
| 1 LPE | 4,491M | +21.8% | |
| 4 LPEs | 4,971M | +34.9% | +160M (+4.3%) |
| 8 LPEs | 5,326M | +44.5% | +89M (+2.4%) |
| 16 LPEs | 6,170M | +67.4% | +105M (+2.9%) |

The first expression pays for the lobe splits (NEE's and the bounce's,
`eval_split`) and the routing record; each further one for its share of the
backward gather, pruned by the expressions a state can still accept
(`Lpe::live_mask`). Writing a product adds its channels' compression on top
(~240M per colour layer here). Three optimizations got the first expression
from +1.6G to these numbers: splitting only when an expression needs it, no
re-drawn sample in `scatter_split`, and no heap allocation in the gather.

The beauty-only render is unchanged: 4,161.8–4,161.9M instructions against
Phase 1's 4,178.8M — lower, because `eval_diffuse` and `ShadingPoint::eval`
are now forced inline (adding `eval_split` as a second caller had pushed
them out of line, +0.28%, and inlining `eval_diffuse` back made `eval_all`
cheaper than before). All 33 goldens bit-identical. `bench_ab.sh`, 5
interleaved reps against the pre-AOV binary, render seconds (min / mean):
cornellbox +0.1% / −0.4%, veach_mis +0.1% / −0.2%, materialx_showcase
+0.5% / +0.4%, openpbr_showcase −2.0% / +1.3% — noise. The MaterialX scene
is the one that does pay something real: the closure walk's coat detection
runs at every vertex, beauty or not.

## Raw light (`add-raw-lighting-aovs`)

V-Ray-style raw light: `rawLight` / `rawGI` / `rawTotalLight`, and
`crust:aov:raw` on any expression whose every path starts with a diffuse
reflection, divided per camera sample by the first hit's diffuse colour,
which `diffuse_albedo` reports. The change's `design.md` has the reasoning;
the points to keep here:

- **Per sample, not per pixel.** The film divides each sample's routed value
  by that sample's filter (`SampleExtras::diffuse_filter`), so `raw ×
  filter` is the light per sample. Per pixel it is exact only where the
  colour is constant; at edges `mean(a·b) ≠ mean(a)·mean(b)`, as for V-Ray.
- **The filter** is the colour factor of `eval_split`'s diffuse share,
  `ρ · (1 − F̄) · base_atten · dark` (OpenPBR) or Σ `color × weight` over
  `Diffuse` leaves (closures). The coat's directional passage and EON's
  shape stay in the light; EON's multiple scattering leaves raw light a
  faint dependence on the colour (the per-sample identity is exact anyway).
  Several diffuse lobes divide by the sum of their filters, never each by
  its own (that would count the light once per lobe).
- **Below 1e-4** (`aov::RAW_FILTER_FLOOR`) a channel's raw value is 0.
- **Only diffuse starts.** `Lpe::starts_with_diffuse_reflection` decides
  on the language: a bare `'diffuse'` label is refused, since it also
  matches events crust never labels so.
- **A raw and a plain slot of one expression share its DFA bit**; nothing
  is routed twice. The filter is computed only when a raw or
  `diffuse_albedo` AOV asks for it.

## Expression variance (`add-lpe-variance`)

`bool crust:aov:variance = true` on an `lpe` var: one scalar channel, the
per-pixel variance of the expression's luminance mean. The change's
`design.md` has the reasoning; the points to keep here:

- **One estimator.** `tracer::var_of_mean(sum, sq, n)` is the free function
  behind `PixelState` (adaptive stop, the `variance` AOV) and the variance
  slot alike, `+∞` below two samples. Per sample the slot reduces
  `luma(w · v)` with the layout's `luma` (the lights' working-space luma) —
  the beauty's own expression in `advance_pixel` — so `C.*[LO]`'s variance
  is the `variance` AOV bit for bit (`tests/lpe.rs`, guided passes and the
  Cornell box included). Passes blend it with the beauty's `Σ share² · var`.
- **Zeros count.** Every sample lands in every slot, so `n` is the pixel's
  own `taken`; no count plane is kept (the proposal's `n` plane would
  always equal it).
- **Moments live only in the unit.** `SlotKey::variance` gives the unit's
  `SlotPlanes` a `VarPlanes { sum, sq }` (f64); `store` resolves them, and
  the film's plane is one f32 per pixel like any scalar. A frame costs 4
  bytes per pixel per variance var, a tile 16 more while it renders.
- **Sharing.** The variance and value slots of one expression share its DFA
  bit (distinct `SlotKey`s, one `lpes` entry), as raw and plain do. A
  variance slot is never `hits_only` whatever its clear value.
- **Refusals at import** (`products.rs`): not `sourceType = "lpe"`
  (`rawLight` included), `closest` accumulation, or a type that is not one
  float/half/double; an unauthored type is `float`.
- **Not additive.** The components of one sample are correlated, so the
  variances of a partition do not sum to the beauty's; the user page says
  so and the diagnostic reports each on its own.

## Motion vector AOV (`add-motion-vector-aov`)

`motionvector`: the pass VectorBlur2 blurs a sharp beauty along, and the
`disableMotionBlur` that makes the beauty sharp. The change's `design.md`
has the full reasoning (D1–D7); the points to keep here:

- **Definition.** The first hit's *forward* 2D displacement from shutter
  open to shutter close, in pixels of the rendered image, `u` right and `v`
  up (Nuke's conventions, Arnold's raw `motionvector`), measured through the
  pinhole at the lens centre (depth of field ignored on purpose: the vector
  describes the in-focus image). Per-shutter units: the authored
  `crust:motion:translate` *is* the displacement over the shutter;
  `shutter:open/close` are still not read. Closest by default, clear `0`,
  two floating-point components only (`float2`, `half2`, `texCoord2f`;
  `int2`/`uint2` refused through the UINT-is-`sampleCount`-only rule,
  `vector2f` because `vector` is 3-component in USD). Channels are
  lowercase `u`/`v` (`ChannelKind::Motion`), so a var named `forward` lands
  on Nuke's built-in `forward` layer; the UV source keeps `U`/`V`.
- **No aliases.** `velocity`, `Vector`, `motionFore` … each encode their
  vector differently (Cycles packs four components; V-Ray and RenderMan
  have their own conventions). The D5 trap again: an alias would be a
  plausible but wrong channel. Each can be added later as its own row.
- **The motion is derived in `WorldBuilder`, not stated by the importer**
  (`rt_world.rs`, `MotionRecord::of` → `World::motion`). Every
  `Geometry::Instance` attached or set with `InstanceHitId::Own` and an end
  transform whose linear part equals the start's records `end.translation −
  start.translation`, in a sparse table sorted by `geom_id` (16 bytes per
  *moving* geometry; a field on every `SideTables` would charge every static
  id). The record is read off the very transform the kernel interpolates, so
  the vector and the blur cannot disagree, and the importer's cases fall out
  for free: a non-invertible mesh drops its motion (baked, no end transform,
  no record), a prototype has `transform_end: None`. An end transform with a
  different linear part, a forwarded label, or an instance over a scene that
  itself moves is `Unresolved`: vector 0, counted, one summarised `WARN` at
  commit. So is any id a forwarding instance reports its inner hits under
  (`InstanceHitId::As(k)`: `k`; `Offset(base)`: `base ..= base +
  inner.max_hit_id()`): a hit carrying it may lie in the forwarding
  placement rather than on the geometry that owns the id, so the owner's
  record goes too, whichever was attached first. The unresolved ids are a
  set, the slots' current state, so a reserved slot given a geometry twice
  is one id and a replaced one is none. Nothing produces any of those today;
  the guards are for the next producer.
  Rejected: storing both endpoint transforms and evaluating `(E − L)·M(t)⁻¹·P`
  for rotation too — 96 bytes per moving geometry and an inverse per sample
  for motion nothing produces.
- **Plumbing.** `FirstHit::Surface` carries `motion`, filled at vertex 0 and
  in `first_wall` (whose `ThinWalls::first` now keeps the wall's `geom_id`),
  looked up only when the layout has a `motionvector` slot
  (`AovLayout::motion` → `PathScratch::motion` → `motion_on`, a constant
  `false` in the beauty-only instantiation). `SampleExtras` carries the
  sample's shutter `time`. `CameraFrame` gained the image plane
  (`lower_left − origin`, `horizontal`, `vertical`) and the resolution, and
  `proj`, the pinhole inverse of `Camera::get_ray` at the lens centre
  (pinned to 1e-5 over two cameras, 25 directions and 5 depths each).
- **The value: rebase, clip, project** (`aov::motion_vector`). `P0 = p −
  time·v` (exact: the kernel moves every point by `v` at constant speed), `P1
  = P0 + v`, the segment clipped to camera depth `≥ 1e-3·max(z0, z1)`, both
  ends projected, the difference scaled by `(width, height)`. The rebase
  makes the value independent of the sample's shutter time, hence of
  whether the beauty is blurred; the clip handles a path through the camera
  (no near plane; a projection at or behind depth 0 blows up or flips
  sign) and depends only on the path, never on `time`, so it keeps that
  independence. `max(z0, z1) > 0` always holds because the visible hit lies
  on the segment in front of the camera, so at most one end is clipped and
  the result is finite, pointing the way the visible part moves. Rejected:
  the clear value for such points (a fast object moving towards the camera
  would lose its blur where it is most visible) and a fixed epsilon (wrong
  at either end of scene scale).
- **Perspective, not interpolation.** Equal world displacements cover more
  pixels up close, so the vector varies across an object even though `v` is
  constant: the end-to-end test's receding floor shortens towards the
  horizon, and two renders of a sphere (blur on, blur off) agree on the
  card at constant depth to rounding but on the sphere only to a fraction of
  a pixel, because their closest samples hit at different depths. A straight
  3D path still projects to a straight 2D segment — the one VectorBlur blurs
  along; only the speed along it is non-uniform.
- **`disableMotionBlur` / `instantaneousShutter`** (`RenderSettings::
  motion_blur`, resolved in `import_render_products` like the camera and
  resolution: the first product's value, else the settings prim's, each flag
  on its own; either `true` is off — but unlike the camera and resolution a
  later product that differs is written with the first's blur and warned
  about, not refused) gate only the shutter draw
  (`Renderer::shutter`, `world.has_motion() && settings.motion_blur`, decided
  once per pass into `PassConfig` so the beauty-only hot path reads one flag
  as before: callgrind on cornellbox at 2 spp counts the same instructions
  in `advance_pixel::<false, false>` with and without this change), never
  the records. Off, no `K_TIME` domain is derived, every ray has `time = 0`
  and the beauty is sharp at the authored positions, bit-identical between
  the two spellings. The two left `warn_unhonoured`. No CLI flag, no
  environment switch: a scene setting, not an optimisation to A/B.
- **Verified in numbers, not by eye.** `crates/crust-render/tests/
  motion_vector.rs` renders `samples/motionvector.usda` through the CLI and
  checks the written `forward.u/.v` against an independent pinhole projection
  of the product's own `P` channel: the right-moving sphere's centre pixel
  within 0.01 px with `v ≈ 0`, the rising sphere's `v > 0` (up is up in the
  file, not in a buffer), the floor's near vectors longer than its far ones,
  the sky `(0, 0)`, every pixel on the row through the sphere either the
  background's `(0, 0)` or its own hit's prediction (closest never blends),
  and the blurred render's vectors equal to its own hits' predictions (the
  rebase, end to end). The zero-AOV render is unchanged: `AOV = false`
  instantiation pinned by callgrind on cornellbox, tiles ↔ scanlines and
  the beauty with and without the var bit-identical
  (`crates/crust-core/tests/motion_vector.rs`).
- **Known gaps.** No 3D velocity, no backward or centred vector, no
  per-frame units, no camera, rotation or deformation motion, no Arnold
  normalised encoding; an instancer prototype authoring
  `crust:motion:translate` neither blurs nor gets a vector (consistent with
  the beauty; `usd-scene-import` known gaps). Open: whether 3-component
  `dataType`s (`color3f`, `vector3f`) should be accepted with a zero third
  channel for Arnold-authored RenderVars that ask for RGB — a later
  relaxation of the type check if real files need it. The VectorBlur2
  settings the user doc gives await a check in Nuke by a person (the
  change's task 6.4).
- `diffuse_albedo` stopped being an alias of `albedo`.

## Known gaps

- Every Non-Goal above, which is warned when authored, not silent.
- The `resolution` fallback is 640×360, not the schema's 2048×1080.
- LPE extensions (RenderMan lobe tokens and prefixes, Arnold `A`, `!`
  inversion) are not parsed.
- `B` is not an event; the dome is `L`.
- `Ng` (geometric normal) is refused: the kernel reports one normal per hit.
- `normal` / `Neye` ignore a MaterialX closure's own `normal` input (applied
  per leaf, after the shading point exists); UsdPreviewSurface and OpenPBR
  normal maps are honoured.
- Guided renders with ≥ 2 training iterations are not repeatable (`ΔEff` is
  wall-clock), so neither are their AOVs.
- Light groups read `crust:light:lpeTag` only; other renderers' tag
  attributes are not read, and backdrops carry no tag.
- The albedo of a volume scatter before any surface is 1.
- Light-only (`--strategy light`) renders near-mirror lobes black at
  practical sample counts — true of the beauty as of every expression.

## Sources

Gathered on 2026-10-03; openusd.org pages were read through their GitHub
sources.

- UsdRender schema:
  `github.com/PixarAnimationStudios/OpenUSD/blob/release/pxr/usd/usdRender/schema.usda`,
  `overview.dox`, `spec.cpp`. Proposal:
  `openusd.org/release/wp_render_settings.html`.
- Hydra: `pxr/imaging/hd/tokens.h`, `aov.h`, `plugin/hdEmbree/renderBuffer.cpp`.
  `pxr/usdImaging/usdImaging/renderSettingsFlatteningSceneIndex.cpp`.
  `pxr/usdImaging/usdAppUtils/frameRecorder.cpp` (usdrecord).
- hdPrman: `third_party/renderman/plugin/hdPrman/renderParam.cpp`.
- Arnold: `github.com/Autodesk/arnold-usd` `libs/common/rendersettings_utils.cpp`,
  `libs/render_delegate/render_delegate.cpp`, and `testsuite/test_0228`,
  `test_2707` (Houdini-authored RenderVars).
- Cycles: `github.com/blender/cycles` `src/hydra/session.cpp`,
  `src/kernel/film/data_passes.h`, `denoising_passes.h`.
- OSL LPE: `github.com/AcademySoftwareFoundation/OpenShadingLanguage` wiki
  "OSL Light Path Expressions", `src/liboslexec/lpeparse.cpp`.
- OpenEXR: `website/TechnicalIntroduction.rst` (channel naming),
  `DeepIDsSpecification.rst`.
- ASWF Color Interop, `Recommendations/04_OpenEXRFiles/OpenEXRFiles.md`
  (v1.0.0, 2026-07-14).
- Cryptomatte v1.2: `github.com/Psyop/Cryptomatte/specification`.
- OIDN: `github.com/RenderKit/oidn/blob/master/doc/api.md`.
- Karma, RenderMan, Arnold, Omniverse user docs were blocked by the network
  proxy. Claims attributed to them come from search summaries and the
  delegates' source, and are marked as conventions above. Re-check before
  hard-coding names (Open Question 2).
