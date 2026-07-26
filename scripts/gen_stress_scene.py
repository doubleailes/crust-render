#!/usr/bin/env python3
"""Generate a traversal-heavy stress scene for benchmarking the BVH.

Layout (all sizes in scene units, Y up):
  - a 6x6 grid of identical ~9.8k-triangle UV spheres, every prim authoring
    the same points/indices and binding the same material, so the importer's
    instancing dedup collapses them to one shared BVH (~353k triangles when
    world-baked instead);
  - one 30k-triangle field of long, thin, randomly-oriented shards -- the
    overlapping-bounds geometry SBVH spatial splits exist for;
  - a floor, a RectLight overhead, and a thin occluder slab between the
    light and the center of the field so a large fraction of NEE shadow
    rays are occluded (exercising the early-exit occlusion query).

Usage: gen_stress_scene.py [out.usda]
The scene is deliberately NOT checked in -- it is ~12 MB of generated text.
"""

import math
import random
import sys

RINGS = 50      # UV sphere rings
SEGS = 100      # UV sphere segments -> 9800 tris, 4902 points
GRID = 6        # GRID x GRID sphere instances
SPACING = 2.5
SHARDS = 30000  # thin-triangle count
FIELD = 16.0    # shard field half-extent


def uv_sphere(radius):
    pts = [(0.0, radius, 0.0)]
    for r in range(1, RINGS):
        phi = math.pi * r / RINGS
        y = radius * math.cos(phi)
        s = radius * math.sin(phi)
        for k in range(SEGS):
            th = 2.0 * math.pi * k / SEGS
            pts.append((s * math.cos(th), y, s * math.sin(th)))
    pts.append((0.0, -radius, 0.0))
    last = len(pts) - 1

    def ring(r, k):
        return 1 + (r - 1) * SEGS + (k % SEGS)

    idx = []
    for k in range(SEGS):  # top fan
        idx += [0, ring(1, k + 1), ring(1, k)]
    for r in range(1, RINGS - 1):  # quads as triangle pairs
        for k in range(SEGS):
            a, b = ring(r, k), ring(r, k + 1)
            c, d = ring(r + 1, k), ring(r + 1, k + 1)
            idx += [a, b, d, a, d, c]
    for k in range(SEGS):  # bottom fan
        idx += [last, ring(RINGS - 1, k), ring(RINGS - 1, k + 1)]
    return pts, idx


def fmt_pts(pts):
    return ", ".join(f"({x:.5g}, {y:.5g}, {z:.5g})" for x, y, z in pts)


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "stress.usda"
    rng = random.Random(42)
    pts, idx = uv_sphere(0.8)
    counts = ", ".join(["3"] * (len(idx) // 3))
    tris_per_sphere = len(idx) // 3

    shard_pts, shard_idx = [], []
    for _ in range(SHARDS):
        x = rng.uniform(-FIELD, FIELD)
        z = rng.uniform(-FIELD, FIELD)
        ln = rng.uniform(1.0, 3.0)
        dx, dz = rng.uniform(-1, 1), rng.uniform(-1, 1)
        n = math.hypot(dx, dz) or 1.0
        dx, dz = dx / n * ln, dz / n * ln
        h = rng.uniform(0.3, 1.2)
        b = len(shard_pts)
        shard_pts += [(x, 0.0, z), (x + dx, h, z + dz), (x + dx * 0.9 + 0.03, h, z + dz * 0.9)]
        shard_idx += [b, b + 1, b + 2]
    shard_counts = ", ".join(["3"] * SHARDS)

    with open(out, "w") as f:
        f.write('#usda 1.0\n(\n    doc = "Generated BVH stress scene -- see scripts/gen_stress_scene.py"\n'
                '    defaultPrim = "World"\n    upAxis = "Y"\n)\n\n')
        f.write('def Xform "World"\n{\n')
        f.write('    def Camera "Cam"\n    {\n'
                '        float focalLength = 24\n'
                '        float horizontalAperture = 20.955\n'
                '        double3 xformOp:translate = (0, 9, 20)\n'
                '        float xformOp:rotateX = -22\n'
                '        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateX"]\n    }\n\n')

        f.write('    def Scope "Looks"\n    {\n'
                '        def Material "Grey"\n        {\n'
                '            token outputs:surface.connect = </World/Looks/Grey/Shader.outputs:surface>\n'
                '            def Shader "Shader"\n            {\n'
                '                uniform token info:id = "UsdPreviewSurface"\n'
                '                color3f inputs:diffuseColor = (0.55, 0.55, 0.6)\n'
                '                float inputs:roughness = 0.4\n'
                '                token outputs:surface\n            }\n        }\n    }\n\n')

        half = (GRID - 1) * SPACING * 0.5
        for i in range(GRID):
            for j in range(GRID):
                x, z = i * SPACING - half, j * SPACING - half
                f.write(f'    def Mesh "Ball_{i}_{j}" (prepend apiSchemas = ["MaterialBindingAPI"])\n    {{\n')
                f.write('        rel material:binding = </World/Looks/Grey>\n')
                f.write(f'        int[] faceVertexCounts = [{counts}]\n')
                f.write(f'        int[] faceVertexIndices = [{", ".join(map(str, idx))}]\n')
                f.write(f'        point3f[] points = [{fmt_pts(pts)}]\n')
                f.write(f'        double3 xformOp:translate = ({x}, 0.8, {z})\n')
                f.write('        uniform token[] xformOpOrder = ["xformOp:translate"]\n    }\n\n')

        f.write('    def Mesh "Shards"\n    {\n')
        f.write(f'        int[] faceVertexCounts = [{shard_counts}]\n')
        f.write(f'        int[] faceVertexIndices = [{", ".join(map(str, shard_idx))}]\n')
        f.write(f'        point3f[] points = [{fmt_pts(shard_pts)}]\n    }}\n\n')

        f.write('    def Mesh "Floor"\n    {\n'
                '        int[] faceVertexCounts = [4]\n'
                '        int[] faceVertexIndices = [0, 1, 2, 3]\n'
                '        point3f[] points = [(-40, 0, -40), (40, 0, -40), (40, 0, 40), (-40, 0, 40)]\n    }\n\n')

        f.write('    def Mesh "OccluderSlab"\n    {\n'
                '        int[] faceVertexCounts = [4]\n'
                '        int[] faceVertexIndices = [0, 1, 2, 3]\n'
                '        point3f[] points = [(-8, 6, -8), (8, 6, -8), (8, 6, 8), (-8, 6, 8)]\n    }\n\n')

        f.write('    def RectLight "Key"\n    {\n'
                '        float inputs:width = 12\n'
                '        float inputs:height = 12\n'
                '        color3f inputs:color = (6, 6, 6)\n'
                '        float inputs:intensity = 1\n'
                '        double3 xformOp:translate = (0, 12, 0)\n'
                '        float xformOp:rotateX = -90\n'
                '        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateX"]\n    }\n}\n\n')

        f.write('def Scope "Render"\n{\n    def RenderSettings "settings"\n    {\n'
                '        int2 resolution = (640, 360)\n'
                '        int crust:samplesPerPixel = 16\n'
                '        int crust:maxDepth = 6\n'
                '        int crust:minSamplesPerPixel = 16\n'
                '        float crust:varianceThreshold = 0\n'
                '        int crust:frame = 0\n    }\n}\n')

    total = GRID * GRID * tris_per_sphere + SHARDS + 4
    print(f"{out}: {GRID * GRID} spheres x {tris_per_sphere} tris + {SHARDS} shards"
          f" = {total} triangles (world-baked)")


if __name__ == "__main__":
    main()
