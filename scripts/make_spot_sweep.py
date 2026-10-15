#!/usr/bin/env python3
"""Write the cone-aware light sampling sweep: one small USDA stage per case.

    scripts/make_spot_sweep.py <out_dir>

openspec/changes/cone-aware-light-sampling (design D1). Every frame is a 40 x 40
floor, diffuse (UsdPreviewSurface, roughness 1) or glossy (metallic 1, roughness
0.25), under one light 1 unit above it, facing down, `normalize`d at intensity 8,
seen at 128 x 96 by a camera looking down at 45 degrees. That is the setup of the
archived solid-angle disk and tube sweep, so its unshaped disk rows are repeated
here like for like.

- Shaped rows: {rect, disk} x ratio x cone x (softness, focus) x floor. `ratio`
  is the light's half-size (a disk's radius, half a square rect's side) over
  tan(cone), i.e. over the cutoff radius on the floor straight below the light:
  0.1 is a small source whose beam is ten times wider than it, 3 a wide source
  of which only a third of the width emits toward any floor point.
- Unshaped rows: disks of radius 0.1, 0.3, 1, 3, on both floors.

`crust:minSamplesPerPixel` is 100000, so a 1024 spp reference never stops a
pixel early, while a 16 spp render takes exactly 16 everywhere. Files are named
by their parameters; the script prints how many it wrote.
"""

import math
import sys
from pathlib import Path

RATIOS = [0.1, 0.5, 1, 3]
CONES = [10, 30, 60]
SHAPINGS = [(0.0, 0.0), (0.4, 2.0)]  # (softness, focus)
FLOORS = {
    "diffuse": "float inputs:metallic = 0\n                float inputs:roughness = 1",
    "glossy": "float inputs:metallic = 1\n                float inputs:roughness = 0.25",
}
UNSHAPED_DISK_RADII = [0.1, 0.3, 1, 3]

HEADER = """#usda 1.0
(
    doc = "make_spot_sweep.py: {doc}"
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
    def Camera "Cam"
    {{
        float focalLength = 24
        float horizontalAperture = 36
        double3 xformOp:translate = (0, 4, 4)
        float xformOp:rotateX = -45
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateX"]
    }}

    def Scope "Materials"
    {{
        def Material "Floor"
        {{
            token outputs:surface.connect = </World/Materials/Floor/Surface.outputs:surface>

            def Shader "Surface"
            {{
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.5, 0.5, 0.5)
                {floor}
                token outputs:surface
            }}
        }}
    }}

    def Mesh "Floor" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-20, 0, -20), (-20, 0, 20), (20, 0, 20), (20, 0, -20)]
        rel material:binding = </World/Materials/Floor>
    }}

"""

FOOTER = """}

def Scope "Render"
{
    def RenderSettings "settings"
    {
        int2 resolution = (128, 96)
        int crust:samplesPerPixel = 16
        int crust:minSamplesPerPixel = 100000
        int crust:maxDepth = 6
        int crust:frame = 1
    }
}
"""

# Every light sits 1 unit up, its local -Z turned onto -Y (facing the floor).
PLACEMENT = """        double3 xformOp:translate = (0, 1, 0)
        float xformOp:rotateX = -90
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateX"]"""


def light(kind, half, shaping):
    """A rect or disk light of half-size `half`, optionally shaped."""
    lines = []
    if kind == "rect":
        lines += [f"float inputs:width = {2 * half:.6g}", f"float inputs:height = {2 * half:.6g}"]
        schema = "RectLight"
    else:
        lines += [f"float inputs:radius = {half:.6g}"]
        schema = "DiskLight"
    lines += ["bool inputs:normalize = 1", "float inputs:intensity = 8"]
    api = ""
    if shaping is not None:
        cone, softness, focus = shaping
        api = ' (\n        prepend apiSchemas = ["ShapingAPI"]\n    )'
        lines += [f"float inputs:shaping:cone:angle = {cone}"]
        if softness:
            lines += [f"float inputs:shaping:cone:softness = {softness}"]
        if focus:
            lines += [f"float inputs:shaping:focus = {focus}"]
    body = "\n".join("        " + l for l in lines)
    return f'    def {schema} "Light"{api}\n    {{\n{body}\n{PLACEMENT}\n    }}\n'


def stage(doc, floor, light_text):
    return HEADER.format(doc=doc, floor=FLOORS[floor]) + light_text + FOOTER


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    written = 0
    for kind in ("rect", "disk"):
        for ratio in RATIOS:
            for cone in CONES:
                half = ratio * math.tan(math.radians(cone))
                for softness, focus in SHAPINGS:
                    for floor in FLOORS:
                        name = f"{kind}_k{ratio:g}_c{cone}_s{softness:g}_f{focus:g}_{floor}"
                        doc = f"{kind}, half-size {half:.4g} = {ratio:g} tan {cone} deg, softness {softness:g}, focus {focus:g}, {floor} floor"
                        text = stage(doc, floor, light(kind, half, (cone, softness, focus)))
                        (out / f"{name}.usda").write_text(text)
                        written += 1
    for radius in UNSHAPED_DISK_RADII:
        for floor in FLOORS:
            name = f"disk_r{radius:g}_unshaped_{floor}"
            doc = f"unshaped disk, radius {radius:g}, {floor} floor"
            (out / f"{name}.usda").write_text(stage(doc, floor, light("disk", radius, None)))
            written += 1
    print(f"{written} stages in {out}")


if __name__ == "__main__":
    main()
