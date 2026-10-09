+++
title = "Warnings"
description = "Every coded import warning, its kind and what to do about it."
date = 2026-10-09T08:00:00+00:00
updated = 2026-10-09T08:00:00+00:00
draft = false
weight = 30
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Every warning crust raises while it imports a stage has a stable code, such as <code>light.degenerate_shape</code>, and a kind. This page lists them all.'
toc = true
top = false
+++

## Reading a warning

A coded warning in the log starts with its code in brackets:

```text
WARN [light.degenerate_shape] RectLight at /lights/key: width 0 × height 1 must be finite and positive — skipped
```

Search this page for the code to learn what it means and what to do.

Each code has one **kind**:

- **refused**: an invalid authored value (not finite, out of range, an unknown token, the
  wrong type) was replaced by a fallback.
- **approximated**: a valid authored value is rendered differently from what it asks for.
- **skipped**: something authored (a prim, an asset, an output channel) contributes
  nothing. A constant or a default may stand in for it.

## How warnings are counted

The import keeps one record per code. A record holds the code, its kind, how many times
it fired (`count`), the first 16 distinct prims it fired on (`prims`) and the first
occurrence's message. A warning about the whole stage counts without a prim.

A few codes fire once per mesh and would flood the log on a large scene: they are logged
once per import and counted every time. Their log line ends "(further occurrences are
counted in the import's warnings)".

A texture or other asset that fails to load is explained once, by the loader, naming the
file. Each material or light that references it is counted, so `count` is the number of
references, not of files.

## Compatibility

Codes are a public interface. A new release can add codes; renaming or removing a code, or
changing its kind, changes the version of every report that carries warnings.

Only warnings raised while the stage is imported have codes. Warnings about environment
variables, from the render itself, and from `crust diagnostic`'s own analysis keep their
plain text and are not counted.


## Render settings

| code | kind | meaning | what to do |
|------|------|---------|------------|
| `settings.invalid_value` | refused | A crust:* render setting has a value outside its domain; its default is used. | Fix the value on the RenderSettings prim; the message names the attribute and the default used. |
| `settings.light_samples_clamped` | approximated | A light-sample count is above the most crust takes; the maximum is used. | Author at most the maximum the message names. |
| `time.outside_range` | approximated | The frame lies outside the stage's time range; animated attributes hold their nearest sample. | Render a frame inside the stage's startTimeCode..endTimeCode, or extend the range. |

## Colour

| code | kind | meaning | what to do |
|------|------|---------|------------|
| `color.working_space_refused` | refused | renderingColorSpace names no usable working space; the render is in lin_rec709. | Name a working space the OCIO config defines and crust can render in. |
| `color.unknown_space` | refused | A colour space is not defined by the OCIO config; the value is used as stored. | Name a colour space the OCIO config defines, or set `OCIO` to the config the asset was authored for. |
| `color.no_conversion` | approximated | Two colour spaces have no conversion between them; values are used as stored. | Use a config whose spaces convert to the working space. |
| `color.no_luminance` | approximated | The working space has no RGB to XYZ matrix; colours are weighed by Rec.709 luminance. | Use a working space whose config gives an RGB to XYZ matrix. |

## Camera

| code | kind | meaning | what to do |
|------|------|---------|------------|
| `camera.not_a_camera` | refused | RenderSettings.camera names a prim that is not a camera; the first camera met is used. | Point `RenderSettings.camera` (or `--camera`) at a camera; `crust ls camera` lists them. |
| `camera.missing` | skipped | The stage authors no camera; the procedural default camera is used. | Author a `Camera` prim. |
| `camera.unreadable` | skipped | A camera prim could not be built into a camera. | Check the camera's attributes (focal length, aperture, clipping). |

## Products and AOVs

| code | kind | meaning | what to do |
|------|------|---------|------------|
| `product.not_a_render_product` | skipped | RenderSettings.products targets a prim that is not a RenderProduct. | Point `products` at `RenderProduct` prims. |
| `product.unsupported_type` | skipped | A RenderProduct's productType is not raster; no file is written for it. | Use `productType = "raster"`, or remove the product. |
| `product.camera_mismatch` | skipped | A product renders through another camera or resolution than the first; no file is written for it. | Render the other camera or resolution as a separate render. |
| `product.motion_blur_mismatch` | approximated | A product asks for other motion blur than the first; its file is written with the first's. | Give every product the same motion-blur setting. |
| `product.region_mismatch` | approximated | A product asks for another dataWindowNDC than the first; its file is written over the first's. | Give every product the same `dataWindowNDC`. |
| `product.unhonoured_attribute` | approximated | A product authors a setting crust does not honour; it renders without it. | Remove the attribute, or accept that it is ignored. |
| `product.invalid_data_window` | refused | dataWindowNDC is not finite or selects no pixel; the full frame is rendered. | Author a finite window that covers at least one pixel. |
| `product.data_window_clipped` | approximated | dataWindowNDC reaches outside the frame; overscan is not supported, so it is clipped. | Keep `dataWindowNDC` inside 0..1. |
| `aov.not_a_render_var` | skipped | orderedVars targets a prim that is not a RenderVar. | Point `orderedVars` at `RenderVar` prims. |
| `aov.unsupported_source` | skipped | A RenderVar's source is one crust does not produce yet (planned, primvar, intrinsic); no channel is written. | Remove the var; the [AOV page](@/docs/usd/aovs.md) lists the sources crust writes. |
| `aov.unknown_source` | skipped | A RenderVar names an unknown source or sourceType; no channel is written. | Use a source name from the [AOV page](@/docs/usd/aovs.md). |
| `aov.type_mismatch` | skipped | A RenderVar's data type is not numeric or cannot hold its source; no channel is written. | Author a numeric `dataType` with enough components for the source. |
| `aov.unknown_accumulation` | refused | A RenderVar's accumulation token is not supported; the source's default is used. | Use `zmin` or leave the accumulation unauthored. |
| `aov.accumulation_ignored` | approximated | A per-pixel source cannot be accumulated as authored; its default accumulation is used. | Leave the accumulation unauthored for per-pixel sources. |
| `aov.invalid_variance` | skipped | crust:aov:variance is authored where it cannot apply; no channel is written. | Use `crust:aov:variance` only on an `lpe` var with a float type and filtered accumulation. |
| `aov.raw_ignored` | approximated | crust:aov:raw is authored on a source that is not a light path expression; it is ignored. | Remove `crust:aov:raw`, or use it on an `lpe` var. |
| `aov.invalid_raw` | skipped | crust:aov:raw needs an expression whose every path starts with a diffuse reflection; no channel is written. | Start the expression with a diffuse reflection (`C<RD>…`). |
| `lpe.invalid` | skipped | A light path expression does not parse or compile; no channel is written. | Fix the expression; the message gives the parser's error. |

## Transforms

| code | kind | meaning | what to do |
|------|------|---------|------------|
| `xform.unknown_op` | approximated | xformOpOrder lists an op that is not a UsdGeomXformOp kind; it reads as identity. | Use the UsdGeomXformOp kinds only. |
| `xform.uncomposable` | refused | An xformOp stack could not be composed; the local transform is identity. | Fix the xformOp stack; the message gives the reason. |
| `xform.motion_vector_unsupported` | approximated | Geometry moves other than by a translation; it is motion blurred but the motionvector AOV reads zero. | Expect a zero motion vector on rotating or scaling geometry. |

## Meshes, subdivision and displacement

| code | kind | meaning | what to do |
|------|------|---------|------------|
| `mesh.non_invertible_transform` | approximated | A mesh's transform is not invertible; it is baked instead of instanced. | Give the mesh a non-zero scale on every axis. |
| `mesh.motion_ignored` | skipped | crust:motion:translate on baked (non-invertible) geometry is ignored. | Give the mesh an invertible transform. |
| `mesh.invalid_uvs` | skipped | A subdivided mesh's texture coordinates do not index cleanly; it renders without them. | Fix the mesh's texture-coordinate indices. |
| `mesh.ptex_face_mismatch` | approximated | A mesh's face count differs from its per-face texture's; its shading is wrong. | Bind a Ptex file made for this mesh. |
| `mesh.displaced_at_cage` | approximated | A displaced mesh is not refined, so only its cage vertices move. Logged once per import. | Raise `--subdiv-level` or set `--subdiv-edge-length` to dice the mesh finer. |
| `mesh.ptex_displaced_at_cage` | approximated | A subdivisionScheme = none mesh with Ptex displacement and non-quad faces is displaced at its cage. Logged once per import. | Use quads, or author a subdivision scheme. |
| `mesh.displacement_exceeds_bound` | approximated | Displacement reaches past crust:displacementBound; it is applied unclamped. | Raise `crust:displacementBound` to the reach the message gives. |
| `subdiv.invalid_setting` | refused | A subdivision level is negative or an edge length is not a positive pixel length; it is clamped or ignored. | Author a level of 0 or more and a positive edge length. |
| `subdiv.level_clamped` | approximated | A subdivision level is above the most crust refines to; it is clamped. | Author a level at most the maximum the message names. |
| `subdiv.adaptive_needs_camera` | approximated | Adaptive subdivision has no render camera to measure from; the uniform level is used. | Name the render camera with `--camera` or `RenderSettings.camera`. |
| `subdiv.legacy_level` | skipped | The per-prim crust:subdivisionLevel is no longer read. Logged once per import. | Move `crust:subdivisionLevel` to the RenderSettings prim, or use `--subdiv-level`. |
| `subdiv.loop_needs_triangles` | approximated | subdivisionScheme = loop on a mesh with non-triangle faces; the base cage is rendered. | Triangulate the mesh, or use `catmullClark`. |
| `subdiv.loop_ptex` | approximated | subdivisionScheme = loop with a per-face texture cannot keep its face ids; the base cage is rendered. | Use `catmullClark`, or a UV texture. |
| `subdiv.failed` | approximated | Subdivision or per-face tessellation failed; the base cage is rendered. | Check the mesh topology; the message gives the reason. |
| `displacement.vector_ignored` | approximated | Vector displacement is not applied. | Use scalar displacement. |
| `displacement.unevaluated` | skipped | The displacement amount is driven by a shader crust does not evaluate; the surface is not displaced. | Drive the displacement with a texture or a constant. |

## Curves and volumes

| code | kind | meaning | what to do |
|------|------|---------|------------|
| `curves.unsupported_basis` | skipped | BasisCurves uses a basis crust does not support. | Use `bezier`, `bspline` or `catmullRom`. |
| `curves.invalid_counts` | skipped | curveVertexCounts overruns the points; the remaining curves are skipped. | Make `curveVertexCounts` sum to the number of points. |
| `curves.non_invertible_transform` | skipped | BasisCurves has a non-invertible transform. | Give the curves a non-zero scale on every axis. |
| `volume.unknown_type` | skipped | crust:volume:type is not homogeneous, smoke or grid. | Use `homogeneous`, `smoke` or `grid`. |
| `volume.invalid_grid` | skipped | A grid volume's gridDims and gridData are missing or do not match. | Author `crust:volume:gridDims` and a `crust:volume:gridData` of that many values. |
| `volume.in_prototype` | skipped | A volume inside an instance prototype; volumes cannot be instanced. | Move the volume out of the prototype. |

## Instancing

| code | kind | meaning | what to do |
|------|------|---------|------------|
| `instancing.instanceable_without_prototype` | approximated | An instanceable prim has no prototype; it is imported directly. | Nothing to do unless the prim was meant to be shared. |
| `instancing.nesting_too_deep` | skipped | A prototype nests instances past the supported depth; the deeper levels are not expanded. | Flatten the instance nesting. |
| `instancing.empty_prototype` | skipped | A prototype contributed no geometry. | Check that the prototype holds geometry crust imports. |
| `instancing.no_prototypes` | skipped | A PointInstancer has no prototypes targets. | Author the `prototypes` relationship. |
| `instancing.no_proto_indices` | skipped | A PointInstancer has no protoIndices. | Author `protoIndices`. |
| `instancing.missing_positions` | skipped | A PointInstancer has fewer positions than protoIndices; the extra instances are skipped. | Author one position per `protoIndices` entry. |
| `instancing.proto_index_out_of_range` | skipped | A PointInstancer's protoIndices entry names no prototype; that instance is skipped. | Keep every index below the number of prototypes. |

## Lights

| code | kind | meaning | what to do |
|------|------|---------|------------|
| `light.non_finite_input` | refused | A light input is not finite; its fallback is used. | Author a finite value. |
| `light.unsupported_multiplier` | approximated | A per-lobe light multiplier (diffuse, specular) is not supported; the light contributes at 1.0. | Remove `inputs:diffuse` / `inputs:specular`, or set them to 1. |
| `light.degenerate_shape` | skipped | A light's size or transform collapses its shape; the light is skipped. | Give the light a finite, positive size and a non-zero scale. |
| `light.unsupported_texture_format` | skipped | A DomeLight's texture:format is not latlong; it emits its uniform colour. | Convert the map to lat-long. |
| `light.map_unreadable` | skipped | A light's or dome's texture could not be loaded; it emits its uniform colour. | Check the file path and format; the message gives the loader's reason. |
| `ies.unreadable` | skipped | An IES profile could not be loaded; the light renders without it. | Check the IES file path and format. |

## Light linking

| code | kind | meaning | what to do |
|------|------|---------|------------|
| `light_link.membership_expression` | approximated | A light-link collection authors membershipExpression, which is not read; it includes every prim. | Use `includes` / `excludes` instead of `membershipExpression`. |
| `light_link.unreadable_collection` | refused | A light-link collection cannot be read; it includes every prim. | Fix the collection; the message gives the reason. |
| `light_link.target_in_instance` | approximated | A light-link collection targets a prim inside an instance; membership is judged on the instance. | Target the instance, or make the target not instanced. |
| `light_link.nested_not_composed` | skipped | A nested collection lies outside the streamed chunk that reads it; it contributes nothing. | Author the nested collection outside the payload, or set `CRUST_STREAM_IMPORT=0`. |
| `light_link.too_many_classes` | skipped | The scene needs more light-link classes than crust encodes; light links are ignored. | Reduce the number of distinct light-link sets. |
| `light_link.ray_mask_rewritten` | approximated | Shadow linking rewrites the authored crust:rayMask bits 3-31. | Do not rely on `crust:rayMask` bits 3-31 with shadow linking. |
| `light_link.shadow_link_unencodable` | approximated | A shadowLink collection cannot be encoded; the light is shadowed by every occluder. | Reduce the number of distinct shadow-link sets. |

## Materials

| code | kind | meaning | what to do |
|------|------|---------|------------|
| `material.fallback_default` | skipped | A material cannot be resolved to a shader crust reads; the default grey OpenPBR is used. | Bind a material that exists and has a surface shader crust reads (UsdPreviewSurface, MaterialX, `crust:openpbr`). |
| `material.volume_ignored` | skipped | A MaterialX volume terminal beside a non-MaterialX surface is ignored. | Make the surface MaterialX too, or drop the volume terminal. |
| `mtlx.incomplete` | approximated | Part of a MaterialX network is not represented; those inputs fall back to their defaults. | Check the nodes the message names against the [materials page](@/docs/usd/materials.md). |
| `mtlx.unusable` | skipped | A MaterialX document or network cannot be used; the material falls back. | Fix the MaterialX document; the message gives the reason. |
| `preview.multiple_primvars` | approximated | A UsdPreviewSurface's textures read several primvars; the first is read for all. | Read one primvar for every texture of a material. |
| `preview.unsupported_connection` | skipped | A UsdPreviewSurface input connects to something other than a UsdUVTexture output; its constant is used. | Connect the input to a `UsdUVTexture` output. |
| `preview.texture_alpha` | approximated | A UsdPreviewSurface input reads texture alpha, which reads 1.0. | Use a colour channel instead of alpha. |
| `preview.texture_without_file` | skipped | A UsdUVTexture has no inputs:file; the input keeps its constant. | Author `inputs:file`. |
| `preview.unread_st` | approximated | A UsdUVTexture's st is driven by a shader crust does not read; the mesh chart is used unchanged. | Drive `st` with a `UsdPrimvarReader_float2`. |

## Assets and textures

| code | kind | meaning | what to do |
|------|------|---------|------------|
| `asset.unsupported_by_host` | skipped | The host's asset loader does not decode this type of asset. | Use a host that decodes assets (the `crust` CLI does). |
| `texture.unreadable` | skipped | A texture could not be loaded; the input reads its fallback. | Check the file path and format; the message gives the loader's reason. |
| `texture.udim_tile_missing` | skipped | A tile of a UDIM set does not decode; the set is used without it. | Fix or remove the tile the message names. |
| `texture.tx_stale` | approximated | A .tx is older than its source and is used anyway. | Rerun with `--auto-tx` to reconvert. |
| `texture.tx_convert_failed` | approximated | --auto-tx could not convert a texture; its source is read instead. | Check that the directory is writable and the source decodes. |
| `texture.stream_fallback` | approximated | A texture could not be streamed and is preloaded instead. | Nothing to do; it renders the same, using more memory. |
