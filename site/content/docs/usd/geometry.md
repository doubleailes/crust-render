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
is named with `--camera` or `RenderSettings.camera`. When
[`crust:dicingCamera`](@/docs/usd/render-settings.md#crust-dicingcamera) names a camera,
the size on screen is read through that camera instead, named or not. It doesn't need adaptive subdivision
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
has moved by this vector when it closes. It is one displacement over the shutter, whatever
the shutter's length, including the default instantaneous one.

Read on `Mesh`, `Sphere`, the [implicit shapes](#implicit-shapes) and native instances
(an `instanceable` prim).

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

Time-sampled `xformOp`s are the other source of motion: with a
[camera shutter](@/docs/usd/render-settings.md#camera-shutter) authored, a prim whose
transform is animated moves from its shutter-open transform to its shutter-close one. On
a prim that has both, the translation is added on top: the prim ends the shutter at its
shutter-close transform, moved by `crust:motion:translate`. A mesh whose transform moves
by `(0, 1, 0)` over the shutter and authors `(1, 0, 0)` has every point move by
`(1, 1, 0)`.

There is no deformation (per-vertex) blur: animated `points` are read at the frame. A
mesh with a non-invertible transform (for example, a scale of zero on one axis) can't
move. Its `crust:motion:translate` is ignored with a warning, and so is its animated
transform ([`xform.motion_ignored`](@/docs/reference/warnings.md)).

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

## Curve attributes in materials

A material bound to `BasisCurves` or `NurbsCurves` can read where on its strand a hit
lies, and the curve primvars the groom authors, through MaterialX `geompropvalue`. That
is what root-to-tip ramps, faded tips and per-strand colour variation are built from.

| `geomprop` | type | value on a curve hit |
|------------|------|----------------------|
| `curve:v` | `float` | position along the whole strand, 0 at its first point and 1 at its last: the curve's parameter, so a `v` ramp agrees with a `vertex` primvar ramp on the same strand. A trimmed `NurbsCurves` runs 0 to 1 over its `ranges` |
| `curve:index` | `float` | the curve's index within its prim, from 0 |
| `curve:random` | `float` | a value in [0, 1) per curve: the same strand gets the same value on every frame and render, neighbouring curves and different prims get unrelated ones |
| any other name | as authored | the primvar of that name (`hairColor` reads `primvars:hairColor`) |

Anywhere else, on a mesh or a sphere, the `curve:` names give the node's `default`, and
any other name reads that geometry's own primvar (see
[primvars in materials](#primvars-in-materials)). A curve prim that does not author the
primvar, or authors one that can't be read, gives the `default` there too.

Curve primvars are read with any of these interpolations:

| interpolation | values | at a hit |
|---------------|--------|----------|
| `constant` | 1 | that value |
| `uniform` | one per curve | the curve's value |
| `varying` | one per span end | linear between the span's two ends |
| `vertex` | one per point | through the curve's basis, as its points are: a `bspline` ramp is a B-spline inside each span too |

The types read are `float`, `float2`, `float3`, `color3f` (converted from its
`colorSpace`) and `int` (read as a float), in any precision (`double3`, `half2`, …),
as an array or, for `constant`, a lone value; indexed primvars (`primvars:<name>:indices`)
are expanded. A `faceVarying` primvar, any other type, or an array whose length does
not match its interpolation is refused with
[`curves.unsupported_primvar`](@/docs/reference/warnings.md#curves-and-volumes).

`primvars:st` on a curve is its chart, read by `texcoord` and by images, as on a mesh.
A curve with no `st` reads the chart at (0, 0), without a warning. Images filter a curve's
chart by how fast it changes along the strand, as they filter a mesh's by its UV
density: a `vertex` or `varying` chart blurs to coarser mip levels with distance, while
one value per curve (`uniform` or `constant`) is read at the finest level.

The `curve:` names belong to Crust Render. A primvar authored under `primvars:curve:`
is not read, and is reported with
[`curves.reserved_primvar`](@/docs/reference/warnings.md#curves-and-volumes).

```usda
def BasisCurves "Groom" (prepend apiSchemas = ["MaterialBindingAPI"])
{
    uniform token type = "cubic"
    uniform token basis = "bspline"
    int[] curveVertexCounts = [6, 6]
    point3f[] points = [...]
    float[] widths = [0.02] (interpolation = "constant")
    color3f[] primvars:hairColor = [(0.3, 0.2, 0.1), (0.2, 0.15, 0.1)] (
        interpolation = "uniform"
    )
    rel material:binding = </World/Looks/Hair>
}
```

```xml
<geompropvalue name="root_colour" type="color3">
  <input name="geomprop" type="string" value="hairColor" />
</geompropvalue>
<geompropvalue name="along" type="float">
  <input name="geomprop" type="string" value="curve:v" />
</geompropvalue>
<mix name="ramp" type="color3">
  <input name="bg" type="color3" nodename="root_colour" />
  <input name="fg" type="color3" value="0.9, 0.8, 0.6" />
  <input name="mix" type="float" nodename="along" />
</mix>
```

`curve:random` is stable only while a prim keeps its curves in the same order, which a
simulation cache with constant topology does. If the order can change, author a stable
per-curve id as a `uniform` `int` primvar and use it as a `randomfloat`'s `in`, or its
`seed` (see [variation nodes](@/docs/usd/materials.md#variation-nodes)).

These attributes cost nothing unless a material reads them. A curve whose material reads
none of them carries no extra data, and renders as it did before they existed. One that
does carries a table of its spans and of the primvars it reads, decoded for that
material only; `--stats` reports its size as `curve strand tables`. A
[primvar AOV](@/docs/usd/aovs.md#primvars) gives a curve that authors its primvar a table
too, whatever its material reads.


## Primvars in materials

A material bound to a `Mesh` reads the mesh's primvars by name through MaterialX
`geompropvalue`, as a curve's material reads the groom's: `geomprop` `Alpha` reads
`primvars:Alpha`, with or without the `primvars:` prefix, converted to the node's type as
MaterialX's `convert` does (a `float` fills every channel of a `color3`, a wider value
keeps its leading channels). That is how per-point opacity, vertex colours, per-face ids
and `displayColor` reach a look.

| interpolation | values | at a hit |
|---------------|--------|----------|
| `constant` | 1 | that value |
| `uniform` | one per face | the face's value |
| `vertex`, `varying` | one per point | linear across each triangle of the face |
| `faceVarying` | one per face-vertex | linear across each triangle, discontinuous between faces |

The types read are those curves read: `float`, `float2`, `float3`, `color3f` (converted
from its `colorSpace`) and `int` (read as a float), in any precision, as an array or, for
`constant`, a lone value. Indexed primvars (`primvars:<name>:indices`) are expanded. A
subdivided mesh refines its `vertex` and `faceVarying` primvars with the surface, as it
refines `primvars:st`, and each refined face keeps its cage face's `uniform` value. A
displaced point reads the primvar of the point it was moved from.

A mesh that does not author the primvar gives the node's `default`, silently. One
authored with another type, an empty array or one whose length does not match its
interpolation, or indices that are out of range or not an `int[]` gives the `default`
too, with
[`mesh.unsupported_primvar`](@/docs/reference/warnings.md#meshes-subdivision-and-displacement).

`Sphere`, `Cube`, `Cylinder`, `Cone`, `Capsule` and `Plane` read `constant` primvars
only: their tessellation has no points or faces an authored array could address, so any
other interpolation is refused with the same warning.

```usda
def Mesh "Debris" (prepend apiSchemas = ["MaterialBindingAPI"])
{
    int[] faceVertexCounts = [...]
    int[] faceVertexIndices = [...]
    point3f[] points = [...]
    float[] primvars:Alpha = [...] (interpolation = "vertex")
    rel material:binding = </World/Looks/Debris>
}
```

```xml
<geompropvalue name="alpha" type="float">
  <input name="geomprop" type="string" value="Alpha" />
  <input name="default" type="float" value="1" />
</geompropvalue>
<!-- into the surface's opacity, or open_pbr_surface's geometry_opacity -->
```

Only the primvars a material names are read, and only on the prims it is bound to, so
a mesh whose material reads none pays nothing. A primvar is not inherited from an
ancestor prim or a `PointInstancer`, and a material that reads both a Ptex texture and
a primvar reads the primvar's `default`, with `mesh.unsupported_primvar`. See
[limitations](@/docs/architecture/limitations.md#materials-and-textures).
## Standard geometry attributes

Crust Render reads these standard `UsdGeom` attributes without any `crust:` attribute.

| attribute | effect |
|-----------|--------|
| `subdivisionScheme` | any value other than `none` makes a mesh a subdivision mesh. Unauthored means `catmullClark`. The refinement level comes from [`crust:subdivisionLevel`](@/docs/usd/render-settings.md#crust-subdivisionlevel). A `none` mesh whose material [displaces](@/docs/usd/materials.md#displacement) it is refined as `bilinear`: its faces stay flat until displaced. Its hard edges then soften, since the displaced mesh shades with smooth normals. The exception is a mesh with any non-quad face whose displacement reads Ptex: refining it would leave its triangles' children with no Ptex face, so it stays its cage, with a warning. |
| `visibility`, `purpose` | a prim with `visibility = "invisible"` is skipped with its whole subtree. Prims with purpose `guide` or `proxy` are skipped. |
| `primvars:st` | UV coordinates for UV and UDIM textures, on meshes and on [curves](#curve-attributes-in-materials) |
| `PointInstancer` and native instancing | prototypes are stored once and placed per instance. Both nest: a `PointInstancer` or an `instanceable` prim inside a prototype is imported too, its prototype still stored once. |
| `xformOp:*`, `xformOpOrder` | every `UsdGeomXformOp` kind composes (single- and three-axis rotations, `translateX`-style single-axis ops, `orient`, `transform`, `!invert!`, `:suffix` ops), on any `Xformable` prim type, as in USD itself: `!resetXformStack!` drops the parent's transform and every op listed before it, and ops authored on a prim that is not `Xformable` (an untyped prim, a `Scope`) are ignored. Every `Xformable` type composes its ops, including the ones of families Crust Render does not otherwise read (`SkelRoot`, `Skeleton`, `Volume` and its fields, `GenerativeProcedural`, `SpatialAudio`). A prim of a type Crust Render does not know (a plugin or studio schema) is not `Xformable` either: its ops are ignored, with an `xform.unknown_type` warning. An op name that is not a `UsdGeomXformOp` kind is read as identity, with a warning. |
| `BasisCurves` | round tubes of the authored `widths`: `linear`, or `cubic` with a `bezier`, `bspline` or `catmullRom` basis. Per-vertex widths follow the basis; per-curve and constant widths are read too. A hit on a curve shades along the strand, so a [hair material](@/docs/usd/materials.md#hair) sees its direction at every placement. `normals` (ribbons) and `wrap` (periodic curves) are not read: every curve is a round, open tube. |
| `NurbsCurves` | round tubes, read like `BasisCurves`: `points`, `curveVertexCounts`, `widths` (per-vertex widths follow the curve), one `order` per curve, and `count + order` `knots` per curve, clamped or not. Order 4 (cubic), 3 (quadratic) and 2 (linear) are read; a higher order is skipped with [`curves.unsupported_order`](@/docs/reference/warnings.md#curves-and-volumes). `ranges` trims each curve to its parameter interval (an empty interval renders nothing). Unequal `pointWeights` (rational curves) are approximated by polynomial spans that keep each span's ends, with `curves.rational_approximated`; weights that are not all positive and finite are ignored, with the same warning. |
