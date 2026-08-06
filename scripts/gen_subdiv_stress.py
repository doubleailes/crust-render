#!/usr/bin/env python3
"""Generate a subdivision-heavy stress scene for memory measurement.

The checked-in samples/subdivision.usda is far too small to move RSS, so
this emits quad cages big enough that the refiner's transient and the
refined mesh's resident cost stand well clear of page-granularity noise:

  - one GRID x GRID quad-grid cage authoring crust:subdivisionLevel = LEVEL
    (defaults: 128^2 = 16 384 cage quads, level 3 -> 1 048 576 refined quads
    -> 2 097 152 triangles), placed once so it takes the bake path;
  - two placements of an identical smaller cage (same points/topology/
    material, so the importer interns them to one slot) at the same LEVEL,
    exercising the committed-prototype/instance path;
  - a floor, a RectLight, a camera, and a tiny RenderSettings (memory, not
    render time, is the subject).

Measure with the --stats report, A/B'd by the kill switch:

    python3 scripts/gen_subdiv_stress.py /tmp/subdiv_stress.usda
    target/release/crust-render -i /tmp/subdiv_stress.usda --stats -l error
    CRUST_SUBDIV=0 target/release/crust-render -i /tmp/subdiv_stress.usda --stats -l error

Subdivision runs during traversal, so its transient lands in the `peak`
column of the `Traverse prims` row; the refined triangles' resident cost is
the `kernel memory` delta between the two runs.

Usage: gen_subdiv_stress.py [out.usda]
The scene is deliberately NOT checked in -- it is generated text.
"""

import sys

GRID = 128            # big cage: GRID x GRID quads, placed once (bake path)
INSTANCED_GRID = 32   # small cage: authored twice -> interned, instanced
LEVEL = 3             # crust:subdivisionLevel for every cage


def quad_grid(n, size):
    """(points, counts, indices) of an n x n quad grid spanning size units."""
    step = size / n
    pts = [
        (i * step - size / 2.0, 0.0, j * step - size / 2.0)
        for j in range(n + 1)
        for i in range(n + 1)
    ]
    counts = [4] * (n * n)
    idx = []
    for j in range(n):
        for i in range(n):
            v0 = j * (n + 1) + i
            idx += [v0, v0 + 1, v0 + n + 2, v0 + n + 1]
    return pts, counts, idx


def fmt_pts(pts):
    return ", ".join(f"({x:.4g}, {y:.4g}, {z:.4g})" for x, y, z in pts)


def mesh(name, n, size, translate, level):
    pts, counts, idx = quad_grid(n, size)
    return f"""
    def Mesh "{name}" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        int crust:subdivisionLevel = {level}
        uniform token subdivisionScheme = "catmullClark"
        point3f[] points = [{fmt_pts(pts)}]
        int[] faceVertexCounts = [{", ".join(map(str, counts))}]
        int[] faceVertexIndices = [{", ".join(map(str, idx))}]
        rel material:binding = </World/Materials/grey>
        double3 xformOp:translate = ({translate[0]}, {translate[1]}, {translate[2]})
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
"""


def main(out_path):
    parts = [
        f"""#usda 1.0
(
    doc = "Generated subdivision memory stress scene (scripts/gen_subdiv_stress.py): {GRID}x{GRID} cage + 2x {INSTANCED_GRID}x{INSTANCED_GRID} instanced cages, all at crust:subdivisionLevel = {LEVEL}."
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
    def Camera "Cam"
    {{
        float focalLength = 18
        float horizontalAperture = 20.955
        double3 xformOp:translate = (0, 10, 30)
        float xformOp:rotateX = -18
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateX"]
    }}

    def Scope "Materials"
    {{
        def Material "grey"
        {{
            token outputs:surface.connect = </World/Materials/grey/Surface.outputs:surface>
            def Shader "Surface"
            {{
                uniform token info:id = "crust:openpbr"
                color3f inputs:baseColor = (0.6, 0.6, 0.6)
                token outputs:surface
            }}
        }}
    }}
"""
    ]
    # The big cage, placed exactly once -> baked flat after flush_meshes.
    parts.append(mesh("BigCage", GRID, 20.0, (0, 2, 0), LEVEL))
    # Two placements of the identical small cage -> one interned slot, two
    # kernel instances of one committed scene.
    parts.append(mesh("SmallCageA", INSTANCED_GRID, 6.0, (-14, 2, 0), LEVEL))
    parts.append(mesh("SmallCageB", INSTANCED_GRID, 6.0, (14, 2, 0), LEVEL))
    parts.append(
        """
    def Mesh "Floor"
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-40, 0, -40), (40, 0, -40), (40, 0, 40), (-40, 0, 40)]
    }

    def RectLight "KeyLight"
    {
        float inputs:width = 20
        float inputs:height = 8
        color3f inputs:color = (5, 5, 5)
        float inputs:intensity = 1.0
        double3 xformOp:translate = (0, 18, 14)
        float xformOp:rotateX = -55
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateX"]
    }
}

def Scope "Render"
{
    def RenderSettings "settings"
    {
        int2 resolution = (320, 180)
        int crust:samplesPerPixel = 4
        int crust:maxDepth = 2
        int crust:minSamplesPerPixel = 4
        float crust:varianceThreshold = 0
        int crust:frame = 0
    }
}
"""
    )
    with open(out_path, "w") as f:
        f.write("".join(parts))
    n_refined = (GRID * GRID + 2 * INSTANCED_GRID * INSTANCED_GRID) * 4**LEVEL
    print(
        f"wrote {out_path}: {GRID * GRID} + 2x{INSTANCED_GRID * INSTANCED_GRID} cage quads "
        f"at level {LEVEL} -> {n_refined} refined quads ({2 * n_refined} triangles)"
    )


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "subdiv_stress.usda")
