+++
title = "Limitations"
description = "What Crust Render does not do, or does only partly."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-01T08:00:00+00:00
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
  procedural smoke, or a voxel grid authored in USD.
- **Building the BVH of a huge scene needs extra memory for a moment.** While the
  acceleration structure is built, its peak memory can be well above what the finished
  render needs, especially with subdivision. Lowering
  [`--subdiv-level`](@/docs/reference/command-line.md#subdiv-level) is the first remedy.
- **Adaptive subdivision refines only geometry used once.**
  [`--subdiv-edge-length`](@/docs/reference/command-line.md#subdiv-edge-length) leaves
  a prototype placed several times at the uniform level, whatever its distance, and
  leaves faces outside the camera's view at their control cage, which reflections and
  shadows then see. Loop subdivision meshes are refined to one level per mesh.

## Materials and textures

- **Native subsurface is approximate.** The `crust:openpbr` subsurface lobe is still a
  tinted diffuse. MaterialX's `subsurface_bsdf` uses a true random walk.
- **`crust:openpbr` and MaterialX's `open_pbr_surface` don't match exactly.** The two
  implementations of OpenPBR still differ in places.
- **MaterialX limits.** A closure tree that collapses to more than eight lobes is refused.
  Only the `uniform_edf` and `generalized_schlick_edf` emission nodes are supported. Zeltner
  sheen is evaluated as Imageworks sheen.
- **Texture filtering is isotropic.** Mip levels are chosen from ray cones, but the filter
  has no direction. A texture seen at a grazing angle is blurred more than an anisotropic
  (EWA) filter would blur it.
- **Streamed Ptex can't rebuild its mip levels in linear light.** A mip-mapped `.ptx` is
  loaded fully unless
  [`CRUST_PTEX_STREAM_MIPSPACE=file`](@/docs/reference/environment-variables.md#crust-ptex-stream-mipspace)
  accepts the file's own, slightly darker, levels.
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
  - A shadow-linked light is sampled only by light sampling, so it is noisier on glossy
    surfaces.

## Path guiding

- **Surfaces only.** Volumes and phase functions aren't guided.
- **Luminance only.** The guide is trained on luminance, not per colour channel.
