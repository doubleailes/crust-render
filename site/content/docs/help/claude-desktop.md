+++
title = "Look-dev with Claude Desktop"
description = "Keep a USD stage open in a crust session, edit it through composition and render it, from Claude Desktop."
date = 2026-10-09T08:00:00+00:00
updated = 2026-10-09T08:00:00+00:00
draft = false
weight = 30
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = '<code>crust mcp</code> serves a live session to Claude Desktop: the agent opens a stage once, then queries it, edits it and renders it by conversation. The session is an override layer on disk, which any USD tool opens and <code>crust render</code> renders as the session did.'
toc = true
top = false
+++

## The session is a file

`open_session(input, output)` writes a new USD layer at `output` whose only
composition arc is a sublayer on `input`:

```usda
#usda 1.0
(
    subLayers = [
        @./shot.usda@
    ]
)
```

When the input authors a `metersPerUnit` or an `upAxis`, the layer copies them, since
a stage reads its units and up axis from its root layer: without the copy the session
would render a camera's [depth of field](@/docs/usd/render-settings.md#camera-depth-of-field),
or a dome's `poleAxis = "scene"`, unlike `crust render` of the input. Resuming a layer that
lacks them adds them. A layer keeps the value it copied even if the input's changes later:
edit it in the layer to follow.

Every edit the agent makes is an opinion in that layer, as an `over` where the prim
is defined elsewhere, and the layer is saved after every edit. What is rendered is
always the saved file, imported exactly as `crust render <output>` imports it. So:

- the file is the session's result: it opens in any USD tool, and renders in crust
  as it did in the session;
- nothing is lost if the server stops: at most the edit in flight;
- calling `open_session` again with the same `input` and `output` resumes it, with
  every opinion it holds. Undo history does not survive the process.

The output must be a `.usda`: a session is a small text file of opinions, readable
and diffable. The sublayer path is written relative to the output's directory
(absolute when the two are on different drives), so the layer can move with its
input.

## What a session writes

A session writes only:

- its override layer;
- the final render's files (`render_final`): the EXR and PNG preview, or every
  RenderProduct the stage authors;
- the `.tx` files [`--auto-tx`](@/docs/reference/command-line.md#auto-tx) creates,
  as a render would.

No tool modifies, saves or creates any other layer. An attribute whose current value
lives in a referenced asset is overridden in the session's layer; the asset file is
not touched. `open_session` refuses an `output` that is the input or one of the
layers it composes, and an existing layer that does not sublayer the input, before
writing anything.

## Install

`crust mcp` is the server: Claude Desktop launches it and talks to it over stdin and
stdout.

The simplest install is the extension bundle the
[nightly release](https://github.com/doubleailes/crust-render/releases/tag/nightly)
ships for each platform Desktop runs on: `crust-nightly-x86_64-pc-windows-msvc.mcpb`,
`crust-nightly-aarch64-apple-darwin.mcpb` (Apple silicon) or
`crust-nightly-x86_64-apple-darwin.mcpb` (Intel). Open it with Claude Desktop, or drag it
onto **Settings → Extensions**, and confirm: the bundle holds the `crust` binary and a
manifest that launches it as `crust mcp`.

To use a `crust` binary of your own instead, point Desktop at it in its
`claude_desktop_config.json`, which Desktop opens from **Settings → Developer → Edit
Config**:

- Windows: `%APPDATA%\Claude\claude_desktop_config.json`
- macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`

```json
{
  "mcpServers": {
    "crust": {
      "command": "C:\\tools\\crust.exe",
      "args": ["mcp"]
    }
  }
}
```

On macOS the command is the binary's path, such as `/usr/local/bin/crust`. Restart
Desktop, and the crust tools appear in the chat's tool menu. Pass absolute paths to
`open_session`: Desktop starts the server in a working directory of its own choosing.

Add `"-l", "debug"` to `args` to record every import and render in Desktop's MCP log.

## The tools

| tool | what it does |
|------|--------------|
| `open_session(input, output)` | creates the override layer `output` on `input` (or resumes it) and imports it; reports the camera, the resolution and the warnings |
| `query(path)` | a prim (type, active, children, variant sets, authored attributes and relationships, the layers that author it) or an attribute (type, value, and `source`, the layer its value comes from, `fallback` for a schema's fallback nothing authors, or null when nothing gives it a value) |
| `check()` | the [`crust check`](@/docs/reference/command-line.md#check) report of the current layer, as `crust-check/2` JSON |
| `set_attribute(path, value, type?)` | authors an attribute value in the layer, typed as the attribute is declared |
| `set_variant(prim, variant_set, variant)` | authors a variant selection |
| `set_active(prim, active)` | activates or deactivates a prim |
| `bind_material(prim, material)` | binds a Material to a prim |
| `author_usda(text)` | merges a USDA snippet of `over`, `def` and `class` specs into the layer, as one edit |
| `drop_to_surface(prims, target?, align_to_normal?)` | moves each prim down until it rests on the surface beneath it, optionally turned to that surface's normal; one edit |
| `scatter_on_surface(target, prototypes, path, seed, count \| density, max_slope?, random_rotation?, scale?)` | defines a `PointInstancer` whose instances lie on the target's surface, by area, from the seed; one edit |
| `undo()` | the layer as it was before the last edit |
| `reload()` | composes the stage again and imports the layer whole, whether or not a source changed; lists the source layers that did |
| `render(region?, spp?, budget_s?, wait?)` | starts a progressive render and answers with the image so far: at 4 samples per pixel by default, at completion with `wait="done"`, within `budget_s` (default 10) either way |
| `snapshot(render_id, budget_s?)` | the render's latest image and progress, at once, or after waiting at most `budget_s` for the render to finish |
| `cancel(render_id)` | stops a render, keeping what it traced |
| `probe(render_id, x, y)` | the beauty and every AOV at a pixel, in the working space, and the prim and material the camera ray hits there |
| `light_paths(render_id, region, group_by?, top?)` | the light paths behind up to 100 pixels of a completed render (`top`: 32 by default, at most 1000), as [`crust light-paths`](@/docs/reference/command-line.md#light-paths) reports them: each path's expression, light, contribution and sample, the brightest first, and their groups (see [Finding the path behind a firefly](@/docs/help/light-paths.md)) |
| `diff(render_a, render_b)` | compares two renders as [`crust diff`](@/docs/reference/command-line.md#diff) compares two files |
| `render_final()` | renders the layer as `crust render <output>` would, and lists the files written |

### Editing

Every edit tool authors opinions in the layer, saves it, and brings the scene up to date
with the saved file, so that it renders exactly as `crust render <output>` would. Its
result says which [warning codes](@/docs/reference/warnings.md) the scene gained or lost,
how long the update took (`import_s`), and how it was applied (`applied`): an edit crust
does not read is not silent. An edit the import refuses is undone at once, and the layer is
left as it was.

Most look-dev edits do not need the whole stage read again. crust compares the layer
before and after the edit, and applies it in place when every value it changed is one of
these:

| `applied` | what changed | what crust does |
|-----------|--------------|-----------------|
| `settings` | an attribute of the `RenderSettings` prim the render uses, or of the render camera (not its transform) | reads the settings and the camera again |
| `materials` | an `inputs:*` value on a shader of a material the scene uses, or one of the material's own inputs | rebuilds those materials |
| `lights` | an `inputs:*` value on a light (not its `width`, `height`, `radius` or `length`) | rebuilds those lights and the light selection |
| `full` | anything else: a prim added or removed, a binding, a connection, a transform, visibility, a variant, a light's size | imports the saved file again |

An edit with changes of several kinds is applied as the most expensive of them, in the
order of the table. A change that turns out to need more is applied as `full`: a material
that becomes a cutout, emissive or displaced, a light that stops emitting, a setting that
changes the working colour space or the subdivision level, a camera or resolution change
on a scene whose tessellation is sized on screen (below), one whose rebuild would report a
different warning, a shader another material also uses (through `inherits` or a
connection), or any edit made after a texture file changed on disk. Either way the render is the same, bit for bit, as after a full import
of the same layer. `undo` is applied the same way.

Production shots usually have three things that look-dev stages lack. None of them, on
its own, turns an edit into `full`:

- **Tessellation sized on screen.** With adaptive subdivision
  (`crust:subdivisionEdgeLength`), or cylinders, cones and capsules sized for the render
  camera, a camera or settings edit stays in place when the view those shapes were
  tessellated for is unchanged: exposure, f-stop, focus distance, samples, AOVs. Moving
  the camera, changing its focal length or aperture, or changing the resolution
  re-tessellates, so it is `full`.
- **Light linking.** On a scene whose lights author `collection:lightLink` or
  `collection:shadowLink`, a light's `inputs:*` edit is still `lights`, and the links stay
  as they were. Editing a link itself is `full`, and so is editing a light whose
  `lightLink` includes nothing (the render no longer counts it as a light).
- **Looks brought in by payloads.** A shader input is `materials` whether the material is
  defined in the stage or in a layer a payload loads.

The decoded textures, Ptex files, light textures and IES profiles stay with the session,
so even a `full` edit does not decode them again. A file that changes on disk (a texture
re-exported mid-session, or a tile added to a UDIM set) is read again by the next
edit, which is then applied as `full`. Set
[`CRUST_SCENE_DELTAS=0`](@/docs/reference/environment-variables.md#crust-scene-deltas)
to import every edit whole with nothing kept, as earlier versions did.

- **Values take the attribute's type.** `set_attribute` reads the type the stage declares
  and converts the JSON value to it: a number for `float`, `[r, g, b]` for `color3f`, a
  string for `token`, `string` or `asset`, an array for an array type. A value that does
  not convert is refused, naming the type, and nothing is authored.
- **Schema attributes are typed before anything authors them.** The session's stage
  carries the same USD schemas as the render's import, so an attribute a prim's schema
  declares, such as a light's `inputs:exposure`, has its type and fallback value even
  when no layer authors it: `query` answers them with `source` `fallback`, and
  `set_attribute` needs no `type`. A `type` that contradicts the schema is refused. Only
  a custom attribute no schema declares needs `type` the first time.
- **Asset paths follow the layer.** A relative asset path is read from the *input's*
  directory, as `query` shows the input's own paths, and written relative to the layer's,
  so it names the same file wherever the layer is opened from.
- **`author_usda` is one edit.** Several opinions, or a new prim (a light, a material),
  go in one snippet: one save, one import, one `undo` step. Each field it authors replaces
  the layer's; sibling prims and properties stay. Text that does not parse, or that
  authors `subLayers`, is refused with the layer byte for byte unchanged.
- **`undo` lasts for the server process.** It restores the layer's exact bytes before the
  last edit. A resumed session starts with no history.

### When another program changes the stage

The session watches every USD layer its import composed: the input, its sublayers, and the
layers behind references and payloads. Another program can rewrite them while the session
is open, such as a scene-assembly agent running as a second MCP server in the same client.
Before each `query`, `check`, edit, `undo`, `render` and `light_paths`, the session checks
the length and modification time of each of those files:

- When one changed, `query` and the next edit read the new composition.
- The next edit, `undo`, `render` or `check` imports the stage whole (`applied` `full`).
  An edit's, `undo`'s and `render`'s result lists the layers in `sources_changed`;
  `check` answers its usual `crust-check/2` report of the new import. An edit is never applied in place
  over a scene imported from a layer that has since changed.
- A source that no longer imports (a half-written file, a syntax error) fails the call,
  naming the layers that changed. The scene is not rendered stale: the next call tries the
  import again.
- `light_paths` refuses a render made before a source changed, as it refuses one made
  before an edit.

`reload()` does the same on demand, and imports even when no USD layer changed. Use it
after changing a file the session does not watch: a MaterialX document a material
reads, or a USD layer rewritten with exactly the same length and modification time. The
undo history stays: `undo` puts back the layer's bytes, composed over the sources as they
are now.

The session never writes those layers. Write them by replacing the file (save to a
temporary file, then rename over the original), as USD's own `Save` does. A file rewritten
in place *while* the session imports it can crash the server when the build reads USD
through memory mappings
([`CRUST_USD_MMAP`](@/docs/reference/environment-variables.md#crust-usd-mmap)).

### Placing things on surfaces

`drop_to_surface` and `scatter_on_surface` compute placement against the geometry the
session renders: subdivided, displaced and instanced as the render sees it, at the
session's frame. Only geometry the camera sees counts. "Up" is the stage's `upAxis` (Y
when the stage authors none). When a source layer changed on disk, the session imports
it before placing anything, so nothing lands on a surface that has since moved. Each call
is one edit: saved, imported, one `undo` step, with the usual `applied`, `import_s` and
warnings, plus `place_s`, the time the placement itself took.

- **`drop_to_surface(prims, target?, align_to_normal?)`** moves each prim along the down
  axis until no point of its geometry lies below the highest surface beneath it, and one
  rests on it. A prim already sunk into the surface is raised. With `target`, only those
  prims' geometry counts, so a cup can drop through a shelf onto the table below. A prim's
  own geometry never counts.
  - With `align_to_normal`, the prim is first turned so its up axis follows the surface
    normal under the centre of its footprint: the face's own normal, whatever vertex
    normals the mesh authors.
  - The move is one op, `xformOp:transform:crustPlace`, first in the prim's
    `xformOpOrder`, after a `!resetXformStack!` if there is one. The prim's own ops,
    including ones a payload authors, follow it unchanged. Dropping again folds into the
    same op.
  - Each prim is reported `placed` (with its contact point and how far its origin
    moved), `no geometry`, or `nothing below`. A prim that is not placed gets no opinion.
  - A path inside an instance, one that cannot hold a transform (a `Scope`, an untyped
    prim), or a prim given together with one of its ancestors, refuses the whole call.
- **`scatter_on_surface(...)`** defines a new `PointInstancer` at `path`. Its instances lie
  on `target`'s triangles in proportion to their area, each prototype's origin on the
  surface, standing upright about the up axis.
  - **Prototypes** are prim paths of the stage, or USD files (`.usd`, `.usda`, `.usdc`,
    `.usdz`), absolute or relative to the input stage's directory. Each becomes a child of the instancer's `Prototypes` scope that
    references it and drops its own placement, so it is instanced at its own origin. The
    prim it references still renders where it is: deactivate it with `set_active` if it
    should not.
  - **How many:** `count` instances, or `density` instances per square stage unit of
    eligible surface, at most 100 000.
  - **Options:**
    - `max_slope` leaves faces steeper than that many degrees bare;
    - `random_rotation` turns each instance about the up axis;
    - `scale: [min, max]` draws each instance's uniform scale.
  - **The same seed over the same scene authors the same bytes**, whatever the machine's
    thread count and the stage's sampler setting. To try other settings, `undo` and
    scatter again: a path that already names a prim is refused, and so is one under an
    instance, where USD would ignore it.
  - Curves and implicit shapes in the target are skipped and listed in `skipped`.

### Rendering

- **`render` answers at the first image.** As soon as every pixel has 4 samples, it
  answers with a PNG of the image so far (at most 1024 pixels on its long side,
  tone-mapped as `crust render`'s preview), the samples every pixel has reached, `done`,
  and a `render_id`. On the Cornell box that is about half a second, where a whole
  render takes ten. A render of 4 samples per pixel or fewer answers when it completes.
  The render goes on refining after the answer.
- **Waiting for the finished image.** `snapshot(render_id, budget_s)` waits for the render
  to finish, for at most `budget_s`, without restarting it. `render(wait="done")` waits
  from the start. Either way, nothing waits longer than its budget: `render`'s defaults
  to 10 s, and a render that needs longer answers with `done = false`.
- **`progress` counts what the progress bar counts.** On a guided render
  (`crust:pathGuiding`) it counts the final pass only and stays at 0 while guiding
  trains, as `crust render`'s bar does. `spp_reached` (each pass's completed stage) and
  `generation` (bumped with every image update) move through training too.
- **One render at a time.** A new render, an edit, `undo`, `reload` or `open_session`
  cancels the one running, and the cancelled render's id is in the result.
- **The render settings are opinions.** `render` takes only a region and a sample count;
  everything else that shapes the render is the stage's `RenderSettings`, so `render_final`
  needs no flag to reproduce it. Change them with `set_attribute` or `author_usda`.
- **Renders are reproducible.** A session render at 16 spp to completion is bit for bit
  the image `crust render -i <output> -s 16` writes, with the same region.
- **The session keeps its 8 latest renders** for `snapshot`, `probe`, `light_paths` and
  `diff`; a render result lists the ids it evicted.
- **`light_paths` inspects the render as it was.** It renders the region again with the
  render's settings and samples, so its pixels are the render's — on the layer that
  render used. It is refused once an edit changed the layer (an `undo` back to it is
  fine) or a source layer changed on disk, for a render that did not complete, while another render runs, and for a region
  outside the render's or over 100 pixels.
- **`diff` compares like with like.** It refuses two renders with different sample counts
  or regions, and a render still running.

## A look-dev session

Here is a session on the Cornell box, as the tool calls Claude makes. (Each line is one
call and the part of its result that matters.)

```text
open_session(input="C:/shots/cornellbox.usda", output="C:/shots/lookdev/cornellbox_lookdev.usda")
  → camera /scene/camera1, 640 × 360, 1 warning (material.fallback_default)
query("/scene/Sky")
  → DomeLight, inputs:texture:file = sky_gradient.exr (authored in cornellbox.usda)
render(region=[160, 90, 480, 270], spp=16, budget_s=10, wait="done")
  → render 1: the crop around the spheres, done
set_attribute("/scene/Sky.inputs:exposure", -1)
  → saved, no warning gained
author_usda("""
  over "scene" {
      def RectLight "key" {
          float inputs:intensity = 30
          float inputs:width = 1
          float inputs:height = 1
          double3 xformOp:translate = (0, 3.9, 0)
          float3 xformOp:rotateXYZ = (-90, 0, 0)
          uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateXYZ"]
      }
  }""")
  → saved, no warning gained
render(region=[160, 90, 480, 270], spp=16, budget_s=10, wait="done")
  → render 2: the same crop
diff(render_a=1, render_b=2)
  → not identical, relative MSE …
probe(render_id=2, x=320, y=200)
  → beauty [r, g, b], prim /scene/pCube1, material /scene/Looks/initialShadingGroup
light_paths(render_id=2, region=[315, 195, 325, 205])
  → summary, groups and the brightest paths: which expression, through which light
undo()
  → the key light is gone; the layer is as it was after set_attribute
render_final()
  → C:/shots/lookdev/cornellbox_lookdev.exr, …/cornellbox_lookdev.png
```

The session's result is `cornellbox_lookdev.usda`, a few lines of opinions over the
Cornell box, and its render. `crust render -i cornellbox_lookdev.usda` renders it again,
and any USD tool opens it.

## Limits

- **One session, one client.** The server holds one session at a time, over stdio only.
- **Most structural edits import the whole stage again.** A `full` edit takes seconds on
  a medium stage; each result says how long. Only the edits in the table above are
  applied in place. Decoded textures the scene no longer uses are let go at the next
  `full` edit. A session holds the stage it authors and the scene it renders at once,
  about twice the memory of a render's import, which puts Moana-scale stages out of reach.
- **`probe` names the prim the import recorded, not the one you may expect.** It names
  the first surface the camera ray through the pixel's centre meets, before any opacity,
  so a cutout's hole names the surface it cuts. A hit inside a native instance or a
  `PointInstancer` names the instance or the instancer prim, not the prototype's mesh,
  and the material is null for geometry no Material prim fills (an area light's
  emitter). Once an edit has imported the stage again, a render from before it names
  nothing: render again.
- **A schema fallback is not always what renders.** Where crust has its own default for
  an unauthored attribute, the render uses it, not the fallback `query` shows: a
  DistantLight with no `inputs:intensity` queries as `50000` and renders as `1`, and a
  camera with no `verticalAperture` queries as `15.2908` but follows the image's aspect.
- **Placement is geometric, not physical.** A drop rests a prim on the surface where its
  vertices meet it: a sharp bump between two vertices can poke through a face, prims
  dropped in one call do not land on each other, and opacity cutouts count as solid. The
  placement is right at the session's frame and subdivision level; an animated parent,
  or a later change to the subdivision, can move the surface away from it. Scattered
  instances stand upright; they are not turned to the surface normal.
- **A placed prim's `xformOpOrder` is the layer's.** The override layer authors the whole
  list, so a later change to the list in a source layer is hidden behind it. `undo` the
  drop and drop again to pick it up.
- **`render_final` blocks the session** until it has written its files.
