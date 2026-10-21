+++
title = "Render passes"
description = "UsdRenderPass: one stage, several passes — holdouts (matte), camera and render visibility, chosen with --pass or rendered all at once with --all-passes."
date = 2026-10-21T08:00:00+00:00
updated = 2026-10-21T22:00:00+00:00
draft = false
weight = 27
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'A shot is often rendered in passes: the characters in one, the set in another. A <code>RenderPass</code> prim describes each; <code>--pass</code> renders one, and <code>--all-passes</code> every one from a single import, from the same stage, with no asset edits.'
toc = true
top = false
+++

A `RenderPass` prim describes one pass of a shot with collections: what the pass holds
out (`matte`), what the camera sees (`cameraVisibility`), and what renders at all
(`renderVisibility`). Crust Render reads all three, and the pass's `renderSource`, for the
pass you name on the command line, or for every pass of the stage with `--all-passes`.

## Choosing a pass

A render has an active pass only when you name one:

```bash
crust render -i shot.usda --pass /Render/Passes/char
```

With no [`--pass`](@/docs/reference/command-line.md#pass), no pass is active and the
stage renders exactly as it did before it defined any: the stage never chooses a pass
itself, so adding a second pass to a stage cannot change what an existing command line
renders. A path that is not a `RenderPass` prim stops the render with an error naming it.

[`crust ls pass`](@/docs/reference/command-line.md#ls) lists the stage's passes.
`crust check`, `crust diagnostic` and `crust light-paths` take `--pass` too, and so does a
[Claude Desktop session](@/docs/help/claude-desktop.md).

## renderSource

When the pass's `renderSource` targets a `RenderSettings` prim, that prim is the settings
the render reads: resolution, camera, products and every `crust:` setting. Each pass can
then write its own files at its own sample count. Without `renderSource`, the stage's
settings are read, as without a pass (see
[Which prim holds the render settings](@/docs/usd/overview.md#which-prim-holds-the-render-settings)).
A `renderSource` that targets anything else logs the warning
[`render_pass.render_source_invalid`](@/docs/reference/warnings.md#render-passes) and
reads the stage's settings.

### Overriding one setting for a pass

The settings prim is read as composed, so a pass can change one of the shot's settings
without restating the rest: give it a `RenderSettings` that `inherits` the shot's (or
`references` or `specializes` it) and authors only what differs. The usual case is a
utility pass that must not blur where the beauty does:

```usda
def RenderSettings "sharp" (
    inherits = </Render/settings>
)
{
    uniform bool disableDepthOfField = 1
    rel products = </Render/Products/util>
}

def RenderPass "util"
{
    rel renderSource = </Render/sharp>
}
```

The util pass renders through the shot's camera, at its resolution and sample count, as
a pinhole. A pass whose settings prim copies the shot's instead keeps every setting the
copy does, the lens included; [`crust check --all-passes`](#checking-every-pass) shows
which passes still render depth of field.

## matte: holdouts

The geometry in the pass's `collection:matte` is **held out**. A camera ray whose first
hit is a holdout ends there: that sample adds zero colour and zero alpha to the image, as
a ray that left the scene does, and every data AOV (depth, normal, position…) keeps its
clear value. Every other ray sees the holdout as the geometry it is: it casts its
shadows, and it shows in reflections and refractions.

The collection holds nothing by default (`includeRoot` is false), so a pass that authors
nothing on `matte` holds out nothing.

Holdouts resolve per sample, not per pixel: where a held-out crate and the character
interpenetrate, each sample shows whichever surface its camera ray reaches first, and
partly covered pixels get the matching fraction of alpha. A holdout with a cutout opacity
holds out with that opacity's probability. Its material's transmission does not count: a
glass matte is a hole, not a window onto what is behind it. A volume between the camera
and a holdout renders as it does without the pass, its glow included.

## cameraVisibility

What the pass's `collection:cameraVisibility` leaves out is **hidden from the camera**:
camera rays go straight past it to what lies behind, while shadow, indirect and
light-sampling rays see it as without the pass. The collection holds everything by
default.

A pass only takes camera visibility away. A light whose source the camera does not see
(the default for an area light), or geometry `crust:rayMask` hides from the camera, stays
hidden whatever the collection says. An area light the pass hides keeps occluding shadow
rays toward other lights, as it did.

A `DomeLight` or `DistantLight` outside `cameraVisibility`, or in `matte`, is invisible
to escaping camera rays, as `domeLightCameraVisibility = false` makes every one; how it
lights the scene does not change.

## renderVisibility

What the pass's `collection:renderVisibility` leaves out is **hidden from every ray**:
camera, shadow, indirect and light-sampling rays all pass it, as
[`crust:rayMask = 0`](@/docs/usd/geometry.md#crust-raymask) makes it. It casts no shadow and shows in
no reflection. The collection holds everything by default (`includeRoot` is true), so
a background plate alone is:

```usda
def RenderPass "plate"
{
    uniform bool collection:renderVisibility:includeRoot = 0
    prepend rel collection:renderVisibility:includes = </World/Set>
}
```

A **light** outside `renderVisibility` is off: it lights nothing, an escaping camera ray
sees no dome, its area light's surface stops no ray, and an AOV that selects only that
light (a light group, a light path expression naming it) is zero. Leave `/World/Lights`
out and the plate renders unlit. Light linking is decided after the pass: a light
linked only to geometry the pass hides lights nothing, as a light linked to nothing does.

`renderVisibility` comes first. A prim it leaves out is not a holdout and not counted as
one, whatever `matte` and `cameraVisibility` say: it is absent, so camera rays go through
where it stands. As with `cameraVisibility`, a pass only takes visibility away: geometry
`crust:rayMask` hides stays hidden.

Hidden geometry stays in the acceleration structure the passes of a shot share; the rays
skip it, so a pass that hides most of the set still pays to traverse it.

## Collections

The three collections are read with full `UsdCollectionAPI` semantics: the nearest
`includes` / `excludes` opinion wins, `expansionRule` (`explicitOnly` names only the
listed prims), included collections, and `membershipExpression`, predicates included
(`/World//{isa:Mesh}`).

A prim is judged by the prim that brings its geometry in: a native instance by its
instance prim, a `PointInstancer`'s instances by the instancer. A target inside an
instance's prototype cannot tell the instances apart, so it logs
`render_pass.target_in_instance` and is judged on the instance.

## Example: a character pass and a background pass

`samples/render_passes.usda` holds a hero, a crate in front of it, a mirror ball, a
floor and a sky, seen through a lens focused on the hero, and four passes. The first two
split the shot:

```usda
def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/Cam>
        rel products = </Render/Products/props>
    }
    def RenderSettings "charSettings"
    {
        rel camera = </World/Cam>
        rel products = </Render/Products/char>
    }

    def Scope "Passes"
    {
        # The character: everything else held out.
        def RenderPass "char"
        {
            rel renderSource = </Render/charSettings>
            prepend rel collection:matte:includes = [</World/Props>, </World/Set>, </World/Sky>]
        }

        # Everything else: the character held out.
        def RenderPass "props"
        {
            prepend rel collection:matte:includes = </World/Chars>
        }
    }
}
```

```bash
crust render -i samples/render_passes.usda --pass /Render/Passes/char    # writes char.exr
crust render -i samples/render_passes.usda --pass /Render/Passes/props   # writes props.exr
```

In `char.exr` the hero has a hole where the crate stands in front of it, and the crate's
shadow still falls on it. In `props.exr` the hero is a hole, its shadow still on the
floor, and its reflection still in the mirror ball. Both products write `color4f`
(RGB with alpha), so each is premultiplied and ready to composite.

The other two use what a pass can change without restating the shot: `plate` keeps only
the floor in `renderVisibility` (the lights are left out, so it renders unlit, with the
floor's alpha), and `util` renders through `/Render/sharp`, the
[override recipe](#overriding-one-setting-for-a-pass) above, writing `util.exr`
without the lens blur.

## Every pass at once

[`crust render --all-passes`](@/docs/reference/command-line.md#all-passes) renders every
pass the stage defines, each to its own products, exactly as `--pass` renders it alone:

```bash
crust render -i samples/render_passes.usda --all-passes -s 16   # char, props, plate, util
```

The passes share one import whenever their settings agree on everything the import builds
into the scene: the geometry, its subdivision and displacement, the materials, the
textures and the acceleration structure are built once, and each pass changes only what
each ray sees, its light list and its settings. All four passes of the sample share one.
A pass whose settings dice differently (another `crust:subdivisionLevel`, say), measure
motion over another shutter, or render in another colour space gets an import of its own;
the log says how many passes each import served.

Before importing anything, `--all-passes` works out every file each pass would write, and
refuses a stage where two passes would write the same one. That is what happens to two
passes whose settings have no `RenderProduct`: both would write `output.exr`.

## Checking every pass

[`crust check --all-passes`](@/docs/reference/command-line.md#check) describes the render
with no pass, then each pass: its settings prim, what it holds out and hides, whether it
renders depth of field and motion blur, its products, and every setting it changes from
the render with no pass. A pass whose settings copy the shot's says `depth of field on`
with nothing changed; one with the override above says `depth_of_field: on -> off`. Two
passes that would write one file are reported as the `passes_share_output` finding.

```bash
crust check -i samples/render_passes.usda --all-passes
```

## Compositing

Each sample of the frame lands in exactly one of two passes that hold each other out:
the char pass keeps the samples whose first hit is the hero, the props pass every other
sample. Their beauties therefore **add** up to the render with no pass active, pixel for
pixel, edges included — composite them with `plus` (additive merge):

```text
whole = char + props
```

This holds only when every sample lands in exactly one pass. Every object the camera
sees, and the dome, must be visible in exactly one pass and held out of all the others.
An object or a dome visible in two passes is counted twice: if the char pass above left
`/World/Sky` out of its `matte`, `char + props` would be the whole render plus the sky
once more wherever the camera sees it. An object held out of every pass leaves a hole.
Light paths don't need this split: shadows, reflections and bounce light go through
held-out objects in every pass, so each pass already carries its own share.

`char over props` (`char + (1 − α_char) · props`) is right only where a pixel is wholly
one pass or the other. At an edge pixel the props pass's colour already leaves out the
hero's share of the samples, and `over` takes that share away a second time: the edge
darkens by `α_char · (1 − α_char)` of the background around it.

Use `over` for a back pass that does *not* hold the front object out, but hides it from
the camera with `cameraVisibility`:

```usda
def RenderPass "propsBack"
{
    prepend rel collection:cameraVisibility:excludes = </World/Chars>
}
```

`char over propsBack` matches the whole render wherever a pixel is wholly one pass or
the other. At an edge it is exact against an even background such as the sky. Where the
background changes under the silhouette, such as the hero's contact shadow on the floor,
`over` adds a small error at edge pixels that more samples don't remove. The back pass
fills each edge pixel with the whole background behind the hero, but the camera saw only
part of it.

## Collections not read

`prune` is not read. When the active pass's `prune` restricts anything, the render logs
[`render_pass.collection_not_read`](@/docs/reference/warnings.md#render-passes) and
renders as if it were unauthored; leave the prims out of `renderVisibility` instead.

## Known gaps

- `prune` is not read, and a volume region in `matte` or left out of `cameraVisibility`
  or `renderVisibility` is neither held out nor hidden: it logs
  `render_pass.volume_ignored` and renders as without the pass.
- A holdout behind a thin wall that the camera ray passes straight through (glass at
  IOR 1) renders black with the wall's alpha of 1, not as a hole.
- There is no shadow catcher: a holdout records no shadow or reflection of the other
  objects into its alpha.
- Geometry a pass hides from every ray still costs that pass traversal time: it stays in
  the acceleration structure the passes share. Rendering that pass alone with `--pass`
  imports no less.
- In a streamed import, a pass collection that includes another collection authored in
  a different top-level subtree cannot read it while that subtree's chunk is imported
  (`render_pass.nested_not_composed`); author the collection on the pass, or set
  `CRUST_STREAM_IMPORT=0`.
