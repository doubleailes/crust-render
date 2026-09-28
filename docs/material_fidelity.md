# Material Fidelity suite: crust results

Ben Houston's [Material Fidelity suite](https://github.com/bhouston/material-fidelity)
renders 826 MaterialX materials on one shader ball under one fixed setup and scores
each renderer against MaterialXView's GLSL render (`materialx-glsl.png`) by PSNR
over 8-bit RGB. This page is crust's run of the **full** suite, measured on
2026-09-28 at crust `a2a11cb` (the `add-mtlx-surface-shaders` closure-tree
evaluator), material-samples `36422cb`.

**Headline: the surface groups now score level with Blender.** Every document's
surface is a `standard_surface` (649), `gltf_pbr` (91) or `open_pbr_surface` (86)
node. Crust now expands each one into the closure tree of its MaterialX nodegraph
and evaluates that tree with MaterialX's own layering (see
`openspec/specs/materials/design.md` § MaterialX). The `surfaces/*` groups score
24.1–24.6 dB against `blender-new`'s 24.6–25.1, and `showcase/open_pbr_surface`
scores above it. The first run, at `2b80992`, read none of these documents: every
ball rendered with the fallback material, and the suite mean was 14.44 dB. It is
now 20.02 dB. What remains is mostly **pattern nodes** crust's compiler has no
operator for, not shading; the `nodes/*` group (17.5 dB) is where that shows.

## Running it

```bash
git clone https://github.com/bhouston/material-fidelity
git clone https://github.com/bhouston/material-samples \
    material-fidelity/submodules/material-samples        # 4.5 GB, includes the reference PNGs (no LFS)
pip install numpy pillow pygltflib OpenEXR
cargo build --release
scripts/material_fidelity/run.py --suite material-fidelity --out fidelity-out      # ~85 min on 4 cores
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
`maxDepth` 12 and `indirectClamp 0`: about 5 s a material. This is a score against
an external reference, not a regression check: noise costs PSNR, so it wants more
samples, not the 16 spp that `check_images.sh` pins for bit-identity between two
crust binaries.

## Results

PSNR in dB against `materialx-glsl.png`, higher is better. 767 of 826 materials
ship a reference. The other columns are the suite's own published renders of the
same materials, scored by the same code. No render failed; no surface node is
logged as unsupported.

| group | n | crust mean | crust median | blender-new mean | blender-nodes mean | threejs-new mean |
|---|---|---|---|---|---|---|
| **all** | 767 | 20.02 | 19.71 | 26.37 | 27.37 | 30.26 |
| nodes | 478 | 17.53 | 17.03 | 27.37 | 28.90 | 31.37 |
| showcase/gltf_pbr | 5 | 21.26 | 20.40 | 22.25 | 22.28 | 25.85 |
| showcase/open_pbr_surface | 8 | 23.32 | 24.71 | 22.75 | 22.79 | 26.41 |
| showcase/standard_surface | 16 | 21.76 | 20.02 | 24.60 | 26.25 | 29.18 |
| surfaces/gltf_pbr | 85 | 24.09 | 24.35 | 24.63 | 24.65 | 28.38 |
| surfaces/open_pbr_surface | 77 | 24.56 | 25.68 | 24.69 | 24.70 | 28.38 |
| surfaces/standard_surface | 98 | 24.48 | 25.40 | 25.11 | 25.18 | 28.84 |
| surface: `gltf_pbr` | 90 | 23.93 | 24.31 | 24.50 | 24.52 | 28.24 |
| surface: `open_pbr_surface` | 85 | 24.45 | 25.67 | 24.50 | 24.52 | 28.20 |
| surface: `standard_surface` | 592 | 18.79 | 18.38 | 26.92 | 28.21 | 30.88 |

Against the first run (fallback everywhere), group means moved: `surfaces/gltf_pbr`
14.21 → 24.09, `surfaces/open_pbr_surface` 16.02 → 24.56, `surfaces/standard_surface`
16.78 → 24.48, `showcase/*` 12.6–13.5 → 21.3–23.3, `nodes` 13.85 → 17.53. Every
`surfaces/*` and `showcase/*` group is within 3 dB of `blender-new`; the widest gap
is `showcase/standard_surface` (−2.84 dB), where `marble_solid`, `brick_procedural`
and the two `onyx_hextiled` samples depend on missing pattern nodes (`fractal3d`,
`hsvtorgb`, `hextiledimage`).

Lowest crust PSNR:

| material | crust | blender-new | blender-nodes | threejs-new |
|---|---|---|---|---|
| `showcase/standard_surface/marble_solid` | 8.585 | 20.172 | 25.025 | 32.519 |
| `surfaces/standard_surface/marble_solid` | 8.585 | 20.172 | 25.025 | 32.519 |
| `nodes/artistic_ior_diag_red_white_edge` | 8.609 | 32.715 | 32.717 | - |
| `nodes/image_format_avif` | 8.858 | 9.755 | 9.756 | 10.185 |
| `nodes/image_format_svg` | 8.858 | 8.887 | 8.887 | 10.177 |
| `nodes/image_format_webp` | 8.858 | 9.744 | 9.744 | 10.167 |
| `nodes/asin_degenerate_out_of_domain_vector4` | 9.379 | 30.676 | 30.717 | 22.774 |
| `nodes/acos_degenerate_out_of_domain_vector4` | 9.428 | 31.096 | 31.128 | 22.775 |
| `surfaces/gltf_pbr/boombox` | 10.28 | 25.441 | 25.537 | 28.891 |
| `nodes/splittb_compare_center_shift` | 10.432 | 24.223 | 24.252 | 30.14 |

Highest crust PSNR:

| material | crust | blender-new | blender-nodes | threejs-new |
|---|---|---|---|---|
| `surfaces/gltf_pbr/feature_emission` | 36.035 | 39.067 | 39.052 | 37.183 |
| `surfaces/standard_surface/greysphere` | 34.661 | 36.672 | 36.823 | 34.026 |
| `nodes/remap_degenerate_vector4` | 34.146 | 36.076 | 36.135 | 33.373 |
| `surfaces/standard_surface/feature_emission` | 34.001 | 36.012 | 36.098 | 38.416 |
| `surfaces/open_pbr_surface/input_base_weight` | 33.173 | 35.841 | 36.046 | 33.839 |
| `surfaces/standard_surface/input_emission` | 32.324 | 33.114 | 33.037 | 34.539 |
| `surfaces/standard_surface/graph_base_color_image_mask` | 32.05 | 33.321 | 33.409 | 34.857 |
| `surfaces/standard_surface/standard_surface_sweep_sheen_0_00` | 31.922 | 33.149 | 33.242 | 35.528 |
| `surfaces/gltf_pbr/input_emissive_strength` | 31.918 | 32.949 | 32.921 | 34.386 |
| `surfaces/gltf_pbr/input_iridescence` | 31.869 | 32.509 | 32.56 | 31.408 |

`image_format_avif/svg/webp` score about 10 dB for **every** renderer: their reference
is itself a failed texture load.

## What still stands between crust and the reference

1. **Pattern nodes `crust-mtlx`'s compiler has no operator for.** This is now the
   dominant gap. An unknown node degrades its input to a constant and is reported;
   the render log names it (`results.json`), and `summarize.py` also scans the
   documents statically, since the compiler never reaches the graph upstream of a
   node it cannot compile. Materials affected, one example each:

| node | materials | example |
|---|---|---|
| `separate2` | 266 | `absval` |
| `fract` | 217 | `absval` |
| `range` | 167 | `artistic_ior_aluminium` |
| `ifgreater` | 146 | `absval` |
| `combine4` | 142 | `absval` |
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

   `colorcorrect` (10 materials) and `heighttonormal` (5) have since gained
   operators (`openspec/specs/materials/design.md`); the suite has not been re-run
   to measure them, and the table above predates that change.

   Plus: `splittb` (11), `fractal2d` (10), `rotate3d` (10), `worleynoise2d` (10), `worleynoise3d` (10), `gltf_normalmap` (8), `transformnormal` (8), `noise2d` (7), `noise3d` (7), `rotate2d` (7), `ramptb` (6), `cellnoise2d` (5), `cellnoise3d` (5), `checkerboard` (5), `gltf_image` (5), `transformvector` (5), `ifequal` (5), `determinant` (4), `ramplr` (4), `circle` (3), `transpose` (3), `switch` (3), `blackbody` (2), `ifgreatereq` (2), `bump` (2), `burn` (2), `difference` (2), `dodge` (2), `hsvtorgb` (2), `and` (2), `or` (2), `xor` (2), `invertmatrix` (2), `minus` (2), `ramp` (2), `ramp4` (2), `rgbtohsv` (2), `unpremult` (2), `gltf_colorimage` (2), `open_pbr_anisotropy` (2), `crossproduct` (1), `distance` (1), `frame` (1), `hextilednormalmap` (1), `not` (1), `overlay` (1), `ramp_gradient` (1), `reflect` (1), `refract` (1), `round` (1), `safepower` (1), `saturate` (1), `screen` (1), `tan` (1), `time` (1), `transformpoint` (1), `gltf_iridescence_thickness` (1).

   The first five (`separate2`, `fract`, `range`, `ifgreater`, `combine4`) are the
   suite's harness nodes: its node-isolation materials wrap the node under test in a
   shared UV-driven graph, so those five gate hundreds of `nodes/*` samples whatever
   node they isolate. (`ifgreater` exists inside the surface builders, built from
   existing ops, but not yet as a document node.) The run also found one silent
   pattern gap, now fixed: a `tiledimage`'s `uvtiling` / `uvoffset` was read only as a
   literal, so a connected one (`textured` feeds it through `convert`) fell back to
   identity. It now folds, and `textured` went from 17.9 to 28.4 dB.
2. **What the closure tree reports rather than renders**, per the change's spec:
   - opacity / alpha has no cutout: `input_alpha_mode_mask` (−17.7 dB against
     `blender-new`), `input_alpha_cutoff` (−12.6), `opacity_mask` (−11.7) and
     `alpha_mode_mask` (−8.7) are the largest shading shortfalls in the suite;
   - OpenPBR fuzz is Zeltner sheen, evaluated as Imageworks / Charlie: the fuzz sweeps
     sit 5–8 dB below `blender-new` (`sweep_fuzz_roughness_0_25` −8.4,
     `input_fuzz_sheenlike` −5.6, `velvet` −5.3);
   - anisotropy rotation. (Subsurface was on this list: `subsurface_bsdf` is a
     random walk now, which this run predates.)
3. **Two more pattern gaps worth naming.** `greysphere_calibration` (−6.6 dB)
   places its colour chart with `place2d`, which crust does not have; the
   `gltf_*` image samples use the `gltf_image` / `gltf_colorimage` /
   `gltf_normalmap` wrappers, which are pattern nodes crust does not have yet.
