# Material Fidelity suite: crust baseline

Ben Houston's [Material Fidelity suite](https://github.com/bhouston/material-fidelity)
renders 826 MaterialX materials on one shader ball under one fixed setup and scores
each renderer against MaterialXView's GLSL render (`materialx-glsl.png`) by PSNR
over 8-bit RGB. This page is crust's run of the **full** suite, measured on
2026-09-27 at crust `2b80992`, material-samples `36422cb`.

**Headline: crust reads none of the suite's materials.** Every document's surface is
a `standard_surface` (649), `gltf_pbr` (91) or `open_pbr_surface` (86) node, and
`crust-mtlx` reduces only *standalone* BSDF graphs (`oren_nayar_diffuse_bsdf`,
`dielectric_bsdf`, `layer`, `mix`, …; see `openspec/specs/materials/design.md`
§ MaterialX). The compiler reports the surface node as unsupported and every ball
renders with the fallback material. The numbers below therefore measure the
*scene* match (background, framing, lighting) plus a white ball, not shading. They
are a floor to measure support against, not a verdict on the BSDFs.

## Running it

```bash
git clone https://github.com/bhouston/material-fidelity
GIT_LFS_SKIP_SMUDGE=1 git clone https://github.com/bhouston/material-samples \
    material-fidelity/submodules/material-samples        # 4.5 GB, includes the reference PNGs
pip install numpy pillow pygltflib OpenEXR
cargo build --release
scripts/material_fidelity/run.py --suite material-fidelity --out fidelity-out      # ~70 min on 4 cores
scripts/material_fidelity/run.py --suite material-fidelity --materials noise3d     # a subset (substring or re:...)
scripts/material_fidelity/summarize.py --suite material-fidelity fidelity-out/results.json
```

`run.py` writes `crust.png` beside each material, following the suite's
`<renderer>.png` convention, so the suite's own viewer (`pnpm viewer`) can show it
next to the others. `results.json` keeps PSNR, time, the render log and the logged
unsupported nodes for each material.

## The scene contract, and how crust meets it

| suite contract | crust shot layer |
|---|---|
| camera: vertical FOV 45°, near 0.05, eye (0,0,5) looking at the origin | `focalLength` 50, apertures 41.4214, `translate (0,0,5)` (a USD camera looks down −Z) |
| `ShaderBall.glb`, centred, bounding-sphere radius 2 | `glb_to_usda.py` converts it once: TRS baked into the points, then normalised, `v = 1 − v` as MaterialX's glTF loader does, `subdivisionScheme = "none"` |
| IBL from `san_giuseppe_bridge_2k.hdr`, background visible, no direct light | `DomeLight` with the `.hdr`, **`rotateY = 180`** |
| no shadows (the Cycles renderer sets `visible_shadow = False`; the rasterisers have no shadow map) | `crust:rayMask = 5` on both meshes (camera + indirect, no shadow rays) |
| 512², no tone mapping, sRGB | EXR → clamp → sRGB OETF → 8-bit in `run.py` (crust's own PNG is tone-mapped) |

The dome rotation was fitted on the pixels outside the ball, which are pure
environment. At 180° those pixels score **34.2 dB** against the reference, against
11.4–11.6 dB at 177° and 183°. The rest of the gap is the reference's softer background
(MaterialXView filters the environment it displays). So MaterialXView's lat-long seam
is where crust (−Z at `u = 0.5`) puts the image centre. The UV flip was checked on
`uv_debug_texcoord_rgb`. Crust ran at 64 spp (32 minimum, adaptive),
`maxDepth` 12 and `indirectClamp 0`: about 5 s a material.

## Results

PSNR in dB against `materialx-glsl.png`, higher is better. 767 of 826 materials
ship a reference. The other columns are the suite's own published renders of the
same materials, scored by the same code.

| group | n | crust mean | crust median | blender-new mean | blender-nodes mean | threejs-new mean |
|---|---|---|---|---|---|---|
| **all** | 767 | 14.44 | 13.20 | 26.37 | 27.37 | 30.26 |
| nodes | 478 | 13.85 | 12.43 | 27.37 | 28.90 | 31.37 |
| showcase/gltf_pbr | 5 | 12.55 | 12.00 | 22.25 | 22.28 | 25.85 |
| showcase/open_pbr_surface | 8 | 13.53 | 11.88 | 22.75 | 22.79 | 26.41 |
| showcase/standard_surface | 16 | 12.72 | 11.89 | 24.60 | 26.25 | 29.18 |
| surfaces/gltf_pbr | 85 | 14.21 | 14.72 | 24.63 | 24.65 | 28.38 |
| surfaces/open_pbr_surface | 77 | 16.02 | 15.22 | 24.69 | 24.70 | 28.38 |
| surfaces/standard_surface | 98 | 16.78 | 16.11 | 25.11 | 25.18 | 28.84 |
| surface: `gltf_pbr` | 90 | 14.12 | 14.50 | 24.50 | 24.52 | 28.24 |
| surface: `open_pbr_surface` | 85 | 15.78 | 15.03 | 24.50 | 24.52 | 28.20 |
| surface: `standard_surface` | 592 | 14.30 | 12.80 | 26.92 | 28.21 | 30.88 |

The "highest crust" materials (~26 dB) are near-white or grey references
(`roughness`, `feature_subsurface`, `input_sheen_roughness`), where the fallback ball
happens to look similar. They are not evidence of anything. The lowest (~7.5 dB) are
saturated or dark references. `image_format_avif/svg/webp` score about 10 dB for
**every** renderer: their reference is itself a failed texture load.

## What stands between crust and a meaningful score

1. **The three surface nodes (all 826 materials).** MaterialX defines each one
   as a nodegraph over the standalone BSDFs crust already reduces (`libraries/bxdf/*.mtlx`).
   Crust's own model is OpenPBR, so `open_pbr_surface` maps onto it almost
   parameter for parameter, `standard_surface` through the documented Standard
   Surface → OpenPBR mapping, and `gltf_pbr` through its metallic-roughness
   definition. This one change turns the suite from "fallback everywhere" into a
   measurement.
2. **Pattern nodes `crust-mtlx`'s compiler has no operator for.** This is a static scan
   of the documents (`summarize.py`), because the compiler stops at the unknown surface
   node and never reaches the graph behind it, so the render log names only the
   surface. Materials affected, one example each:

| node | materials | example |
|---|---|---|
| `standard_surface` | 649 | `absval` |
| `separate2` | 266 | `absval` |
| `fract` | 217 | `absval` |
| `range` | 167 | `artistic_ior_aluminium` |
| `ifgreater` | 146 | `absval` |
| `combine4` | 142 | `absval` |
| `gltf_pbr` | 91 | `carpaint` |
| `open_pbr_surface` | 86 | `carpaint` |
| `separate3` | 68 | `binormal_default` |
| `dot` | 42 | `convert_invalid_implicit_boolean_to_color3` |
| `magnitude` | 26 | `binormal_default` |
| `hextiledimage` | 25 | `hextiledimage` |
| `place2d` | 21 | `hextiledimage_texcoord_translate` |
| `unifiednoise2d` | 21 | `unifiednoise2d` |
| `unifiednoise3d` | 21 | `unifiednoise3d` |
| `tangent` | 20 | `gltf_normalmap_manual_model_frame_to_world` |
| `bitangent` | 19 | `binormal_default` |
| `transformmatrix` | 17 | `matrix33_transformmatrix` |
| `creatematrix` | 15 | `matrix33_creatematrix` |
| `atan2` | 14 | `atan2` |
| `separate4` | 13 | `combine4` |
| `fractal3d` | 12 | `fractal3d` |
| `splitlr` | 11 | `colorcorrect_edge_contrast_negative` |
| `splittb` | 11 | `colorcorrect_edge_contrast_negative` |

   Plus: `colorcorrect` (10), `fractal2d` (10), `rotate3d` (10), `worleynoise2d` (10), `worleynoise3d` (10), `gltf_normalmap` (8), `transformnormal` (8), `noise2d` (7), `noise3d` (7), `rotate2d` (7), `ramptb` (6), `cellnoise2d` (5), `cellnoise3d` (5), `checkerboard` (5), `gltf_image` (5), `transformvector` (5), `ifequal` (5), `heighttonormal` (5), `determinant` (4), `ramplr` (4), `circle` (3), `transpose` (3), `switch` (3), `blackbody` (2), `ifgreatereq` (2), `bump` (2), `burn` (2), `difference` (2), `dodge` (2), `hsvtorgb` (2), `and` (2), `or` (2), `xor` (2), `invertmatrix` (2), `minus` (2), `ramp` (2), `ramp4` (2), `rgbtohsv` (2), `unpremult` (2), `gltf_colorimage` (2), `open_pbr_anisotropy` (2), `crossproduct` (1), `distance` (1), `frame` (1), `hextilednormalmap` (1), `not` (1), `overlay` (1), `ramp_gradient` (1), `reflect` (1), `refract` (1), `round` (1), `safepower` (1), `saturate` (1), `screen` (1), `tan` (1), `time` (1), `transformpoint` (1), `gltf_iridescence_thickness` (1).

   The first five (`separate2`, `fract`, `range`, `ifgreater`, `combine4`) are the
   suite's harness nodes. Its node-isolation materials wrap the node under test in
   a shared UV-driven graph, so those five gate hundreds of `nodes/*` samples
   whatever node they isolate.
3. **Known reduction gaps that will then show** (`openspec/specs/materials/design.md`
   § Known gaps: MaterialX): MaterialX transmission renders opaque, `thin_film_bsdf`
   pools as a plain dielectric, a coat's `tint` is dropped, and `opacity` is ignored.
   The `transmission_*`, `thin_film` and `feature_opacity` samples will stay low
   until those gaps close.
