# Proposal

## Why

A firefly is one sample. Crust can say how noisy a render is (`crust diagnostic`,
its `firefly_energy` finding) and can split the image by light path expressions an
artist already thought to ask for, but it cannot answer the question an artist asks
first: *which path, through which light, made this bright pixel?* The diagnostic's
own Known gaps list "no per-object or per-light attribution of the missing energy".
A light path inspector answers it: select a few pixels, get the individual light
paths sorted by power, each with its expression, its light, its contribution and
the sub-pixel sample it came from. Crust already records every
path's events, per lobe, for the LPE AOVs; this change turns that record into an
inspector, shaped first for an agent to read.

## What Changes

- New subcommand `crust light-paths -i scene.usda --region X0,Y0,X1,Y1`: re-renders
  a small region (at most 100 pixels, e.g. 10×10) with the render's own settings and
  sample count, reproducing the samples of the full-frame render bit for bit, and
  records every **light path**: one contribution of one camera sample that ends on a
  light (an NEE connection, an emitter hit by a bounce, a light crossed or escaped
  to) or on non-light emission (`O`).
- Each record carries: its contribution (RGB, unclamped and after the indirect
  clamp), luminance, the expression it is an instance of (OSL LPE syntax, labelled
  by the dominant lobe at each vertex, with the exact per-lobe split beside it and a
  flag when the label is ambiguous), the light's prim path, the pixel, the sub-pixel
  film coordinates, the sample index and the camera.
- Records are reported as the top N by luminance (default 32), and aggregated by a
  chosen key: `none`, `light`, `expr` or `expr+light`, with each group's share of the
  region's luminance, of its second moment, its hit rate and its brightest sample.
- One report value, written as a versioned JSON report `crust-light-paths/1` (agent
  and CI facing: conclusion first, every truncation stated, every field usable as is)
  and a short Markdown summary on stdout. No geometry is written.
- New MCP tool `light_paths(render_id, region, group_by, top)` returning the same
  JSON for a session render, so an agent can go probe → light paths → edit → render
  → light paths.
- Prim paths per vertex are part of the schema from the start (`prims`), `null` with
  a note until the hit → prim table of `add-identity-aovs-openexrid` exists; the
  `expr+prims+light` grouping is refused until then. Step 2 fills them in without a
  format bump.
- Normal renders are unchanged: no instruction is added to a render that does not
  inspect, and the inspected region's beauty is the frame's, bit for bit.

## Capabilities

### New Capabilities

- `light-path-inspection`: recording, ranking and aggregating the individual light
  paths of a small pixel region, and the `crust-light-paths/1` report.

### Modified Capabilities

- `cli`: the `light-paths` subcommand joins the subcommand list and shares the scene
  and settings flags.
- `mcp-session`: the server lists a `light_paths` tool.

## Impact

- `crates/crust-core/src/tracer/route.rs`, `tracer/mod.rs`, `tracer/path.rs`: an
  inspection fold over the existing per-path `Route`, an event alphabet that names
  every lobe label, lights recorded by index; the per-sample film position and index
  passed to the fold.
- `crates/crust-core/src/inspect/` (new): the report value, grouping, top-N, JSON
  and Markdown.
- `crates/crust-render/src/main.rs` (subcommand), `crates/crust-render/src/mcp/`
  (tool).
- Docs: `site/content/docs/reference/command-line.md`, a help page on finding the
  path behind a firefly, `site/content/docs/help/claude-desktop.md`,
  `docs/architecture.md` (invariants), `CLAUDE.md` (documentation table), the
  `light-path-inspection` design record.
- No new dependency, no `unsafe`, no environment switch.
- Depends on `add-identity-aovs-openexrid` for step 2 (prims) only.
