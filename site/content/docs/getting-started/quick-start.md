+++
title = "Quick Start"
description = "Build Crust Render and render a bundled sample."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-01T08:00:00+00:00
draft = false
weight = 20
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = "Build Crust Render, render a bundled sample, then render your own USD file."
toc = true
top = false
+++

## Requirements

- A 64-bit Linux, macOS or Windows machine.
- To build from source: Rust **1.96** or newer. The repository's `rust-toolchain.toml`
  selects **1.98.1**, the version CI builds with, and `rustup` installs it on the first
  `cargo` command. Dependencies are locked in the committed `Cargo.lock`.

## Installation

### Pre-built binaries

Each [GitHub release](https://github.com/doubleailes/crust-render/releases) carries
binaries for:

- `x86_64-unknown-linux-musl` (Linux)
- `x86_64-apple-darwin` (macOS)
- `x86_64-pc-windows-gnu` (Windows)

Unpack the archive and put the `crust` binary (`crust.exe` on Windows) on
your `PATH`.

### From source

```bash
git clone https://github.com/doubleailes/crust-render.git
cd crust-render
cargo build --release
# the binary is target/release/crust
```

Always build with `--release`. A debug build is many times slower.

By default the build includes the `jit` feature, which compiles MaterialX shading
programs to machine code with Cranelift. To build a renderer that only interprets them:

```bash
cargo build --release --no-default-features
```

The two builds render the same image. At run time, `CRUST_SHADER_JIT=0` also turns the JIT
off (see [Environment variables](@/docs/reference/environment-variables.md)).

## Your first render

The repository includes sample scenes in `samples/`:

```bash
# render the Cornell box: writes cornell.exr and cornell.png
cargo run --release -- render -i samples/cornellbox.usda -o cornell.exr

# or, with an installed binary
crust render -i samples/cornellbox.usda -o cornell.exr
```

The run prints four `INFO` lines: the resolution and sample count, the render time, and
the two images it wrote. The EXR holds linear radiance. The PNG is the same image tone-mapped to
sRGB.

Without `-i`, Crust Render draws a built-in procedural scene. This is useful to check that
the binary works:

```bash
crust render
```

## Common variations

```bash
# fewer samples for a quick preview
crust render -i scene.usda -s 16

# a given frame through a given camera
crust render -i shot.usda -f 1012 --camera /shot/cam/renderCam

# subdivide every subdivision-surface mesh twice
crust render -i character.usda --subdiv-level 2

# print timings, memory use and scene statistics at the end
crust render -i scene.usda --stats

# keep a full debug log of the run in ./logs/
crust render -i scene.usda -l debug --log-file logs
```

[Command line](@/docs/reference/command-line.md) describes every flag.

## Sample scenes

| scene | shows |
|-------|-------|
| `cornellbox.usda` | the classic test scene |
| `openpbr_showcase.usda` | `crust:openpbr` materials: metal, plastic, glass, coat, … |
| `materialx_showcase.usda` | MaterialX look-dev graphs |
| `veach_mis.usda` | the Veach multiple importance sampling test (try `--strategy light` and `--strategy bsdf`) |
| `cornellbox_guided.usda` | path guiding through `crust:pathGuiding` |
| `smoke.usda`, `fog.usda` | `crust:volume:*` volume regions |
| `motionblur.usda` | `crust:motion:translate` and `crust:rayMask` |
| `light_visibility.usda` | `crust:light:cameraVisible` |
| `light_linking.usda` | UsdLux light and shadow linking |
| `domelight.usda`, `dome_backdrop.usda` | dome lights and environment maps |
| `subdivision.usda` | subdivision surfaces |
| `ptex_quads.usda` | Ptex textures |
| `animation.usda` | time samples (try `-f`) |
| `instancing.usda`, `nested_instancing.usda` | USD instancing |

## Write your own scene

Crust Render reads any USD stage, so you can export from a DCC (Maya, Houdini, Blender,
…) or write `.usda` by hand. A minimal scene needs a camera, some geometry and a light.
This one adds a material and a `RenderSettings` prim that sets the resolution and sample
count. Save it as `first.usda` and run `crust render -i first.usda -o first.exr`:

```usda
#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{
    def Camera "cam"
    {
        float focalLength = 24
        double3 xformOp:translate = (0, 1.2, 7)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    def Mesh "floor"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-10, 0, 10), (10, 0, 10), (10, 0, -10), (-10, 0, -10)]
    }

    def Sphere "ball" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        double radius = 1
        rel material:binding = </World/Looks/red>
        double3 xformOp:translate = (0, 1, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    def DistantLight "sun"
    {
        bool inputs:normalize = 1
        float inputs:intensity = 3
        float inputs:angle = 2
        float3 xformOp:rotateXYZ = (-50, 30, 0)
        uniform token[] xformOpOrder = ["xformOp:rotateXYZ"]
    }

    def DomeLight "sky"
    {
        float inputs:intensity = 0.3
    }

    def Scope "Looks"
    {
        def Material "red"
        {
            token outputs:surface.connect = </World/Looks/red/Surface.outputs:surface>

            def Shader "Surface"
            {
                uniform token info:id = "crust:openpbr"
                color3f inputs:baseColor = (0.8, 0.1, 0.1)
                float inputs:specularRoughness = 0.2
                token outputs:surface
            }
        }
    }
}

def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/cam>
        int2 resolution = (960, 540)
        int crust:samplesPerPixel = 256
        int crust:maxDepth = 16
    }
}
```

[USD attributes](@/docs/usd/overview.md) lists everything you can set on the stage.
