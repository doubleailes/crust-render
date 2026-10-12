# Design

## Context

See proposal.md for why. The design starts from what the LPE AOVs already built
(`openspec/specs/aovs/design.md` D10, D11; `crates/crust-core/src/tracer/route.rs`):

- The AOV instantiation of `trace_path` fills a `Route` per camera sample: per
  vertex, the arrival pass-throughs (`Ts`, thin walls), the emission counted there,
  each NEE share **per light sample and per lobe** with the sampled light's symbol,
  the bounce continuation **per lobe** (`f_j·cos/pdf`, summing to `factor`), the
  emission the bounce found next (`next_emit`), the hidden lights crossed, and on
  escape the terminal lights with their shares. That is every emission-ending term
  of the beauty's recurrence, already split by lobe.
- `Route`'s symbols are the compiled LPE's alphabet: `(type, scatter, label)` with
  labels only for what the render's expressions mention, and lights by their tag
  (`RouteCtx::new`). It distinguishes only what the expressions distinguish.
- `C.*[LO]` is pinned bitwise to the beauty, and `labelling_lights_changes_no_value`
  pins that labels route only. Region renders are bit-identical sub-images of the
  frame (`add-render-region`).
- `advance_pixel` knows each sample's index (`sample` in `taken..target`) and its
  filter-importance-sampled offset `(fx, fy)` and weight `wx·wy`; the pixel is
  `Σ wᵢ Lᵢ / Σ wᵢ`.
- No hit → prim table exists yet (`mcp/render.rs`, `NO_PRIM_TABLE`); it is planned
  in `add-identity-aovs-openexrid`.

## Goals / Non-Goals

**Goals:**

- Name the individual path behind a firefly: its expression, light, contribution and
  sample, ranked by luminance.
- Records that partition the region's beauty exactly, so shares add up and nothing
  is attributed twice.
- A report an agent can act on without reading all of it.
- Zero cost for every render that does not inspect.

**Non-Goals:**

- Writing paths as geometry (a USD curves layer). Useful only with a viewer, and
  crust ships none.
- Vertex positions in the records, for the same reason.
- Inspecting more than 100 pixels, or the whole frame: a firefly is local, and the
  cap is what bounds the record store.
- An exact per-lobe *tree* of expressions per record (see D2).
- Running from `crust diagnostic` automatically when `firefly_energy` fires — a
  follow-up once the report has been used on real scenes.
- Making guided renders reproducible (a known gap of guiding, `diagnostics` design).

## Decisions

### D1. A record is one emission-ending term of one sample

Every term of the beauty recurrence that ends on emission is one record:

| term in `Route` | record ends with |
|---|---|
| `emit` at a primary / post-scatter vertex | the emitter's `L` or `O` |
| each NEE entry (one per light sample) | that light's `L` |
| `next_emit` of a bounce | the hit emitter's `L` or `O` |
| each crossing | the crossed light's `L` |
| each terminal light on escape | that light's `L` (dome, distant, backdrop) |
| `segment_emit` (volume emission) | `O` after the volume's events |

A record's value is the product of the prefix — each earlier vertex's `atten` and
full `factor` — times the term (with its MIS weight), exactly the way the gather
builds the beauty. So a sample's records sum to its radiance up to associativity, and
weighted by `wᵢ / Σw` they sum to the pixel. The report states the largest relative
difference over the region (`consistency`), so a broken pair shows up as a number,
not as a plausible-looking table.

So a light path is one connection to one light: one camera sample yields several
records sharing its pixel, coordinates and sample index.

### D2. Label by the dominant lobe; keep the split beside it

The value of a record does not depend on labels; only its expression does. At each
vertex the expression names the lobe with the largest luminance share of that
vertex's value toward the record's direction (the NEE direction for an NEE term,
the sampled direction for the bounce). Each vertex's shares are listed
(`lobe_split`), and a dominant share under 0.6 anywhere sets `label_ambiguous`.

- **Chosen over one record per lobe combination**: the exact alternative branches
  per vertex, lobes^depth records per term. It needs pruning, an `(other)` bucket,
  and buries the top of the list. The exact per-lobe partition stays available: the
  record's expression, authored as an `lpe` RenderVar, routes by D11.
- **Chosen over the picked lobe**: crust's BSDFs are one-sample mixtures, so the
  picked lobe is not the one carrying the value (D11's trap). The dominant lobe is
  computed from the split already in `Route`, so it costs nothing new.
- Consequence: grouping by `expr` is exact for unambiguous records and an
  approximation for ambiguous ones; the report counts the ambiguous records' share
  of luminance so the reader knows how much that is.

### D3. An inspection alphabet; lights by index

The inspection builds its own `RouteCtx` instead of reusing the render's:

- **Lobes**: an alphabet that names every `LobeLabel` (diffuse, specular, coat,
  sheen, transmission, subsurface, translucent), every scatter and event type, so
  the symbol decodes back to `<RG'coat'>` unambiguously.
- **Lights by index, in a side channel.** Encoding one label per light into the
  `u16` symbol (`types × scatters × labels`) overflows at around 1 600 lights, and a
  tag containing a quote cannot be a label (the diagnostic's
  `an_unwritable_tag_is_left_out_not_the_breakdown`). `Route` records the light-list
  index next to each `L` entry; names come from `LightList::name` and tags from the
  import, resolved once when the report is written.
- **The expression names the light by its LPE tag** (`<L.'sky'>`) when it has one,
  plain `L` otherwise, so the expression pasted into a RenderVar routes the same
  light. The prim path is in `light`.

The DFA gather is not run for the inspection: the user's own AOVs are not gathered,
only the beauty and the records.

### D4. Reproducing the frame's samples

The inspection is a region render with the scene's settings and the requested `-s`,
through the AOV instantiation. The beauty is unchanged by that instantiation
(`C.*[LO]` and the zero-AOV render are pinned to it), and the region is a
bit-identical sub-image of the frame, so the records are the samples of the pixels
the user saw — the firefly itself, not a lookalike. A test pins it: the inspected
region's beauty equals the full render's pixels bitwise (Cornell box, `veach_mis`).

Adaptive sampling stops per pixel on that pixel's own samples, so it reproduces too.
Guiding does not (its training depends on the region and on wall-clock), so under
guiding the report says `reproduces_frame: false` with the reason, and runs anyway:
the paths are still the scene's paths, just not the same samples.

### D5. The record store

Per work unit: a bounded min-heap of the top N records, and a hash map from group
key to aggregate. Units merge **in unit order** after the render, and ties order by
(luminance desc, pixel y, pixel x, sample, record index), so the same command
writes the same bytes with any thread count.

- Keys are interned: the expression as a short `Vec<u16>` of inspection symbols
  plus the light index, not strings — no allocation per record in the hot loop.
  Strings are built once per kept record and group, when the report is written.
- The downside of a hash map is that the number of distinct keys has no bound
  (it grows with depth × lobes × lights, and with prims in step 2). The 100-pixel cap
  bounds it: 100 px × 1 024 spp × a few records per sample is about a million
  updates and at most that many keys; in practice a few thousand.
- `none` keeps no map; `light`, `expr`, `expr+light` each fold the same records.

### D6. What is measured

Per record, `luma` is the layout's working-space luminance (the beauty's own, the
one the variance slot uses). The region's total is Σ over pixels of the pixel's
luma. Per group:

- `luma_share`: the group's `Σ wᵢ lumaᵢ / Σw`, summed over pixels, over the total.
- `second_moment_share`: each sample's `x²` (`x` its luma) split among its records by
  `luma_r / x`; summed per group over the region's `Σ x²`. An exact partition,
  because records are non-negative (a negative luma from an out-of-gamut colour is
  taken as 0 for this split only). This is the column that finds fireflies: a rare
  path can have 2% of the mean and 90% of the second moment.
- `hit_rate`: samples with at least one record in the group over samples.
- `max`: the group's largest record.
- `clamped` variants: the indirect clamp scales vertex 0's continuation by the
  factor the beauty computed (`Route::r1`, `aovs` D9); records beyond vertex 0
  carry `contribution_clamped`, and the report states `clamp_removed_share`. The
  ranking uses the unclamped luma: that is where the firefly is.

### D7. The report, for an agent first

`crust-light-paths/1`, following the CLI's shared report shape (`format`,
`crust_version`, snake_case, units in names, `null` for non-finite):

```json
{
  "format": "crust-light-paths/1",
  "crust_version": "…",
  "summary": "1 group carries 91% of the region's luminance and 99% of its second moment: C<RD'diffuse'><RS'specular'><L.'sun'> via /lights/sun, in 3 of 25600 samples.",
  "command": "crust light-paths -i shot.usda -s 256 --region 618,331,628,341",
  "region": [618, 331, 628, 341],
  "settings": { "spp": 256, "indirect_clamp": 10.0, "filter": "gaussian", "…": "…" },
  "reproduces_frame": true,
  "consistency": { "max_relative_error": 2.1e-7 },
  "clamp_removed_share": 0.84,
  "ambiguous_luma_share": 0.02,
  "group_by": "expr+light",
  "groups": [ { "key": "…", "expression": "…", "light": "/lights/sun",
                "luma_share": 0.91, "luma_share_clamped": 0.31,
                "second_moment_share": 0.99, "hit_rate": 1.2e-4,
                "max_luma": 912.0, "records": 3, "top_records": [0, 1, 2] } ],
  "records": [ { "id": 0, "luma": 912.0, "contribution": [..], "contribution_clamped": [..],
                 "expression": "…", "lobe_split": [[{"event": "<RD'diffuse'>", "share": 0.97}, …], …],
                 "label_ambiguous": false, "light": "/lights/sun", "prims": null,
                 "pixel": [623, 336], "coordinates": [623.4159, 336.6139],
                 "sample": 4, "depth": 2, "camera": "/cams/shot" } ],
  "truncated": { "kept": 32, "of": 4811 },
  "prims_note": "crust keeps no table from a hit to its prim yet (add-identity-aovs-openexrid)",
  "hints": [ { "code": "rare_bright_path", "group": 0,
               "next": "author the expression as an lpe RenderVar to see it over the frame" } ]
}
```

Rules that make it consumable:

- **Conclusion first**: `summary` is one sentence built from the first group;
  `groups` (sorted by `second_moment_share`, then `luma_share`) precede `records`.
- **Nothing silently cut**: `truncated`, `ambiguous_luma_share`, `prims_note`,
  `reproduces_frame` with a reason.
- **Every value usable as is**: expressions in crust's LPE grammar with the light's
  tag; prim paths; `region` and `command` in CLI syntax.
- **Hints are codes**, each naming a group and a next step, never a setting that
  changes the picture without saying so: `rare_bright_path` (hit rate < 1e-3 and
  second moment share > 0.5), `single_path_dominates` (luma share > 0.8),
  `clamp_hides_path` (a group whose clamped share is under half its unclamped one),
  `labels_ambiguous` (ambiguous luma share > 0.2). Thresholds are data, like
  `noise::RULES`.
- **Deterministic**: no timing in the JSON (it goes to the log and the Markdown),
  stable ordering (D5).

The Markdown on stdout is rendered from the same value (as `crust diagnostic`
does), so the two cannot disagree; with `--json -` it moves to stderr per the CLI's
report rule.

### D8. Surfaces

- **CLI** `crust light-paths`: the scene/settings flags `diagnostic` and `check`
  share, plus `-s`; `--region` required and capped at 100 pixels (any shape);
  `--top N` (default 32), `--group-by` (default `expr+light`), `--json PATH|-`.
  Usage errors before loading, exit status 2, like the other subcommands.
- **MCP** `light_paths(render_id, region, group_by, top)`: re-renders the region
  with the render's `RenderSettings` (its region and spp) on the session's scene.
  `Job` keeps a hash of the layer bytes it rendered; the call is refused when the
  current layer's hash differs (an edit undone matches again), and when the render
  has not completed. It returns the same JSON value.
- Both call one engine function in `crust-core` (`inspect::run(scene, settings,
  region, &Options) -> Report`); the engine writes no file.

### D9. Cost

Renders that do not inspect are unchanged: the zero-AOV render's instruction count
is pinned, and must not move. The inspection fold lives in the AOV instantiation
behind one per-sample check of an `Option` on the scratch (as `route` already is);
measured with callgrind on an AOV render of the Cornell box, the check must stay
within the noise of an AOV render without it. Recording the light index beside each
`L` entry is the one change to `Route` itself and is done only when the inspection
alphabet is active.

### D10. Two steps for prims

Step 1 ships with `prims: null` and `prims_note`, and refuses
`expr+prims+light`. Step 2, once `add-identity-aovs-openexrid` provides the hit →
prim table, fills `prims` with each vertex's **instance** prim path (resolved from
`(geom_id, prim_id, placement)`, never `/__Prototype_N`, which the stage epoch
renumbers) and enables the grouping. The format stays `/1`: a field that was `null`
gains values, no key changes meaning.

## Risks / Trade-offs

- [The fold and the beauty drift apart: a new emission term added to the
  integrator but not to the records] → `consistency` in every report, and a test
  asserting it below 1e-5 on the sample scenes (Cornell box, `veach_mis`, a volume,
  a thin-walled glass). Added to `docs/architecture.md` § Invariants as a pair.
- [Dominant-lobe grouping misleads on mixed lobes] → `label_ambiguous` per record,
  `ambiguous_luma_share` per report, `lobe_split` always present.
- [Guided scenes are not reproduced] → stated in the report; the inspection still
  names the scene's dominant paths.
- [Wide filters put a sample's coordinates outside its pixel] → documented;
  `coordinates` are where the sample was taken, `pixel` is where it counts.
- [The hint thresholds are guesses] → data, like the diagnostic's rules, tuned on
  real scenes; they never block anything.
- [Large records per sample on deep paths with many light samples] → the 100-pixel
  cap and the top-N heap bound the memory; groups are counted, not stored.

## Migration Plan

Additive: a new subcommand and MCP tool, nothing removed. Step 2 lands with or after
`add-identity-aovs-openexrid` and does not bump the format.

## Open Questions

- The 0.6 ambiguity threshold and the hint thresholds: calibrate on ALab and
  `veach_mis` during implementation; they are report data and change no spec.
