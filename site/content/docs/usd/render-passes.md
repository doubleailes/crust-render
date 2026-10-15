+++
title = "Render passes and layers"
description = "RenderPass: one render invocation, its image split into render layers, its files named by a pattern."
date = 2026-10-15T08:00:00+00:00
updated = 2026-10-15T08:00:00+00:00
draft = false
weight = 27
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'A <code>RenderPass</code> is one render: <code>crust render --pass</code> renders it with the settings it names, and splits its image into render layers that each hold out the others.'
toc = true
top = false
+++

## Selecting a render pass

A render pass is a USD `RenderPass` prim. `crust render --pass <PRIM_PATH>` renders it, and
[`crust ls pass`](@/docs/reference/command-line.md#ls) lists the passes a stage holds.

The pass's `renderSource` relationship names the `RenderSettings` prim the render uses:
its camera, resolution, samples and [render products](@/docs/usd/aovs.md). A pass that
authors no `renderSource` uses the stage's own settings, as a render without `--pass`
does.

```usda
def Scope "Render"
{
    def RenderSettings "settings" { ... }
    def RenderSettings "fx_settings"
    {
        int2 resolution = (640, 360)
        ...
    }

    def Scope "Passes"
    {
        def RenderPass "render"
        {
            rel renderSource = </Render/settings>
        }

        def RenderPass "fx"
        {
            rel renderSource = </Render/fx_settings>
        }
    }
}
```

```bash
crust ls pass -i shot.usda                          # /Render/Passes/render, /Render/Passes/fx
crust render -i shot.usda --pass /Render/Passes/fx  # renders at 640×360
```

The command stops with an error, before anything is rendered, when the `--pass` path is
not a `RenderPass` on the stage, or when its `renderSource` targets a prim that is not a
`RenderSettings`. Without `--pass`, a stage renders exactly as before, whatever passes it
holds.

## Render layers

A pass splits its objects into **render layers**. One traversal renders every layer, and
each layer holds out the others: it shows only the objects it owns, and is empty (zero
colour, zero alpha) where another layer's objects are in front. Lighting is untouched:
every object still shadows, reflects and bounces light in every layer. So the layers'
colours add up to the beauty, and compositing them back over each other gives the image a
render without layers gives.

### Declaring layers

A layer is a multiple-apply collection on the `RenderPass` prim, named
`collection:layer:<name>`. Its membership follows `UsdCollectionAPI`: `includes`,
`excludes`, `includeRoot` and `expansionRule`, nested collections included, as light-link
collections are read. A prim inherits its ancestors' membership, so including `/World/Hero`
puts every mesh below it in the layer.

```usda
def RenderPass "render" (
    prepend apiSchemas = ["CollectionAPI:layer:set", "CollectionAPI:layer:hero"]
)
{
    rel renderSource = </Render/settings>
    rel collection:layer:set:includes = </World/Set>
    rel collection:layer:hero:includes = </World/Hero>
}
```

- **Order.** Layers are ordered as the pass's `apiSchemas` list them. `crust ls pass`
  shows the order.
- **The `default` layer.** Every pass has one more layer, `default`, always last. It owns
  every prim no layer claims (a light's own geometry, say), and every camera ray that
  escapes the scene, the sky and a dome light included. A pass with no layer collection
  has only `default`, which is then the whole image.
- **Overlaps.** A prim two layers claim belongs to the earlier one, with a
  [`pass.layer_overlap`](@/docs/reference/warnings.md) warning naming the prim and both
  layers.
- **What decides a layer.** Membership is judged on the prim that brings the geometry
  into the render: the mesh, the native instance, or the point instancer. Two native
  instances of one prototype can be in two layers. The instances of one point instancer
  are all in its layer, and a collection that targets a prim inside an instance's
  prototype or a point instancer's prototypes cannot tell it apart from its siblings: the
  target is judged on the instance or the instancer, with a
  [`pass.layer_target_in_instance`](@/docs/reference/warnings.md) warning.
- **Expressions.** A layer collection's `membershipExpression` is not read: the layer
  claims nothing, with a warning.

### Which layer a pixel belongs to

Each camera sample belongs to the layer of the first thing its camera ray meets:

- a surface, after cutouts are passed through: a ray that crosses the transparent part
  of a cut-out leaf belongs to what it meets behind it;
- a thin-walled surface, such as a window, which counts as met: what is seen through a
  glass pane belongs to the pane's layer;
- a volume, where the ray scatters in it;
- nothing at all: a ray that escapes belongs to `default`.

That sample's colour, alpha and AOVs go to its layer, and nothing goes to the others. A
pixel whose samples are shared between layers is split between them in proportion, as its
alpha is.

## The files a pass writes

A pass writes its files where its **file pattern** says: the pass's `crust:filePattern`
string attribute, or [`-o`](@/docs/reference/command-line.md#output) given with `--pass`,
which replaces it. Without either, the pattern is `$l_$n_$o.$04f.$x`.

The pattern is expanded once for every layer and every RenderVar of the pass: each var of
the products its `RenderSettings` lists, each var once. A pass whose settings list no
product writes one var per layer, `Beauty` (colour and alpha). A relative path is resolved
against the working directory, and missing directories are created.

### Specifiers

A pattern is plain text with these specifiers:

| specifier | expands to |
|-----------|------------|
| `$l`, `$L` | the pass prim's name |
| `$n` | the layer's name |
| `$o` | the RenderVar's name (its `driver:parameters:aov:name`, else the prim's) |
| `$f` | the frame, an integer |
| `$01f`–`$09f` | the frame, zero-padded to that width (the sign counts: `-003`) |
| `$c`, `$C` | the render camera prim's name |
| `$e`, `$E` | the eye: `1` and `left`, crust renders one view |
| `$x` | `exr` |
| `$(NAME)` | the environment variable `NAME` |

- `$_` before a specifier lower-cases its value: `$_o` is `diffuse` for the var `Diffuse`.
- `\$` writes a literal `$`, and a lone `$` (one followed by no name) is literal too.
- Without [`-f`](@/docs/reference/command-line.md#frame), the frame is 1. A subframe
  rounds to the nearest frame, halves away from zero, with a
  [`product.subframe_frame`](@/docs/reference/warnings.md) warning, as a product path's
  `$F4` does.
- Any other `$NAME`, husk's `$F4` included, is left in the path as written, with a
  [`product.unknown_variable`](@/docs/reference/warnings.md) warning.

Two patterns stop the command before anything is rendered, since either would write
somewhere you did not mean: a `$(NAME)` whose variable is not set, and a path whose
extension is not `.exr` (in any case). Crust writes only OpenEXR.

`$l` is the pass prim's name, not its path: a path would add directories.

### Outputs that share a path share a file

Every (layer, RenderVar) output whose pattern expands to the same path is written to that
one multichannel EXR, outputs ordered by layer, then by RenderVar. Leaving `$o` out of the
pattern puts a layer's AOVs in one file; leaving `$n` out puts the layers in one file.

In a file holding one output, the channels are named as a
[render product](@/docs/usd/aovs.md)'s: the beauty `R`, `G`, `B`, `A`, any other var
`<var>.R` and so on. In a file holding several outputs, each output's channels are prefixed
with its **layer name**: the pass's `crust:layerPattern`, expanded with the same specifiers
for that layer and var, default `$n_$o`. The beauty of the layer
`hero` is then `hero_Beauty.R`, `hero_Beauty.A`, and its diffuse `hero_Diffuse.R`. Two
outputs of one file whose layer names are equal would overwrite each other's channels, so
the command stops before rendering.

A file that holds exactly one layer and that layer's beauty gets a tone-mapped PNG beside
it, of that layer. A file holding several layers gets none.

The file carries the header a product's file carries: crust's sampling stamp, its
software name and the working space. The products' authored `driver:parameters:*` values
are not copied into a pass's files.

### Example

The `render` pass of `samples/render_passes.usda` authors
`crust:filePattern = "renders/$l_$n_$o.$04f.$x"`, with the layers `set`, `hero` and
`default`, and the vars `Beauty`, `Diffuse` and `Depth`:

```bash
crust render -i samples/render_passes.usda --pass /Render/Passes/render -f 3
# renders/render_set_Beauty.0003.exr (+ .png), renders/render_set_Diffuse.0003.exr,
# renders/render_set_Depth.0003.exr, the same three for hero and for default

crust render -i samples/render_passes.usda --pass /Render/Passes/render -o 'renders/$l_$n.$x'
# renders/render_set.exr, renders/render_hero.exr, renders/render_default.exr (+ a .png each)

crust render -i samples/render_passes.usda --pass /Render/Passes/render -o 'renders/$l.$x'
# renders/render.exr, every layer's channels in one file, no PNG
```

Single-quote a pattern on the command line so the shell leaves its `$` alone.
[`crust check --pass`](@/docs/reference/command-line.md#check) lists the files and their
channels without rendering.

### Pass collections crust does not read

A `RenderPass` can also author `renderVisibility` and `cameraVisibility` collections, which
remove objects from a pass. Crust renders every object in every pass: a pass that authors
either collection so that it restricts what is rendered is rendered as if it did not, with
a [`pass.collection_not_read`](@/docs/reference/warnings.md) warning. Use layers to split
what a pass shows. Per-pass material or light overrides have no USD schema, and are not
supported either.

[`samples/render_passes.usda`](https://github.com/doubleailes/crust-render/blob/main/samples/render_passes.usda)
holds two passes: `render`, with the layers `set` and `hero`, and `fx`, with its own
settings and no layer.
