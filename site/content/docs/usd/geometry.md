+++
title = "Geometry"
description = "The geometry prims Crust Render reads, per-prim ray visibility and transform motion blur."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-13T08:00:00+00:00
draft = false
weight = 30
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Which geometry prims render, and two attributes on them: <code>crust:rayMask</code> chooses which rays see a prim, and <code>crust:motion:translate</code> moves it during the shutter.'
toc = true
top = false
+++

## Supported prims

| prim | renders as |
|------|------------|
| `Mesh` | triangles; a subdivision surface unless `subdivisionScheme = "none"` (see [standard attributes](#standard-geometry-attributes)) |
| `Sphere` | an exact sphere |
| `BasisCurves`, `NurbsCurves` | round tubes |
| `Cube`, `Plane`, `Cylinder`, `Cylinder_1`, `Cone`, `Capsule`, `Capsule_1` | triangles, tessellated as usdview draws them (below) |
| `PointInstancer`, `instanceable` prims | their prototypes, stored once and placed per instance |

Any other geometry type (`Points`, `NurbsPatch`, `HermiteCurves`,
`TetMesh`, and a UsdVol `Volume`, whose fields Crust Render does not read: author
[`crust:volume:*`](@/docs/usd/volumes.md) instead) renders nothing. It is reported with one
[`geom.unsupported_type`](@/docs/reference/warnings.md#geometry-types) warning per type,
which counts the prims.

### Implicit shapes

The implicit shapes are turned into the same meshes OpenUSD's GeomUtil library builds for
usdview's Storm renderer: the same points, faces and seam, and the same orientation for
each `axis`. They then behave like any `Mesh`: bound materials, `crust:rayMask`,
`crust:motion:translate`, light linking and instancing all apply. Their attributes,
read at the render frame:

| prim | attributes (default) |
|------|----------------------|
| `Cube` | `size` (2) |
| `Plane` | `width` (2), `length` (2), `axis` (`"Z"`, the plane's normal) |
| `Cylinder`, `Cone` | `radius` (1), `height` (2), `axis` (`"Z"`) |
| `Cylinder_1` | `radiusBottom` (1), `radiusTop` (1), `height` (2), `axis` (`"Z"`) |
| `Capsule` | `radius` (0.5), `height` (1, the straight part only), `axis` (`"Z"`) |
| `Capsule_1` | `radiusBottom` (0.5), `radiusTop` (0.5), `height` (1), `axis` (`"Z"`) |

- `Cube` and `Plane` render faceted. The curved shapes shade with smooth normals around
  their sides; a cylinder's flat caps keep their own flat normal.
- A `Cylinder_1` with `radiusTop = 0` is a cone. A `Capsule_1` with unequal radii has caps
  that meet its sloping side smoothly.
- The shapes are never subdivided, whatever `subdivisionScheme` or
  [`crust:subdivisionLevel`](@/docs/usd/render-settings.md#crust-subdivisionlevel) says.
- They have no texture coordinates, so a UV texture reads nothing on them. A Ptex
  file is addressed by the faces Crust Render generates, which no Ptex file is painted
  for.
- [Displacement](@/docs/usd/materials.md#displacement) is not applied to them: the shape
  renders undisplaced, with a
  [`geom.implicit_displacement`](@/docs/reference/warnings.md#geometry-types) warning.
- `doubleSided` is not read. Every surface is seen from both sides, as with every other
  prim.
- A prim carrying [`crust:volume:type`](@/docs/usd/volumes.md) is a volume, never a
  surface, even when it is a `Cube`.

### How many segments a curved shape gets

A cylinder, cone or capsule is a polygon around its axis, so its outline is made of
straight segments. Crust Render picks the number of segments for each shape from its size
on screen. It uses the smallest number that keeps the outline within half a pixel of the
true curve, between 16 and 256 segments. A capsule's end caps get a quarter as many rings,
and at least two.

This needs the render camera before the stage is read, so it applies only when the camera
is named with `--camera` or `RenderSettings.camera`. It doesn't need adaptive subdivision
to be turned on. Without a named camera, every curved shape gets 48 segments. So does a
shape inside a prototype that is placed more than once, since one mesh serves every
placement. A prototype placed once is sized for its placement only when
[adaptive subdivision](@/docs/usd/render-settings.md#crust-subdivisionedgelength) is on,
and a prototype reached through a native instance nested in another prototype always gets
48. A shape outside the camera's view gets 16. Under
[`CRUST_ADAPTIVE_FRUSTUM=0`](@/docs/reference/environment-variables.md#crust-adaptive-frustum)
it is sized by its distance instead, as if it were in view.

Up close a shape gets many triangles: a capsule that fills a 2K frame takes about 10 000.
`-l debug` logs how many shapes took each segment count.

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

Read on `Mesh`, `Sphere`, `BasisCurves`, `NurbsCurves` and the [implicit shapes](#implicit-shapes),
including prims inside instance prototypes. On a light, it replaces the light's default visibility; see
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

Read on `Mesh`, `Sphere` and the [implicit shapes](#implicit-shapes).

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

The blur can be turned off for the whole render with
[`disableMotionBlur`](@/docs/usd/render-settings.md#disablemotionblur) on the
`RenderSettings` prim (or its first `RenderProduct`). The prim then renders sharp at its
authored position, and the translation is still available to the
[`motionvector`](@/docs/usd/aovs.md#motion-vectors) AOV, so the blur can be added in
compositing instead.

`samples/motionblur.usda` shows both `crust:motion:translate` and `crust:rayMask = 6`.
`samples/motionvector.usda` renders the same kind of motion sharp, with its motion
vectors.

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
| `subdivisionScheme` | any value other than `none` makes a mesh a subdivision mesh. Unauthored means `catmullClark`. The refinement level comes from [`crust:subdivisionLevel`](@/docs/usd/render-settings.md#crust-subdivisionlevel). A `none` mesh whose material [displaces](@/docs/usd/materials.md#displacement) it is refined as `bilinear`: its faces stay flat until displaced. Its hard edges then soften, since the displaced mesh shades with smooth normals. The exception is a mesh with any non-quad face whose displacement reads Ptex: refining it would leave its triangles' children with no Ptex face, so it stays its cage, with a warning. |
| `visibility`, `purpose` | a prim with `visibility = "invisible"` is skipped with its whole subtree. Prims with purpose `guide` or `proxy` are skipped. |
| `primvars:st` | UV coordinates for UV and UDIM textures |
| `PointInstancer` and native instancing | prototypes are stored once and placed per instance. Both nest: a `PointInstancer` or an `instanceable` prim inside a prototype is imported too, its prototype still stored once. |
| `xformOp:*`, `xformOpOrder` | every `UsdGeomXformOp` kind composes (single- and three-axis rotations, `translateX`-style single-axis ops, `orient`, `transform`, `!invert!`, `:suffix` ops), on any `Xformable` prim type, as in USD itself: `!resetXformStack!` drops the parent's transform and every op listed before it, and ops authored on a prim that is not `Xformable` (an untyped prim, a `Scope`) are ignored. Every `Xformable` type composes its ops, including the ones of families Crust Render does not otherwise read (`SkelRoot`, `Skeleton`, `Volume` and its fields, `GenerativeProcedural`, `SpatialAudio`). A prim of a type Crust Render does not know (a plugin or studio schema) is not `Xformable` either: its ops are ignored, with an `xform.unknown_type` warning. An op name that is not a `UsdGeomXformOp` kind is read as identity, with a warning. |
| `BasisCurves` | round tubes of the authored `widths`: `linear`, or `cubic` with a `bezier`, `bspline` or `catmullRom` basis. Per-vertex widths follow the basis; per-curve and constant widths are read too. A hit on a curve shades along the strand, so a [hair material](@/docs/usd/materials.md#hair) sees its direction at every placement. `normals` (ribbons) and `wrap` (periodic curves) are not read: every curve is a round, open tube. |
| `NurbsCurves` | round tubes, read like `BasisCurves`: `points`, `curveVertexCounts`, `widths` (per-vertex widths follow the curve), one `order` per curve, and `count + order` `knots` per curve, clamped or not. Order 4 (cubic), 3 (quadratic) and 2 (linear) are read; a higher order is skipped with [`curves.unsupported_order`](@/docs/reference/warnings.md#curves-and-volumes). `ranges` trims each curve to its parameter interval (an empty interval renders nothing). Unequal `pointWeights` (rational curves) are approximated by polynomial spans that keep each span's ends, with `curves.rational_approximated`; weights that are not all positive and finite are ignored, with the same warning. |
