#!/usr/bin/env python3
"""Generate a Moana-shaped USD scene for benchmarking the *importer*.

The Moana Island Scene is not a triangle-count problem so much as a
*prim*-count problem: a deep Xform hierarchy, tens of thousands of mesh
prims that mostly repeat a handful of distinct archives, and
PointInstancers scattering those archives hundreds of thousands of times.
This generator reproduces that shape at a size that fits in a laptop's
RAM, so `--stats` numbers move for the same reasons they move on Moana.

Layout:
  - `UNIQUE` distinct grid meshes, each ~`RES*RES*2` triangles;
  - `ELEMENTS` element groups, each a 3-level Xform chain holding
    `MESHES_PER_ELEMENT` mesh prims that re-author one of the unique
    meshes verbatim (so the importer's content-hash dedup is exercised);
  - `FILLER_XFORMS` empty Xform prims per element (traversal overhead);
  - `INSTANCERS` PointInstancers, each scattering a class prototype
    `PLACEMENTS` times.

Usage: gen_moana_like_scene.py [out.usda] [scale]
  scale is a float multiplier on the counts (default 1.0).
The scene is deliberately NOT checked in.
"""

import math
import random
import sys

RES = 12                 # grid resolution -> 2*RES*RES tris, (RES+1)^2 points
UNIQUE = 60              # distinct archive meshes
ELEMENTS = 40            # element groups
MESHES_PER_ELEMENT = 25  # mesh prims per element
FILLER_XFORMS = 300      # empty Xform prims per element
INSTANCERS = 6
PLACEMENTS = 40000       # per instancer


def grid_mesh(seed, res=RES):
    """A displaced grid: points, faceVertexCounts, faceVertexIndices."""
    rng = random.Random(seed)
    pts = []
    for j in range(res + 1):
        for i in range(res + 1):
            x = i / res - 0.5
            z = j / res - 0.5
            y = 0.25 * math.sin(6.0 * x + seed) * math.cos(6.0 * z) + rng.uniform(-0.02, 0.02)
            pts.append((x, y, z))
    counts, idx = [], []
    for j in range(res):
        for i in range(res):
            a = j * (res + 1) + i
            b = a + 1
            c = a + res + 2
            d = a + res + 1
            counts += [3, 3]
            idx += [a, b, c, a, c, d]
    return pts, counts, idx


def fmt_points(pts):
    return ", ".join("(%.5f, %.5f, %.5f)" % p for p in pts)


def fmt_ints(v):
    return ", ".join(str(x) for x in v)


def mesh_body(pts, counts, idx, indent):
    pad = " " * indent
    return (
        f"{pad}point3f[] points = [{fmt_points(pts)}]\n"
        f"{pad}int[] faceVertexCounts = [{fmt_ints(counts)}]\n"
        f"{pad}int[] faceVertexIndices = [{fmt_ints(idx)}]\n"
    )


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "moana_like.usda"
    scale = float(sys.argv[2]) if len(sys.argv) > 2 else 1.0
    elements = max(1, int(ELEMENTS * scale))
    placements = max(1, int(PLACEMENTS * scale))

    rng = random.Random(7)
    archives = [grid_mesh(k) for k in range(UNIQUE)]

    w = open(out, "w")
    w.write('#usda 1.0\n(\n    upAxis = "Y"\n    defaultPrim = "World"\n)\n\n')

    # Render settings + camera + a light.
    w.write(
        'def RenderSettings "Render" {\n'
        "    int2 resolution = (320, 180)\n"
        "    custom int crust:samplesPerPixel = 1\n"
        "    custom int crust:maxDepth = 2\n"
        "}\n\n"
        'def Camera "Cam" {\n'
        "    float focalLength = 35\n"
        "    float horizontalAperture = 36\n"
        '    uniform token[] xformOpOrder = ["xformOp:translate"]\n'
        "    double3 xformOp:translate = (0, 40, 180)\n"
        "}\n\n"
        'def DistantLight "Sun" {\n'
        "    float inputs:intensity = 3\n"
        '    uniform token[] xformOpOrder = ["xformOp:rotateXYZ"]\n'
        "    double3 xformOp:rotateXYZ = (-45, 20, 0)\n"
        "}\n\n"
    )

    # Materials.
    w.write('def Scope "mtl" {\n')
    for m in range(8):
        w.write(
            f'    def Material "m{m}" {{\n'
            f'        token outputs:surface.connect = </mtl/m{m}/s>\n'
            f'        def Shader "s" {{\n'
            f'            uniform token info:id = "UsdPreviewSurface"\n'
            f"            color3f inputs:diffuseColor = ({0.2 + 0.1 * m:.2f}, 0.5, 0.3)\n"
            f"            float inputs:roughness = 0.5\n"
            f"        }}\n"
            f"    }}\n"
        )
    w.write("}\n\n")

    w.write('def Xform "World" {\n')

    # --- Element groups: deep-ish hierarchy, repeated archive geometry ---
    for e in range(elements):
        w.write(f'    def Xform "element_{e}" {{\n')
        w.write('        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:scale"]\n')
        w.write(
            f"        double3 xformOp:translate = ({rng.uniform(-200, 200):.3f}, 0, {rng.uniform(-200, 200):.3f})\n"
        )
        w.write("        double3 xformOp:scale = (1, 1, 1)\n")
        w.write(f'        def Xform "xgGeneratedInstances" {{\n')
        for k in range(MESHES_PER_ELEMENT):
            pts, counts, idx = archives[(e * MESHES_PER_ELEMENT + k) % UNIQUE]
            w.write(f'            def Xform "grp_{k}" {{\n')
            w.write(
                '                uniform token[] xformOpOrder = ["xformOp:translate"]\n'
                f"                double3 xformOp:translate = ({rng.uniform(-20, 20):.3f}, {rng.uniform(0, 8):.3f}, {rng.uniform(-20, 20):.3f})\n"
            )
            w.write(f'                def Mesh "mesh_{k}" (\n')
            w.write(f"                    prepend apiSchemas = [\"MaterialBindingAPI\"]\n")
            w.write("                ) {\n")
            w.write(f"                    rel material:binding = </mtl/m{k % 8}>\n")
            w.write(
                '                    uniform token[] xformOpOrder = ["xformOp:scale"]\n'
                "                    double3 xformOp:scale = (10, 10, 10)\n"
            )
            w.write(mesh_body(pts, counts, idx, 20))
            w.write("                }\n")
            w.write("            }\n")
        w.write("        }\n")
        # Filler Xforms: pure traversal cost, exactly like Moana's
        # xgGeneratedInstances scopes.
        w.write('        def Xform "scopes" {\n')
        for f in range(FILLER_XFORMS):
            w.write(f'            def Xform "s_{f}" {{}}\n')
        w.write("        }\n")
        w.write("    }\n")

    w.write("}\n\n")

    # --- PointInstancers over class prototypes ---
    w.write('class Xform "_protos" {\n')
    for p in range(INSTANCERS):
        pts, counts, idx = archives[p % UNIQUE]
        w.write(f'    class Xform "proto_{p}" {{\n')
        for part in range(2):
            w.write(f'        class Mesh "part_{part}" (\n')
            w.write('            prepend apiSchemas = ["MaterialBindingAPI"]\n')
            w.write("        ) {\n")
            w.write(f"            rel material:binding = </mtl/m{(p + part) % 8}>\n")
            w.write(mesh_body(pts, counts, idx, 12))
            w.write("        }\n")
        w.write("    }\n")
    w.write("}\n\n")

    for p in range(INSTANCERS):
        w.write(f'def PointInstancer "scatter_{p}" {{\n')
        w.write(f"    rel prototypes = [</_protos/proto_{p}>]\n")
        w.write("    int[] protoIndices = [" + fmt_ints([0] * placements) + "]\n")
        pos = [
            (rng.uniform(-400, 400), rng.uniform(0, 3), rng.uniform(-400, 400))
            for _ in range(placements)
        ]
        w.write("    point3f[] positions = [" + fmt_points(pos) + "]\n")
        w.write("}\n\n")

    w.close()

    tris = 2 * RES * RES
    print(f"wrote {out}")
    print(f"  unique archives     : {UNIQUE} x {tris} tris")
    print(f"  mesh prims          : {elements * MESHES_PER_ELEMENT}")
    print(f"  filler xforms       : {elements * FILLER_XFORMS}")
    print(f"  instancer placements: {INSTANCERS * placements}")


if __name__ == "__main__":
    main()
