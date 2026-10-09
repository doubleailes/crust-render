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
| `query(path)` | a prim (type, active, children, variant sets, authored attributes and relationships, the layers that author it) or an attribute (type, value, and `source`, the layer its value comes from) |
| `check()` | the [`crust check`](@/docs/reference/command-line.md#check) report of the current layer, as `crust-check/1` JSON |
| `set_attribute(path, value, type?)` | authors an attribute value in the layer, typed as the attribute is declared |
| `set_variant(prim, variant_set, variant)` | authors a variant selection |
| `set_active(prim, active)` | activates or deactivates a prim |
| `bind_material(prim, material)` | binds a Material to a prim |
| `author_usda(text)` | merges a USDA snippet of `over`, `def` and `class` specs into the layer, as one edit |
| `undo()` | the layer as it was before the last edit |
| `render(region?, spp?, budget_s?)` | starts a progressive render and answers within `budget_s` (default 10) with the image so far |
| `snapshot(render_id)` | the render's latest image and progress, without waiting |
| `cancel(render_id)` | stops a render, keeping what it traced |
| `probe(render_id, x, y)` | the beauty and every AOV at a pixel, in the working space |
| `diff(render_a, render_b)` | compares two renders as [`crust diff`](@/docs/reference/command-line.md#diff) compares two files |
| `render_final()` | renders the layer as `crust render <output>` would, and lists the files written |

### Editing

Every edit tool authors opinions in the layer, saves it, and imports the saved file again,
exactly as `crust render <output>` would. Its result says which
[warning codes](@/docs/reference/warnings.md) the import gained or lost, and how long it
took: an edit crust does not read is not silent. An edit the import refuses is undone at
once, and the layer is left as it was.

- **Values take the attribute's type.** `set_attribute` reads the type the stage declares
  and converts the JSON value to it: a number for `float`, `[r, g, b]` for `color3f`, a
  string for `token`, `string` or `asset`, an array for an array type. A value that does
  not convert is refused, naming the type, and nothing is authored.
- **Undeclared attributes need a `type`.** A schema attribute nothing authors yet, such
  as a light's `inputs:exposure`, has no type on the session's stage: neither openusd 0.7
  nor openusd-schemas 0.7 ships schema data. Pass `type` (`"float"`) the first time.
- **Asset paths follow the layer.** A relative asset path is read from the *input's*
  directory, as `query` shows the input's own paths, and written relative to the layer's,
  so it names the same file wherever the layer is opened from.
- **`author_usda` is one edit.** Several opinions, or a new prim (a light, a material),
  go in one snippet: one save, one import, one `undo` step. Each field it authors replaces
  the layer's; sibling prims and properties stay. Text that does not parse, or that
  authors `subLayers`, is refused with the layer byte for byte unchanged.
- **`undo` lasts for the server process.** It restores the layer's exact bytes before the
  last edit. A resumed session starts with no history.

### Rendering

- **`render` never blocks for a whole render.** It answers within `budget_s` with a PNG
  of the image so far (at most 1024 pixels on its long side, tone-mapped as `crust
  render`'s preview), the samples every pixel has reached, `done`, and a `render_id`. The
  render goes on refining; `snapshot` shows it later.
- **`progress` counts what the progress bar counts.** On a guided render
  (`crust:pathGuiding`) it counts the final pass only and stays at 0 while guiding
  trains, as `crust render`'s bar does. `spp_reached` (each pass's completed stage) and
  `generation` (bumped with every image update) move through training too.
- **One render at a time.** A new render, an edit, `undo` or `open_session` cancels the
  one running, and the cancelled render's id is in the result.
- **The render settings are opinions.** `render` takes only a region and a sample count;
  everything else that shapes the render is the stage's `RenderSettings`, so `render_final`
  needs no flag to reproduce it. Change them with `set_attribute` or `author_usda`.
- **Renders are reproducible.** A session render at 16 spp to completion is bit for bit
  the image `crust render -i <output> -s 16` writes, with the same region.
- **The session keeps its 8 latest renders** for `snapshot`, `probe` and `diff`; a render
  result lists the ids it evicted.
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
render(region=[160, 90, 480, 270], spp=16, budget_s=10)
  → render 1: the crop around the spheres, done
set_attribute("/scene/Sky.inputs:exposure", -1, type="float")
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
render(region=[160, 90, 480, 270], spp=16, budget_s=10)
  → render 2: the same crop
diff(render_a=1, render_b=2)
  → not identical, relative MSE …
probe(render_id=2, x=320, y=200)
  → beauty [r, g, b]
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
- **Every edit imports the whole stage again.** Seconds on a medium stage; each result
  says how long. A session holds the stage it authors and the scene it renders at once,
  about twice the memory of a render's import, which puts Moana-scale stages out of reach.
- **`probe` does not name the prim.** crust keeps no table from a hit to its prim yet, so
  `prim` and `material` are null, and the result says why.
- **`render_final` blocks the session** until it has written its files.
