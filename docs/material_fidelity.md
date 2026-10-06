# Material Fidelity suite: crust results

Ben Houston's [Material Fidelity suite](https://github.com/bhouston/material-fidelity)
renders 826 MaterialX materials on one shader ball under one fixed setup and scores
each renderer against MaterialXView's GLSL render (`materialx-glsl.avif`) by PSNR
over 8-bit RGB. This page is crust's run of the **full** suite with
`scripts/material_fidelity/fidelity.sh`, measured on 2026-09-29 at crust `37d4ee8`
(the random-walk `subsurface_bsdf`), material-fidelity `fe4de1e`,
mtlx-sample-library `8f8ce2e`. That run is `scripts/material_fidelity/baseline.json`.

**Headline: the surface groups score level with Blender.** Every document's
surface is a `standard_surface` (649), `gltf_pbr` (91) or `open_pbr_surface` (86)
node. Crust expands each one into the closure tree of its MaterialX nodegraph
and evaluates that tree with MaterialX's own layering (see
`openspec/specs/materials/design.md` § MaterialX). The `surfaces/*` groups score
24.1–24.4 dB against `blender-new`'s 24.6–25.1. The first run, at `2b80992`, read
none of these documents: every ball rendered with the fallback material, and the
suite mean was 14.44 dB. It is now 19.97 dB. What remains is mostly **pattern
nodes** crust's compiler has no operator for, not shading; the `nodes/*` group
(17.6 dB) is where that shows. The one shading model that moved since the
previous run, subsurface, moved *away* from both of the suite's references
(below).

## Running it

One script runs the whole suite, end to end:

```bash
scripts/material_fidelity/fidelity.sh            # init, build, run, report, check: ~80 min on 4 cores
```

| step | what it does |
|---|---|
| `init` | clones material-fidelity at the pinned revision into `.fidelity/` (gitignored), then the one submodule crust needs, `mtlx-sample-library` (~900 MB: documents, textures, every renderer's images), at the revision the suite pins, over https (its `.gitmodules` says SSH); creates a venv with `numpy`, `pillow>=11.3` (AVIF), `pygltflib`, `OpenEXR` |
| `build` | `cargo build --release -p crust-render` |
| `run [run.py args]` | renders and scores every material into `.fidelity/out/results.json`; `--materials noise3d` / `re:^input_` selects by leaf directory, as the suite's own CLI does |
| `report` | `summarize.py` → `.fidelity/out/report.md`: the tables below |
| `check [check.py args]` | fails if any material fell more than 0.5 dB below `scripts/material_fidelity/baseline.json`, errors where it rendered, or is no longer scored (its reference went missing) |
| `baseline` | accepts the run: rewrites `baseline.json` (commit it with the change that moved it). A `--materials` run is refused, since it would drop every unselected material from the gate; `baseline --partial` merges it in instead |
| `goldeneye [pytest args]` | the same fixtures through [Goldeneye](#through-goldeneye) |

`FIDELITY_ROOT` moves the work directory, `SUITE_REV` picks another suite
revision (bump `PINNED_SUITE_REV` and `baseline.json` together), `PYTHON` uses an
interpreter that already has the dependencies. Every step is also a plain script
(`run.py`, `summarize.py`, `check.py`, `goldeneye_suite.py`, each with `--help`)
over the helpers in `suite.py`.

`run.py` writes into each material directory what the suite's own renderers
write: `crust.avif` (encoded as the suite encodes every render, AVIF quality 90,
4:4:4), `crust.json` (its render report: status and log lines) and a `crust`
entry in `metrics.json`. `--no-write-suite` leaves the checkout alone. The suite's
viewer (`pnpm viewer`) lists only its built-in renderers, so showing crust there
takes one entry in its `packages/samples/src/built-in-renderers.ts`.
`results.json` keeps, per material, the PSNR of the AVIF (`psnr`, the number the
suite would publish) and of the same pixels before encoding (`psnr_lossless`), the
time, the render log and the nodes crust logged as unsupported.

The baseline is what makes the suite usable as a regression check. The score is
against an external reference, so an absolute threshold would fail hundreds of
materials for gaps already known (below); `check` fails only on a material that
moved *away* from the reference. A render is deterministic for a given binary,
so any movement is the change's.

### Through Goldeneye

[Goldeneye](https://github.com/anderslanglands/goldeneye) is the pytest-based USD
render regression runner behind Typhoon, NVIDIA's OpenUSD reference renderer. Its
AOUSD materials conformance suite (over 1200 tests) credits this Material
Fidelity suite (Anders Langlands, *A Reference Renderer for OpenUSD
Interoperability*).
`fidelity.sh goldeneye` exports the suite as a Goldeneye project in
`.fidelity/goldeneye` (`goldeneye_suite.py`), installs Goldeneye into the venv
without its Typhoon dependency, and runs `pytest` with a `crust` renderer
profile. Each case is scored by mean FLIP against the decoded `materialx-glsl`
reference (default threshold 0.1), with a FLIP map per case and an HTML report
(`goldeneye view` in that directory).

The fixtures are the same shot layer `run.py` renders, with relative asset paths
and a `RenderProduct` named after the case. So a Hydra renderer driven by
`usdrender --outputRoot` (Goldeneye's default Typhoon command) renders them
unchanged, for a side-by-side with a reference path tracer; it ignores the
`crust:*` attributes, so its ball shadows itself. `fidelity.sh goldeneye-accept`
turns the latest run's failures into per-case expected failures
(`<case>.goldeneye.toml`, naming the nodes crust logged as unsupported), which is
how Goldeneye carries known gaps. After that a run fails only on a case that
passed before.

The profile's render command is `goldeneye_suite.py render`, not `crust`
directly, for two reasons. crust does not create the output's directory, which
`usdrender` does. And FLIP's EXR reader (tinyexr) crashes on crust's tiled EXRs,
so the wrapper rewrites each one as a scanline EXR with the same pixels.

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
`maxDepth` 12 and `indirectClamp 0`: about 6 s a material. This is a score against
an external reference, so noise costs PSNR and it wants more samples, not the
16 spp that `check_images.sh` pins for bit-identity between two crust binaries.
Measured on 30 materials, 16 spp scores 0.34 dB lower on average than 64 spp, and
up to 1.37 dB lower on the materials that match best (`input_base_weight`,
`greysphere`). That is the noise, not the shading, and it is more than `check`'s
0.5 dB tolerance. The adaptive cascade the 16 spp rule guards against is harmless
here: `check` compares scores with a tolerance, not pixels for bit-identity, and
an unchanged binary still reproduces its images exactly.

## Results

PSNR in dB against `materialx-glsl.avif`, higher is better. 767 of 826 materials
ship a reference. The other columns are the suite's own published renders of the
same materials, scored by the same code (`materialx-osl` is MaterialX's OSL
backend, rendered by OSL's `testrender`). No render failed; no surface node is
logged as unsupported. The crust column scores `crust.avif`, as the suite scores
every renderer; before encoding the mean is 19.96 dB, so the codec costs crust
nothing measurable.

| group | n | crust mean | crust median | blender-new mean | blender-nodes mean | materialx-osl mean | threejs-new mean |
|---|---|---|---|---|---|---|---|
| **all** | 767 | 19.97 | 19.71 | 26.31 | 27.30 | 23.54 | 32.52 |
| nodes | 478 | 17.55 | 17.23 | 27.31 | 28.82 | 24.03 | 34.22 |
| showcase/gltf_pbr | 5 | 21.24 | 20.40 | 22.23 | 22.27 | 20.90 | 26.39 |
| showcase/open_pbr_surface | 8 | 21.94 | 21.49 | 22.73 | 22.77 | 22.48 | 26.63 |
| showcase/standard_surface | 16 | 21.72 | 20.04 | 24.54 | 26.16 | 22.76 | 30.89 |
| surfaces/gltf_pbr | 85 | 24.05 | 24.36 | 24.59 | 24.61 | 22.73 | 29.44 |
| surfaces/open_pbr_surface | 77 | 24.37 | 25.73 | 24.65 | 24.66 | 22.55 | 29.46 |
| surfaces/standard_surface | 98 | 24.27 | 25.42 | 25.07 | 25.14 | 22.96 | 30.39 |
| surface: `gltf_pbr` | 90 | 23.90 | 24.33 | 24.46 | 24.48 | 22.62 | 29.27 |
| surface: `open_pbr_surface` | 85 | 24.15 | 25.56 | 24.47 | 24.48 | 22.54 | 29.19 |
| surface: `standard_surface` | 592 | 18.78 | 18.42 | 26.86 | 28.14 | 23.82 | 33.50 |

Against the previous run (`a2a11cb`, material-samples `36422cb`, PNG images) no
group moved by more than 0.21 dB, except `showcase/open_pbr_surface`: 23.32 →
21.94, all of it from `ketchup` and `pearl`. The subsurface isolates account for
most of the 0.2 dB the `surfaces/open_pbr_surface` and `surfaces/standard_surface`
groups lost. See "Subsurface" below.

Lowest crust PSNR:

| material | crust | blender-new | blender-nodes | materialx-osl | threejs-new |
|---|---|---|---|---|---|
| `showcase/standard_surface/marble_solid` | 8.574 | 20.184 | 25.005 | 23.377 | 34.86 |
| `surfaces/standard_surface/marble_solid` | 8.574 | 20.184 | 25.005 | 23.377 | 34.86 |
| `nodes/artistic_ior_diag_red_white_edge` | 8.622 | 32.551 | 32.552 | 29.396 | 39.221 |
| `nodes/image_format_avif` | 8.871 | 9.755 | 9.756 | - | 9.929 |
| `nodes/image_format_svg` | 8.871 | 8.893 | 8.893 | 27.466 | 9.922 |
| `nodes/image_format_webp` | 8.871 | 9.743 | 9.743 | 10.021 | 9.916 |
| `nodes/asin_degenerate_out_of_domain_vector4` | 9.389 | 30.551 | 30.589 | 26.8 | 22.812 |
| `nodes/acos_degenerate_out_of_domain_vector4` | 9.437 | 30.962 | 30.994 | 26.294 | 22.831 |
| `surfaces/gltf_pbr/boombox` | 10.284 | 25.406 | 25.503 | 23.225 | 29.465 |
| `nodes/splittb_compare_center_shift` | 10.429 | 24.217 | 24.247 | 22.1 | 32.112 |

Highest crust PSNR:

| material | crust | blender-new | blender-nodes | materialx-osl | threejs-new |
|---|---|---|---|---|---|
| `surfaces/gltf_pbr/feature_emission` | 35.56 | 38.426 | 38.417 | 34.49 | 36.665 |
| `surfaces/standard_surface/greysphere` | 34.095 | 36.27 | 36.387 | 25.003 | 37.372 |
| `nodes/remap_degenerate_vector4` | 33.642 | 35.722 | 35.776 | 24.969 | 37.556 |
| `surfaces/standard_surface/feature_emission` | 33.565 | 35.698 | 35.774 | 31.809 | 38.603 |
| `surfaces/open_pbr_surface/input_base_weight` | 32.82 | 35.534 | 35.708 | 24.138 | 36.338 |
| `surfaces/standard_surface/input_emission` | 32.063 | 32.995 | 32.922 | 31.47 | 34.312 |
| `surfaces/gltf_pbr/input_emissive_strength` | 31.802 | 32.855 | 32.826 | 31.285 | 34.212 |
| `surfaces/standard_surface/graph_base_color_image_mask` | 31.74 | 33.121 | 33.216 | 25.531 | 36.704 |
| `surfaces/standard_surface/standard_surface_sweep_sheen_0_25` | 31.705 | 31.119 | 31.194 | 24.756 | 34.514 |
| `surfaces/standard_surface/standard_surface_sweep_sheen_0_00` | 31.702 | 32.978 | 33.061 | 24.932 | 37.128 |

`image_format_avif/svg/webp` score about 10 dB for **every** renderer (OSL aside on
`svg`): their reference is itself a failed texture load.

### Subsurface: the random walk moved away from both references

The baseline gate found this on its first use. Rendered with the commit before the
random walk (`a080d1a`), every subsurface material scores higher against
`materialx-glsl`:

| material | random walk | before (`a080d1a`) | change |
|---|---|---|---|
| `showcase/open_pbr_surface/ketchup` | 20.74 | 26.83 | +6.09 |
| `showcase/open_pbr_surface/pearl` | 21.82 | 26.67 | +4.85 |
| `surfaces/open_pbr_surface/feature_subsurface` | 20.10 | 24.87 | +4.77 |
| `surfaces/open_pbr_surface/input_subsurface_radius` | 20.33 | 24.87 | +4.54 |
| `surfaces/standard_surface/feature_subsurface` | 20.03 | 24.18 | +4.15 |
| `surfaces/open_pbr_surface/input_subsurface_scatter_anisotropy` | 21.17 | 24.87 | +3.70 |
| `surfaces/standard_surface/input_subsurface_scale` | 20.87 | 24.18 | +3.31 |
| `surfaces/standard_surface/input_subsurface_anisotropy` | 21.47 | 24.18 | +2.71 |
| `surfaces/standard_surface/input_subsurface_radius` | 21.70 | 24.18 | +2.48 |

The earlier model scores the same 24.18 / 24.87 whatever the radius, scale or
anisotropy: it ignored them, and so does the reference. Against `materialx-osl`
instead of the GLSL render the walk is also 1.6–7.5 dB lower than the earlier
model. `blender-new` (Cycles) is above both on `ketchup` and below both on
`pearl`, so it is no tie-breaker either. No render in the suite is
known to trace a subsurface random walk, so this score cannot decide which model
is right. It says only that the walk is not what these references show. What
would decide it is a random-walk reference: Cycles with its random-walk
subsurface forced on, or Typhoon.

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

   `colorcorrect` (10 materials) and `heighttonormal` (5) have operators now and are
   no longer on the list. The render log's own count (`report.md`) is lower than the
   static scan for the harness nodes (`separate2` 145, `fract` 113, `combine4` 11):
   the compiler stops at the first node it cannot compile, so it never reaches the
   rest of the graph.

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
   - opacity / alpha had no cutout: `input_alpha_mode_mask` (−17.3 dB against
     `blender-new`), `input_alpha_cutoff` (−12.4), `opacity_mask` (−11.6) and
     `alpha_mode_mask` (−8.6) were the largest shading shortfalls in the suite.
     Opacity is a stochastic cutout now (`openspec/changes/add-mtlx-cutout-and-rotation`),
     which this run predates;
   - OpenPBR fuzz is Zeltner sheen, evaluated as Imageworks / Charlie: the fuzz sweeps
     sit 5–8 dB below `blender-new` (`sweep_fuzz_roughness_0_25` −8.2,
     `input_fuzz_sheenlike` −5.5, `velvet` −5.2);
   - anisotropy rotation, applied since, like opacity.

   Subsurface is rendered now, as a random walk, and scores 2.5–6 dB below the
   model it replaced (see "Subsurface" above). That is a question about the
   references, not a closure the tree leaves out.
3. **Two more pattern gaps worth naming.** `greysphere_calibration` (−6.4 dB)
   places its colour chart with `place2d`, which crust does not have; the
   `gltf_*` image samples use the `gltf_image` / `gltf_colorimage` /
   `gltf_normalmap` wrappers, which are pattern nodes crust does not have yet.
