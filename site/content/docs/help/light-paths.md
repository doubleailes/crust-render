+++
title = "Finding the path behind a firefly"
description = "Name the light path that makes a pixel bright with crust light-paths, then see it over the whole frame."
date = 2026-10-13T08:00:00+00:00
updated = 2026-10-13T08:00:00+00:00
draft = false
weight = 25
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'A firefly is one sample. <code>crust light-paths</code> re-renders a few pixels exactly as the render did and lists the light paths their samples took — which expression, through which light, from which sample — so the path behind a bright pixel has a name you can act on.'
toc = true
top = false
+++

## From a bright pixel to its path

The loop has three steps: find the pixel, inspect the region around it, and author the
path it names as an AOV to see it over the whole frame.

### 1. Find the pixel

Open the render in an image viewer and read the coordinates of the bright pixel, from the
image's top-left corner (as `--region` and an EXR data window count them). In a
[Claude Desktop session](@/docs/help/claude-desktop.md), `probe` gives the same: the
pixel's value, its prim and its material. [`crust diagnostic`](@/docs/help/diagnosing-a-render.md)
says *whether* fireflies matter (its `firefly_energy` finding); this page is about *which
path* they are.

### 2. Inspect the region

Ask for a region of at most 100 pixels around it, at the sample count the render used:

```bash
crust light-paths -i shot.usda -s 256 --region 618,331,628,341
```

The region is rendered with the render's own settings, so its samples are the frame's:
every pixel in the report's `pixels` is the pixel the render wrote, bit for bit, and the
records are the samples of the firefly you saw, not a lookalike. Two cases change that,
and the report says so in `reproduces_frame_reason`: path guiding (it trains on the region
alone) and adaptive sampling's neighbour hold (a pixel at the region's edge can stop
earlier than in the frame). Render with `crust:adaptiveNeighbourTolerance = -1`, or at a
sample count below the adaptive minimum, when you need the exact samples.

The Markdown summary opens with one sentence:

```text
The top group carries 8% of the region's luminance and 98% of its second moment:
C<RD'diffuse'><RS'specular'>L via /lights/bulb, in 12 of 25600 samples (13610 records in 10 groups).
```

Read it as: one path — a diffuse bounce off the floor, a mirror ball, then the bulb — takes
12 samples in 25 600 and holds almost all the variance. That is the firefly: a caustic,
which light sampling cannot reach through the mirror. Its brightest record has a
luminance of 56, where the floor's direct light (`C<RD'diffuse'>L`, 78% of the region's
light) never exceeds 0.15.

### 3. See it over the frame

Every expression in the report is written in crust's
[light path expression](@/docs/usd/aovs.md#light-path-expressions) grammar, the light named
by its `crust:light:lpeTag` when it has one. Pasted into an `lpe` RenderVar, it routes the
same paths over the whole frame:

```usda
def RenderVar "caustic"
{
    uniform token dataType = "color3f"
    uniform string sourceName = "C<RD'diffuse'><RS'specular'>L"
    uniform token sourceType = "lpe"
}
```

Render with it and look at the channel: where else the path lands, and how much of the
image it is.

## What the report holds

`--json PATH` (or `-` for stdout) writes `crust-light-paths/1`. The order is the order to
read it in:

- **`summary`**: the sentence above.
- **`groups`**, best first by `second_moment_share`, then `luma_share`. Each has its
  `key`, `expression` and `light`; `luma_share` (its share of the region's light, as the
  pixels average it) and `luma_share_clamped` (the same after the
  [indirect clamp](@/docs/reference/command-line.md#indirect-clamp), over the same total);
  `second_moment_share` (its share of the sum of every sample's squared luminance: the
  noise); `hit_rate` (the fraction of samples that took it); `max_luma`; and
  `top_records`, its paths among `records`. `--group-by` picks the key: `expr+light`
  (default), `expr`, `light` or `none`.
- **`records`**, the `--top` brightest light paths (32 by default, at most 1000): `luma`,
  `contribution` and `contribution_clamped` (RGB), `expression`, `lobe_split` (each
  vertex's share per lobe), `label_ambiguous`, `light`, `pixel`, `coordinates` (where in
  the pixel the sample was taken), `sample`, `depth`, `camera`, and its `group`.
- **`truncated`**: how many paths were kept, of how many.
- **`pixels`**: each pixel's beauty and sample count.
- **`consistency`**: how exactly the paths add back up to the pixels (`max_relative_error`)
  and to each sample (`max_sample_relative_error`) — about 1e-7, rounding. A path is one
  term of the render's own estimate, so they partition it: nothing is counted twice.
- **`clamp_removed_share`**, **`ambiguous_luma_share`**, and **`hints`**.

### Hints

Each hint is a code, the group it is about, the rule that fired it and a next step:

| code | fires when | means |
|------|------------|-------|
| `rare_bright_path` | a group's `hit_rate` < 1e-3 and `second_moment_share` > 0.5 | a firefly: few samples, most of the noise |
| `single_path_dominates` | a group's `luma_share` > 0.8 | the region's light is this one path |
| `clamp_hides_path` | a group's `luma_share_clamped` < half its `luma_share` | the clamp removes most of the path: inspect with `--indirect-clamp 0` to see what it carries |
| `labels_ambiguous` | `ambiguous_luma_share` > 0.2 | see below |

A hint never changes a setting for you; the clamp and the sample count trade the picture
for the noise, and are yours to decide.

## How a path is named

A path's expression names, at each vertex, the lobe carrying most of that vertex's light
toward the path's direction: crust's materials sample one lobe at a time, but every lobe
contributes to every direction, so the lobe that was drawn is often not the one that
carried the light. Each vertex's split between its lobes is listed in `lobe_split`, and a
path whose named lobe carries less than 60% somewhere is `label_ambiguous`.
`ambiguous_luma_share` says how much of the region's light that is. Grouping by expression
is exact for the rest; for the exact split of an ambiguous one, author one expression per
lobe — the AOVs split every sample by lobe.

## Limitations

- At most 100 pixels per inspection, and one camera.
- `prims` is `null` for every path, and `--group-by expr+prims+light` is refused, until
  crust can resolve a hit to its prim.
- Under path guiding, and under adaptive sampling's neighbour hold, the region's samples
  are the scene's paths but not the frame's samples.
- The hint thresholds are first guesses, to be tuned on real scenes.
