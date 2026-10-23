+++
title = "Limitations"
description = "What Crust Render does not do, or does only partly."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-23T08:00:00+00:00
draft = false
weight = 30
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Crust Render is a single-author project. Its gaps are documented rather than hidden.'
toc = true
top = false
+++

Each capability's design record in the repository ends with a "Known gaps" section, with
the full detail and workarounds. This page summarizes the gaps that affect what you can
render.

## Platform

- **CPU only.** Rendering runs on the CPU, in parallel with
  [rayon](https://docs.rs/rayon). There is no GPU or wavefront renderer, and no coherent
  ray-packet traversal.
- **SIMD stops at 128 bits.** BVH traversal and triangle tests use SSE2/NEON-width
  vectors. Going wider would need nightly Rust or `unsafe` code
  ([why](@/docs/architecture/design-choices.md#safe-rust-written-from-scratch)).

## Geometry

- **Motion blur is transform-only.** It comes from time-sampled transforms over the
  [camera's shutter](@/docs/usd/render-settings.md#camera-shutter) and from
  [`crust:motion:translate`](@/docs/usd/geometry.md#crust-motion-translate). What is not
  blurred:
  - deformation: animated `points` and `velocities` are read at the frame;
  - curves, lights and volumes, and transforms animated inside a prototype or on a
    `PointInstancer`'s per-instance arrays;
  - the camera's own motion.

  Only the shutter's two ends are read, and the transform is interpolated linearly
  between them, element by element. A path that curves or goes out and back within the
  shutter becomes its chord, and a large rotation shrinks the object mid-blur. With
  `shutter:open` other than 0, transforms are read at `frame + shutter:open` while every
  other attribute is read at the frame.
- **No OpenVDB or `UsdVolVolume`.** Volumes are the three
  [`crust:volume:type`](@/docs/usd/volumes.md#crust-volume-type) kinds: homogeneous,
  procedural smoke, or a voxel grid authored in USD, or a homogeneous medium inside a mesh
  through its material's [`volume` terminal](@/docs/usd/materials.md#volume-materials).
- **Medium boundaries hold one medium at a time.** Inside a volume material's mesh, other
  volume materials and glass don't change the medium, and a camera that starts inside one
  doesn't see it. The mesh must be closed, with outward normals.
- **Building the BVH of a huge scene needs extra memory for a moment.** While the
  acceleration structure is built, its peak memory can be well above what the finished
  render needs, especially with subdivision. Lowering
  [`--subdiv-level`](@/docs/reference/command-line.md#subdiv-level) is the first remedy.
- **Adaptive subdivision refines only geometry used once.**
  [`--subdiv-edge-length`](@/docs/reference/command-line.md#subdiv-edge-length) leaves
  a prototype placed several times at the uniform level, whatever its distance, and
  leaves faces outside the camera's view at their control cage, which reflections and
  shadows then see. Loop subdivision meshes are refined to one level per mesh.

- **Displacement is scalar and applied at the dicing rate.** Vector displacement is
  refused. Detail finer than the tessellation is lost, since nothing turns it into bump.
  The dicing rate ignores displacement: a strongly displaced region is diced as finely as
  its undisplaced cage asks. A displaced `subdivisionScheme = "none"` mesh loses its hard
  edges. Where a map jumps across a UV seam, one ring of triangles stretches across the
  step. RenderMan displacement networks other than a `PxrPtexture` (or a `PxrBlend` multiply of
  two) through an optional `PxrDispTransform` are refused. See
  [displacement](@/docs/usd/materials.md#displacement).

## Materials and textures

- **Thick glass casts a solid shadow.** A closed dielectric (a bottle, a glass, a jar)
  bends the light that crosses it, so shadow rays stop at it. What lies behind or inside
  it is lit only by light that refracts through it, which is slow to converge. A
  thin-walled transmissive surface (a window pane, a soap film) is the exception: shadow
  rays pass through it, tinted. Mark a pane or a sheet thin-walled
  ([`geometryThinWalled`](@/docs/usd/materials.md#geometry)) rather than modelling it as
  a closed slab.
- **Native subsurface is approximate.** The `crust:openpbr` subsurface lobe is still a
  tinted diffuse. MaterialX's `subsurface_bsdf` uses a true random walk.
- **`crust:openpbr` and MaterialX's `open_pbr_surface` don't match exactly.** The two
  implementations of OpenPBR still differ in places.
- **Hair in a groom authored in metres loses strand-to-strand light.** Every ray starts
  0.001 units off the surface it leaves, which is 1 mm in metres: wider than a hair, so
  strands that close neither shadow nor light each other. Author grooms in centimetres.
- **Hair roughness is capped.** `chiang_hair_bsdf` clamps its roughness inputs at 1, as
  MaterialX's reference implementation does, so hair cannot be rougher than an artist
  roughness of about 0.6.
- **Curves are round, open tubes.** `BasisCurves` `normals` (ribbons) and `wrap`
  (periodic curves) are not read. Curve primvars reach materials, but `faceVarying`
  ones are not read. Rational
  `NurbsCurves` (unequal `pointWeights`) are approximated by polynomial spans, and
  NURBS curves above order 4 are skipped.
- **Mesh primvars in materials have gaps.** `geompropvalue` reads a mesh's own primvars,
  but not one authored on an ancestor or a `PointInstancer`; implicit shapes read only
  `constant` ones; and a material that also reads a Ptex texture reads every primvar's
  `default`. A `geompropvalue` in a MaterialX displacement graph reads no primvar at all
  (it gives its `default`, with `mtlx.incomplete`). Point, vector and normal primvars are
  read in the prim's own space, not transformed to world space.
- **MaterialX limits.** A closure tree that collapses to more than eight lobes is refused.
  Only the `uniform_edf` and `generalized_schlick_edf` emission nodes are supported. A
  MaterialX BSDFs follow BSDL, the library OSL's reference renderer shades with, not
  MaterialX's GLSL shaders, so a document can look different in a GLSL viewer. A very
  rough refracting `dielectric_bsdf` lit by a sampled light returns about 2% more light
  than it receives, as it does in OSL's reference renderer. A coat over glass, seen from
  inside within a degree or two past the critical angle, returns up to 6% more than it
  receives.
- **Texture filtering is isotropic.** Mip levels are chosen from ray cones, but the filter
  has no direction. A texture seen at a grazing angle is blurred more than an anisotropic
  (EWA) filter would blur it.
- **Streamed Ptex is in linear light only from the cap down.** A streamed `.ptx` rebuilds
  its mip levels at and below the
  [`CRUST_PTEX_MAX_LOG2`](@/docs/reference/environment-variables.md#crust-ptex-max-log2)
  cap (32×32) in linear light, exactly as a loaded one does. Its levels above the cap are
  the file's own, averaged in the file's encoding, because rebuilding them would mean
  reading full-resolution faces. A loaded `.ptx` has no levels above the cap at all.
- **Small and large Ptex files differ in detail.** A file under
  [`CRUST_PTEX_STREAM_MIN_MB`](@/docs/reference/environment-variables.md#crust-ptex-stream-min-mb)
  (8 MiB) is loaded, capped at 32×32 per face; a larger one streams and keeps the detail
  above the cap. So two textures authored at the same resolution can show different
  detail in a close-up.
- **Each streamed Ptex file holds one open file for the render.** At the defaults this is
  a few dozen files on the Moana Island. Streaming thousands, by lowering
  `CRUST_PTEX_STREAM_MIN_MB` or raising the budget, can exceed the open-file limit.
- **Colour-space conversions are a curve and a matrix.** Every texture space in the ACES
  configs is a transfer curve followed by a change of primaries, and Crust Render
  converts exactly those. A conversion of any other shape, such as one through a 3D LUT,
  is refused with a warning, and the values are used as stored.
- **A colour that names no colour space is taken as already in the working space**, even
  where `UsdColorSpaceAPI` would fall back to `lin_rec709_scene`. The two agree in the
  default working space.
- **Colour Ptex is always decoded by gamma 2.2.** There is no way to declare a colour
  `.ptx` linear, so linear colour Ptex data renders too dark. Ptex read as a displacement
  map is read raw.
- **Some inputs are read but not used**, with a warning: `subsurface*` and `specularTint`
  on `PxrDisneyBsdf`, and `UsdTransform2d` on `UsdPreviewSurface` textures.

## Camera and framing

- **Square pixels only.** An authored `pixelAspectRatio` other than 1 is ignored with a
  warning, and the `aspectRatioConformPolicy` `adjustPixelAspectRatio` frames as
  `expandAperture` with a warning naming the pixel aspect it would have needed
  ([details](@/docs/usd/render-settings.md#aspectratioconformpolicy)).
- **No lens shift.** `horizontalApertureOffset` and `verticalApertureOffset` are not read:
  the frame is always centred on the camera's axis.

## Lights

- **Not supported:** mesh lights, portal lights, light filters, and shaping on distant and
  dome lights.
- **Diffuse and specular multipliers are averaged in the lobe AOVs.** The part of a light
  with `inputs:diffuse` or `inputs:specular` that a surface receives through a bounce is
  shared between its lobes' AOVs at the multipliers' average for that bounce
  ([details](@/docs/usd/lights.md#diffuse-and-specular-multipliers)). The beauty is exact.
- **Emissive surfaces aren't sampled as lights.** An emissive MaterialX surface, emissive
  curves, instances or volumes are found only by rays that hit them. Use UsdLux lights for
  a scene's main light sources.
- **The light tree can't see walls.** `tree` light selection ranks lights by power,
  distance, orientation and the surface's horizon, not by whether anything is in the way;
  `learned` is the visibility-aware selection. It treats the receiving surface as diffuse,
  so a glossy highlight from a dim light is not favoured, and lights at infinity (domes,
  distant lights) keep a fixed share whatever they contribute. A large light close to the
  subject is picked often even where it faces away, so a studio rig of big softboxes can
  render noisier under `tree` than under `power`; `tree` is opt-in for that reason.
- **Light-linking gaps:**
  - A collection target inside an instance prototype can't tell instances apart.
  - `membershipExpression` is refused.
  - A shadow-linked dome light is sampled only by light sampling, so it is noisier on
    glossy surfaces. Other shadow-linked lights are combined with BSDF sampling as usual
    (see [`CRUST_LINK_TWIN`](@/docs/reference/environment-variables.md#crust-link-twin)).
  - Seen in a mirror or through clear glass, a shadow-linked light casts the shadows of
    every occluder, including the ones its `collection:shadowLink` leaves out.

## Path guiding

- **Not repeatable bit for bit.** A guided render decides whether its last pass is
  guided from an efficiency measured in wall-clock time, so two guided renders of the
  same stage can differ when that estimate sits near 1.
- **Surfaces only.** Volumes and phase functions aren't guided.
- **Luminance only.** The guide is trained on luminance, not per colour channel.
- **Darker where fireflies carry the image.** On ALab, a guided render of a crop came
  out 6.9% darker than an unguided one over 40 seeds — a bias, not noise, and not yet
  diagnosed ([#244](https://github.com/doubleailes/crust-render/issues/244)).
  `crust diagnostic` reports guiding as `biased` there, and never suggests it. Scenes without strong fireflies (the Cornell box, `veach_mis`) show no shift.

## Precomp

- **Display operations are a fixed set.** Blur, convolve, vector blur, merge and grade;
  no programmable neighbourhood operation, no roto, nothing across frames.
- **Sample filters read AOVs only.** A graph holding a texture, a `geompropvalue`, a
  geometry node or a closure is refused, and so is a node crust's MaterialX compiler does
  not implement (`hsvadjust`, for one).
- **Crops clamp at their edges.** A display filter on a
  [region](@/docs/usd/render-settings.md#datawindowndc) reads the region's border pixel past
  its edge, so a blurred crop is not the same pixels of a blurred full frame near its
  border.
- **Adaptive sampling reads the beauty only.** A stage whose beauty is black stops every
  pixel at its minimum; see [A stage without a
  beauty](@/docs/usd/render-settings.md#a-stage-without-a-beauty).
- **An MCP edit re-imports.** While the settings author a precomp, every edit imports the
  stage again rather than updating it in place.

## Watching and stopping a render

- **Only the beauty, or the displayed output, is previewed.**
  [`--checkpoint`](@/docs/reference/command-line.md#checkpoint) rewrites the PNG from the
  image so far — the [displayed output](@/docs/usd/render-settings.md#crust-display) when
  the settings author one; the AOVs are written once, when the render ends or is
  [interrupted](@/docs/reference/command-line.md#interrupting-a-render).
- **Loading can't be stopped and kept.** A Ctrl-C while the stage loads (or while the
  `learned` or `neural` light selection trains, before the render starts) quits at once
  and writes nothing.
- **A guided preview gets noisier when the last pass starts.** Its first samples replace
  the last training pass's image, tile by tile, until it catches up.
- **An interrupted guided render's `crust:sppTaken`** counts its last pass's samples only,
  and is `(0, 0)` when its image holds no last pass: stopped during training, or before
  the last pass gave every pixel two samples (that pass is then left out). The training
  samples the image does hold are not counted there. `crust:renderStatus` says the frame
  is partial.
- **The progress bar counts scheduled samples.** In an adaptive render it runs ahead once
  most pixels have stopped, and a guided render's training passes don't move it.

## Diagnostic

- **Crops stand in for the frame.** `crust diagnostic` compares settings on up to three
  crops; its full-frame numbers are estimates. See
  [Diagnosing a render](@/docs/help/diagnosing-a-render.md#limitations) for the rest.
- **Guiding is suggested as an attribute.** `crust render` has no guiding flag, so a
  guiding suggestion is authored on the stage (`crust:pathGuiding`).

## Hydra (usdview)

- **Only usdview on OpenUSD 26.08 is tested**, on Linux and Windows. Houdini Solaris,
  Maya-USD and Omniverse are not. macOS is not built.
- **Light linking, volume fields (UsdVol), motion blur and points are not carried**
  to crust through Hydra. Each is reported once and the rest renders. See
  [Rendering in usdview](@/docs/help/usdview.md#what-it-does-not-carry-yet).
- **The scene is written to disk.** The delegate writes the stage as USD layers and
  crust imports them. Memory holds Hydra's copy of the scene and crust's.
- **AOVs fill at completion**, not while the render refines. Only `depth`, `normal`,
  `Neye` and `Peye` are rendered.
- **Playback shows each frame's first pass**, which is not real time on a large
  viewport or stage. Every frame is a full import.

## MCP sessions

- **Structural edits import the whole stage again.** An edit confined to values of the
  render settings, the render camera, a light's inputs or a used material's shader inputs
  is applied in place, keeping the scene's geometry and BVH. Anything else (a prim added
  or removed, a binding, a transform, visibility, a variant, a light's size), or an
  in-place update that cannot give the same scene a full import would, imports the saved
  file again, which on a production stage takes as long as a render's import, less the
  texture decoding the session keeps.
  [`CRUST_SCENE_DELTAS=0`](@/docs/reference/environment-variables.md#crust-scene-deltas)
  imports every edit whole.
- **A large stage costs more memory in a session than in a render.** Between edits a
  session holds the scene it renders, its authoring stage (payloads unloaded) and every
  texture it decoded, including ones the scene no longer uses until the next full import.
  A full import builds the new scene before the old one is let go, so its peak is about
  two scenes; an in-place edit does not. Applying most edits in place makes look-dev
  edits cheap, not the stage smaller: Moana-scale stages are still out of reach. See
  [Look-dev with Claude Desktop](@/docs/help/claude-desktop.md#limits).
- **`query` can show a fallback the render does not use.** An unauthored schema
  attribute answers the USD schema's fallback (`source` `fallback`), but where crust has
  its own default the render uses that instead: a DistantLight with no
  `inputs:intensity` queries as `50000` and renders as `1`; a camera with no
  `verticalAperture` queries as `15.2908` but follows the image's aspect, and one with
  no `focusDistance` queries as `0` but focuses at `10`. Author the value to be sure.
- **`probe` names no prim or material**: there is no table from a hit to its prim yet.
