#!/usr/bin/env python3
"""Convert the Material Fidelity suite's `ShaderBall.glb` into a `.usda` crust can load.

Crust reads USD only, so the suite's shader ball is converted once, outside the
renderer. The conversion applies the suite's scene contract: every node's TRS is
baked into the points, then the whole model is centred at the origin and scaled
so its bounding-box half-diagonal is 2.0 (MaterialXView's
`IDEAL_MESH_SPHERE_RADIUS`, which the Blender and Three.js renderers copy).

Texture coordinates are flipped to MaterialX's lower-left origin (`v = 1 - v`),
as MaterialX's own glTF loader does. Meshes stay triangles with
`subdivisionScheme = "none"`: the ball is already a render mesh.

Usage: glb_to_usda.py ShaderBall.glb shaderball.usda
"""
import struct
import sys

import numpy as np
import pygltflib

IDEAL_MESH_SPHERE_RADIUS = 2.0
COMPONENTS = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4}
DTYPES = {5121: np.uint8, 5123: np.uint16, 5125: np.uint32, 5126: np.float32}


def accessor(g, blob, index):
    a = g.accessors[index]
    view = g.bufferViews[a.bufferView]
    n = COMPONENTS[a.type]
    dtype = np.dtype(DTYPES[a.componentType])
    start = (view.byteOffset or 0) + (a.byteOffset or 0)
    stride = view.byteStride or n * dtype.itemsize
    if stride == n * dtype.itemsize:
        arr = np.frombuffer(blob, dtype, a.count * n, start)
    else:
        rows = [np.frombuffer(blob, dtype, n, start + i * stride) for i in range(a.count)]
        arr = np.concatenate(rows)
    return arr.reshape(a.count, n) if n > 1 else arr


def quat_matrix(q):
    x, y, z, w = q
    return np.array(
        [
            [1 - 2 * (y * y + z * z), 2 * (x * y - z * w), 2 * (x * z + y * w)],
            [2 * (x * y + z * w), 1 - 2 * (x * x + z * z), 2 * (y * z - x * w)],
            [2 * (x * z - y * w), 2 * (y * z + x * w), 1 - 2 * (x * x + y * y)],
        ]
    )


def node_matrix(node):
    m = np.eye(4)
    if node.matrix:
        return np.array(node.matrix, dtype=float).reshape(4, 4).T
    t = node.translation or [0, 0, 0]
    r = node.rotation or [0, 0, 0, 1]
    s = node.scale or [1, 1, 1]
    m[:3, :3] = quat_matrix(r) @ np.diag(s)
    m[:3, 3] = t
    return m


def walk(g, index, parent, out):
    node = g.nodes[index]
    world = parent @ node_matrix(node)
    if node.mesh is not None:
        out.append((node.name or f"mesh{index}", node.mesh, world))
    for child in node.children or []:
        walk(g, child, world, out)


def fmt(rows, n):
    if n == 1:
        return ", ".join(str(int(v)) for v in rows)
    return ", ".join("(" + ", ".join(f"{v:.7g}" for v in r) + ")" for r in rows)


def main(src, dst):
    g = pygltflib.GLTF2().load(src)
    blob = g.binary_blob()
    instances = []
    for root in g.scenes[g.scene or 0].nodes:
        walk(g, root, np.eye(4), instances)

    meshes = []
    for name, mesh_index, world in instances:
        for prim in g.meshes[mesh_index].primitives:
            if prim.mode not in (None, 4):
                raise SystemExit(f"{name}: only triangle primitives are supported")
            p = accessor(g, blob, prim.attributes.POSITION).astype(float)
            p = (world[:3, :3] @ p.T).T + world[:3, 3]
            normal_m = np.linalg.inv(world[:3, :3]).T
            n = (normal_m @ accessor(g, blob, prim.attributes.NORMAL).astype(float).T).T
            n /= np.linalg.norm(n, axis=1, keepdims=True)
            uv = accessor(g, blob, prim.attributes.TEXCOORD_0).astype(float).copy()
            uv[:, 1] = 1.0 - uv[:, 1]
            idx = accessor(g, blob, prim.indices).astype(np.int64)
            # glTF: a negative-determinant world transform flips the winding.
            # The points are baked, so the triangles are turned back here.
            if np.linalg.det(world[:3, :3]) < 0.0:
                idx = idx.reshape(-1, 3)[:, [0, 2, 1]].reshape(-1)
            meshes.append((name, p, n, uv, idx))

    lo = np.min([m[1].min(axis=0) for m in meshes], axis=0)
    hi = np.max([m[1].max(axis=0) for m in meshes], axis=0)
    radius = np.linalg.norm(hi - lo) * 0.5
    center = (lo + hi) * 0.5
    scale = IDEAL_MESH_SPHERE_RADIUS / radius

    with open(dst, "w") as f:
        f.write('#usda 1.0\n(\n    doc = "ShaderBall.glb from the Material Fidelity suite, '
                'normalised: centred, bounding-sphere radius 2."\n'
                '    defaultPrim = "ShaderBall"\n    upAxis = "Y"\n    metersPerUnit = 1\n)\n\n')
        f.write('def Xform "ShaderBall"\n{\n')
        for name, p, n, uv, idx in meshes:
            p = (p - center) * scale
            tris = len(idx) // 3
            f.write(f'    def Mesh "{name}"\n    {{\n')
            f.write('        uniform token subdivisionScheme = "none"\n')
            f.write('        uniform token orientation = "rightHanded"\n')
            f.write(f"        int[] faceVertexCounts = [{', '.join(['3'] * tris)}]\n")
            f.write(f"        int[] faceVertexIndices = [{fmt(idx, 1)}]\n")
            f.write(f"        point3f[] points = [{fmt(p, 3)}]\n")
            f.write(f"        normal3f[] normals = [{fmt(n, 3)}] (\n"
                    '            interpolation = "vertex"\n        )\n')
            f.write(f"        texCoord2f[] primvars:st = [{fmt(uv, 2)}] (\n"
                    '            interpolation = "vertex"\n        )\n')
            f.write("    }\n\n")
        f.write("}\n")
    print(f"wrote {dst}: {len(meshes)} meshes, scale {scale:.6g}, centre {center}")


if __name__ == "__main__":
    main(*sys.argv[1:3])
