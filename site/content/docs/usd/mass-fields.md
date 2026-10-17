+++
title = "Curve mass fields"
description = "Bake a groom's strands into a mass field, and shade them with its colour and normal, with crust:mass:* attributes and the crust_mass_* MaterialX nodes."
date = 2026-10-17T08:00:00+00:00
updated = 2026-10-17T08:00:00+00:00
draft = false
weight = 55
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'A groom that authors <code>crust:mass:radius</code> is baked into a field around its strands, so two strands crossing at a point read the same colour and the same normal there.'
toc = true
top = false
+++

Stylised hair wants its colour designed into the *mass* of the hair, and its diffuse
lighting to follow the mass's surface rather than each strand's tube. Shading every strand
on its own breaks both wherever two strands cross: the front strand changes, and with it
the colour and the normal, so the crossing reads as a hard line. A mass field answers
both. At import, Crust Render splats the opted-in strands into a 3D field — their
coverage density, the signed distance to the surface where that coverage reaches a
threshold, and, when the material provides one, a colour — and a material reads the field
at every hit through four MaterialX nodes. Everything read off the field is a function of
position alone, so two strands crossing at a point agree there exactly.

The specular stays the strand's own: a [hair lobe](@/docs/usd/materials.md#hair) in the
same material keeps its tangent. How much of the mass a strand takes — everywhere, along
the strand, by depth, by density — is a `mix` in the material graph, so the field exposes
no look of its own.

## crust:mass:radius

`float crust:mass:radius`, on a `BasisCurves` or `NurbsCurves` prim. A positive value opts
the prim in and sets the splat radius, in the curves' own units, as their `widths` are: a
groom under a scaled `Xform` scales its mass with its strands (under a non-uniform scale
the radius is scaled by the cube root of the scale's determinant, the same in every
direction). Author it at about three times the spacing between neighbouring strands:
smaller, and the strands' own tubes come through; larger, and the mass loses its clumps.
The voxel is half the radius.

Unauthored, or `0`, the prim bakes nothing and renders exactly as it did before mass
fields existed. A negative or non-finite radius is refused with
[`mass.invalid_setting`](@/docs/reference/warnings.md#curve-mass-fields) and bakes nothing.

```usda
def BasisCurves "Groom" (prepend apiSchemas = ["MaterialBindingAPI"])
{
    uniform token type = "cubic"
    uniform token basis = "bspline"
    int[] curveVertexCounts = [...]
    point3f[] points = [...]
    float[] widths = [0.02] (interpolation = "constant")
    rel material:binding = </World/Looks/StylisedHair>
    float crust:mass:radius = 0.15
}
```

## crust:mass:threshold

`float crust:mass:threshold`, default `0.5`. Coverage is normalised so the centre line of
a lone straight strand as wide as the radius has coverage 1, and a strand twice as wide
2; the mass's surface lies where the coverage equals the threshold. A lower threshold
puts the surface further out, around the flyaways; a higher one hugs the dense core. A
non-positive or non-finite value takes the default, with `mass.invalid_setting`.

## crust:mass:group

`token crust:mass:group`. Curve prims authoring the same token under the same placement
bake one field together, so a groom split over several prims — a common export — is one
mass, and a strand of one prim crossing a strand of another reads the same normal at the
crossing. Prims under different placements never share a field: two characters' grooms
can carry the same token and stay apart. At the top level, a placement is the prims'
world transform (siblings under one `Xform` share one); inside an instance prototype it
is the prototype.

A group's radius, threshold and cap are its first prim's, in path order. A prim in the
group authoring other values is named by one
[`mass.group_conflict`](@/docs/reference/warnings.md#curve-mass-fields) warning and bakes
with the first prim's.

## crust:mass:maxResolution

`int crust:mass:maxResolution`, default `512`: the most voxels along any axis of the
field's bounds. The voxel is half the radius unless that would put more voxels than this
along an axis, in which case it grows until it does not (logged once at `DEBUG`). A value
below 1 takes the default, with `mass.invalid_setting`.

Only the regions the strands' coverage or the surface's distance band reach hold memory:
the field is kept in bricks of 8³ voxels, and `--stats` reports how many, their bytes and
the bake time (`curve mass fields`, and the `Bake mass fields` phase).

## Which prims are read

The attributes are read on `BasisCurves` and `NurbsCurves` only. On any other prim they
bake nothing and are reported once per prim with
[`mass.not_curves`](@/docs/reference/warnings.md#curve-mass-fields).

A field is baked once per frame from the strands at the frame's time code, and does not
move over the shutter: under motion blur the strands blur while their mass colour and
normal stay where the frame put them.

## The mass colour

A material provides the colour the bake splats through its `outputs:crust:mass:color`
terminal, connected to the `color3` output of a MaterialX shader prim (an `ND_*` node, as
in any [inline MaterialX network](@/docs/usd/materials.md#materialx-networks-in-usd)). The
graph is evaluated at samples along every strand the material is bound to, with the
sample's position and the strand's
[curve attributes](@/docs/usd/geometry.md#curve-attributes-in-materials) — `curve:v`,
`curve:index`, `curve:random` and the groom's primvars — and the field holds the
coverage-weighted mean of its values, so sparse coverage never darkens it. Where the
strands of two colours overlap the colour blends; well inside each, it is that strand's.

```usda
def Material "StylisedHair" (
    prepend references = @hair_mass.mtlx@</MaterialX/Materials/mass_hair>
)
{
    color3f outputs:crust:mass:color.connect = </World/Looks/StylisedHair/Ramp.outputs:out>
    def Shader "V"
    {
        uniform token info:id = "ND_geompropvalue_float"
        string inputs:geomprop = "curve:v"
        float outputs:out
    }
    def Shader "Ramp"
    {
        uniform token info:id = "ND_mix_color3"
        color3f inputs:bg = (0.35, 0.15, 0.05)
        color3f inputs:fg = (0.8, 0.5, 0.2)
        float inputs:mix.connect = </World/Looks/StylisedHair/V.outputs:out>
        color3f outputs:out
    }
}
```

The terminal does not change how the material shades. Without one, the field holds no
colour and `crust_mass_color` reads its `default`. A terminal connected to anything but a
MaterialX node, and a node in the graph that needs a view or a surface hit —
`viewdirection`, `normal`, `tangent`, `normalmap`, `heighttonormal` — are reported with
[`mass.color_unsupported`](@/docs/reference/warnings.md#curve-mass-fields); the terminal
is ignored, or that node reads its default.

## The lookup nodes

Four MaterialX nodes read the field of the geometry hit, at the hit's position. They are
defined in the crust node library shipped with the renderer
(`crates/crust-mtlx/libraries/crust/crust_defs.mtlx`), beside MaterialX's own.

| node | type | value at a hit on a curve with a field |
|------|------|---------------------------------------|
| `crust_mass_color` | `color3` | the mass colour: the coverage-weighted mean of the strands' colours around the point |
| `crust_mass_normal` | `vector3` | the mass normal, world space, unit length: the gradient of the signed distance — outside the mass it points away from the surface, inside it points toward the nearest point of the surface, so a strand deep in a clump shades as the clump's surface would |
| `crust_mass_depth` | `float` | how far inside the mass the point is, in world units; 0 outside |
| `crust_mass_density` | `float` | the strands' coverage: 1 on the centre line of a lone strand as wide as the radius |

Each has one input, `default`, which it returns wherever the field has nothing to say: on
a mesh, on a curve that bakes no field, on a field without a colour (`crust_mass_color`),
and where the field defines no normal — exactly on a symmetry axis such as a round
clump's centre line, or in a field whose coverage never reaches the threshold, which has
no surface at all (`crust_mass_normal`). No warning is raised: the material degrades to
per-strand shading. A material can read the nodes on any geometry.

The normal is the gradient of the signed distance after one smoothing pass, interpolated
between voxels, so it is continuous across the field. It is stable from frame to frame in
proportion to the radius against the strand spacing: at three spacings, a groom many
strands deep moves it by about a degree per frame of ordinary simulation jitter, a
one-layer sheet by a few. `crust_mass_density` is the signal to fade the mass out with
where it is thin — flyaways far from the mass would otherwise read a distant surface's
normal.

```xml
<crust_mass_normal name="mass_n" type="vector3">
  <input name="default" type="vector3" value="0, 0, 1" />
</crust_mass_normal>
<crust_mass_density name="mass_rho" type="float" />
<smoothstep name="fade" type="float">
  <input name="in" type="float" nodename="mass_rho" />
  <input name="low" type="float" value="0.1" />
  <input name="high" type="float" value="0.4" />
</smoothstep>
```

## The mass diffuse lobe

`crust_mass_diffuse_bsdf` (`weight`, `color`, `normal`) is a Lambertian lobe around the
normal it is given — the mass normal, in the use it is built for — rather than the
strand's tube. Its hemisphere is that normal's, and its light passes out of the strand it
starts in exactly as a `chiang_hair_bsdf` fibre's does, so a strand lit from behind its own
tube still lights as the mass's surface would. It keeps its normal whichever way it faces:
every other lobe ignores a normal facing away from the surface, but a mass normal
routinely faces away from the tube's (a strand on the far side of a clump, seen through a
gap), and that is exactly the discontinuity the field removes. Only a normal that is not
finite or has no length falls back to the hit's. It is a stylisation lobe: its normal is
not the surface's, so it is not reciprocal against the geometry. Under a uniform white
environment it returns `color`.

A typical stylised material layers the mass diffuse, with the mass colour faded by
density, under a hair fibre for the specular:

```xml
<crust_mass_color name="mass_c" type="color3">
  <input name="default" type="color3" value="0.3, 0.15, 0.05" />
</crust_mass_color>
<mix name="colour" type="color3">
  <input name="bg" type="color3" nodename="per_strand_colour" />
  <input name="fg" type="color3" nodename="mass_c" />
  <input name="mix" type="float" nodename="fade" />
</mix>
<mix name="normal" type="vector3">
  <input name="bg" type="vector3" nodename="tube_normal" />
  <input name="fg" type="vector3" nodename="mass_n" />
  <input name="mix" type="float" nodename="fade" />
</mix>
<crust_mass_diffuse_bsdf name="mass_diffuse" type="BSDF">
  <input name="color" type="color3" nodename="colour" />
  <input name="normal" type="vector3" nodename="normal" />
</crust_mass_diffuse_bsdf>
<layer name="hair" type="BSDF">
  <input name="top" type="BSDF" nodename="specular_fibre" />
  <input name="base" type="BSDF" nodename="mass_diffuse" />
</layer>
```

`samples/hair_mass.usda` is such a material on a groom, documented in the file. When a
mass lobe or a fibre is mixed with a lobe that reflects about a normal of its own (a normal
map, or the mass normal blended into an ordinary diffuse), the other lobe's light that
enters the tube meets the tube's far wall, as it would on its own — the rule that already
holds for a fibre beside a transmitting lobe; path guiding is off where such a mix is hit.
A mass lobe beside a fibre, or beside a diffuse on the hit's own normal, costs nothing
more than the fibre alone.

## Instanced grooms

A field is read at top-level curves, and at curves placed by a direct `instanceable`
instance whose transform is static and a rotation, translation and uniform scale: the
field is baked once in the prototype's space and read through each instance's transform,
its normal turned with the instance and its depth scaled by it. A groom placed any other
way — by a `PointInstancer`, through a prototype that groups many parts, with a non-uniform
scale — cannot name its placement at a hit, so its lookups return their `default`s and the
import raises one [`mass.unplaceable`](@/docs/reference/warnings.md#curve-mass-fields)
per prim. The field is still baked and reported.

## Edits in a session

In an MCP session, an edit of any `crust:mass:*` attribute, of a prim's group, or of the
material's mass colour graph re-imports the stage and bakes the affected fields again, so
the edited scene renders exactly as a fresh import of the same layers would. A material
edit that leaves the colour graph alone is applied in place.
