+++
title = "Limitations"
description = "What Crust Render does not do, or does only partly."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-09T08:00:00+00:00
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

- **Motion blur is transform-only.** It is set with
  [`crust:motion:translate`](@/docs/usd/geometry.md#crust-motion-translate), and the
  transform is interpolated linearly. There is no deformation blur, no rotation blur, and
  animated transforms aren't blurred.
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
  (periodic curves) are not read, and curve primvars don't reach materials.
- **MaterialX limits.** A closure tree that collapses to more than eight lobes is refused.
  Only the `uniform_edf` and `generalized_schlick_edf` emission nodes are supported. A
  Zeltner sheen uses the table Adobe's reference uses rather than MaterialX's own curve
  fits, which differ from it by up to 0.08 in reflectance for very smooth sheens seen at
  grazing angles.
- **Texture filtering is isotropic.** Mip levels are chosen from ray cones, but the filter
  has no direction. A texture seen at a grazing angle is blurred more than an anisotropic
  (EWA) filter would blur it.
- **Streamed Ptex can't rebuild its mip levels in linear light.** A mip-mapped `.ptx` is
  loaded fully unless
  [`CRUST_PTEX_STREAM_MIPSPACE=file`](@/docs/reference/environment-variables.md#crust-ptex-stream-mipspace)
  accepts the file's own, slightly darker, levels. Displacement Ptex is read raw, so its
  stored levels are already correct and it streams.
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

## Lights

- **Not supported:** mesh lights, portal lights, light filters, and shaping on distant and
  dome lights. `inputs:diffuse` and `inputs:specular` are ignored with a warning.
- **Emissive surfaces aren't sampled as lights.** An emissive MaterialX surface, emissive
  curves, instances or volumes are found only by rays that hit them. Use UsdLux lights for
  a scene's main light sources.
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

## Watching and stopping a render

- **Only the beauty is previewed.** [`--checkpoint`](@/docs/reference/command-line.md#checkpoint)
  rewrites the PNG from the image so far; the AOVs are written once, when the render ends
  or is [interrupted](@/docs/reference/command-line.md#interrupting-a-render).
- **Loading can't be stopped and kept.** A Ctrl-C while the stage loads (or while the
  `learned` light selection trains, before the render starts) quits at once and writes
  nothing.
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

## MCP sessions

- **Every edit imports the whole stage again**, and a session holds the stage it authors
  beside the scene it renders, about twice a render's import memory: Moana-scale stages
  are out of reach. See [Look-dev with Claude Desktop](@/docs/help/claude-desktop.md#limits).
- **Unauthored schema attributes have no type in a session.** Neither openusd 0.7 nor
  openusd-schemas 0.7 ships schema data, and the session registers none, so
  `set_attribute` on an attribute nothing authors yet (a light's `inputs:exposure`) needs
  its `type`, and `query` cannot show its fallback. openusd-schemas' unreleased `main`
  ships the data; the fix comes with crust's move to the release that includes it.
- **`probe` names no prim or material**: there is no table from a hit to its prim yet.
