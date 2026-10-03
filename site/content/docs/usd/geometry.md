+++
title = "Geometry"
description = "Per-prim ray visibility and transform motion blur."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-01T08:00:00+00:00
draft = false
weight = 30
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Two attributes on geometry prims: <code>crust:rayMask</code> chooses which rays see a prim, and <code>crust:motion:translate</code> moves it during the shutter.'
toc = true
top = false
+++

## crust:rayMask

`int`, default **7** (every ray type).

Which kinds of ray see this prim. The value is a sum of bits:

| bit | value | ray type |
|-----|-------|----------|
| 0 | 1 | **camera** rays: the prim appears in the image |
| 1 | 2 | **shadow** rays: the prim casts shadows |
| 2 | 4 | **indirect** rays: the prim appears in reflections and refractions, and bounces light |

Common values:

| value | effect |
|-------|--------|
| `7` | visible to everything (the default) |
| `6` | invisible to the camera, still casts shadows and bounces light: a light blocker or bounce card |
| `5` | casts no shadows |
| `1` | seen by the camera only: no shadows, not in reflections |
| `0` | invisible to every ray |

Read on `Mesh`, `Sphere` and `BasisCurves` prims, including prims inside instance
prototypes. On a light, it replaces the light's default visibility; see
[Lights](@/docs/usd/lights.md#crust-raymask-on-a-light).

```usda
# A card that blocks light but doesn't appear in the image.
def Mesh "Blocker"
{
    int crust:rayMask = 6
    # ...
}
```

Use only bits 0–2. Crust Render uses the higher bits for UsdLux shadow linking
(`collection:shadowLink`), and logs a warning if it has to overwrite bits you authored.

## crust:motion:translate

`float3`, default **none** (no motion).

A world-space translation that the prim moves through while the shutter is open, for
transform motion blur. The prim starts at its authored position when the shutter opens and
has moved by this vector when it closes.

Read on `Mesh` and `Sphere` prims.

```usda
# A sphere that moves one unit to the right during the shutter.
def Sphere "Mover"
{
    double radius = 0.6
    float3 crust:motion:translate = (1, 0, 0)
    double3 xformOp:translate = (-1.5, 0.6, 0)
    uniform token[] xformOpOrder = ["xformOp:translate"]
}
```

This is the only kind of motion blur. There is no deformation (per-vertex) blur, and
animated `xformOp`s are read at the render frame only: they don't blur. A mesh with a non-invertible transform (for example, a scale of zero on
one axis) can't move. Its `crust:motion:translate` is ignored with a warning.

`samples/motionblur.usda` shows both `crust:motion:translate` and `crust:rayMask = 6`.

## crust:displacementBound

`float`, local units, on a `Mesh` prim or on its `Material`. The largest distance the
material's [displacement](@/docs/usd/materials.md#displacement) can move a vertex. The
mesh prim's value wins. Next comes RenderMan's `primvars:displacementbound:sphere` on the
mesh prim (the Moana Island authors it), then the material's `crust:displacementBound`.

Only adaptive subdivision (`--subdiv-edge-length`) reads it. Geometry outside the camera's
view is diced coarsely, and displacement can push geometry into view. So the view test
grows each mesh's box by this bound. A constant displacement needs none, since its bound
is exact. A textured displacement with no bound skips the view test altogether, so it
is diced by distance alone, in view or not, and `--stats` counts it as
"frustum test skipped".

A bound that is too small is not enforced: the full offset is still applied, and one
warning names the mesh.

```usda
def Mesh "Cliff" (prepend apiSchemas = ["MaterialBindingAPI"])
{
    float crust:displacementBound = 0.5
}
```

## Standard geometry attributes

Crust Render reads these standard `UsdGeom` attributes without any `crust:` attribute.

| attribute | effect |
|-----------|--------|
| `subdivisionScheme` | any value other than `none` makes a mesh a subdivision mesh. Unauthored means `catmullClark`. The refinement level comes from [`crust:subdivisionLevel`](@/docs/usd/render-settings.md#crust-subdivisionlevel). A `none` mesh whose material [displaces](@/docs/usd/materials.md#displacement) it is refined as `bilinear`: its faces stay flat until displaced. Its hard edges then soften, since the displaced mesh shades with smooth normals. |
| `visibility`, `purpose` | a prim with `visibility = "invisible"` is skipped with its whole subtree. Prims with purpose `guide` or `proxy` are skipped. |
| `primvars:st` | UV coordinates for UV and UDIM textures |
| `PointInstancer` and native instancing | prototypes are stored once and placed per instance |
