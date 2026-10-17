# light-path-inspection — design record

> Design record for the **light-path-inspection** capability (`crust light-paths`,
> the MCP `light_paths` tool): the reasoning, measurements and history behind the
> behaviour `spec.md` states. Introduced by the `add-light-path-inspector` change;
> its design is carried here with what implementation found.
> `docs/architecture.md` is the map; the user's view is
> `site/content/docs/help/light-paths.md`.

## Why

A firefly is one sample. `crust diagnostic` says how noisy a render is, and the LPE
AOVs split the image by the expressions an artist already thought to ask for, but
neither answers the question asked first: *which path, through which light, made
this bright pixel?* The inspector answers it for a few pixels: every light path of
every sample, ranked, grouped, each with its expression, light, contribution and
sample — shaped first for an agent to read.

## Shape

- **Engine in `crust-core/src/inspect/`, thin hosts.** `inspect::run(&mut Renderer,
  region, &Options) -> Report`; the CLI (`crust light-paths`) and the MCP tool both
  call it, print `Report::to_markdown()` or answer `Report::to_json()`'s value. The
  Markdown is rendered from the `Report`, so it cannot disagree with the JSON. The
  engine writes nothing.
- **A renderer, not a scene.** The design named `run(scene, settings, region, …)`;
  it takes `&mut Renderer` instead, because an MCP session keeps its scene as a
  renderer that every render retunes. `run` retunes to the renderer's own settings
  with the region applied, renders, and retunes back, so the light selection (the
  `learned` pre-pass included) is kept: only the region changes.
- **One render path.** `Renderer::render_inspection` is `render_impl` with an AOV
  layout that carries no slot, the inspection's routing
  (`RouteCtx::inspection`) and the store configuration (`AovLayout::inspect`). It is
  the AOV instantiation of `trace_path`, whose beauty is the beauty-only render's bit
  for bit (`aovs` design D8), and a region render, whose pixels are the frame's
  (`rendering` design record, "Rendering a region"). Tiled, silent, no control.

## D1. A record is one emission-ending term of one sample

`Route::fold` (`tracer/route.rs`) walks the path's `VertexRec`s and its `Route`
forward, keeping the prefix product (each earlier vertex's `atten · factor`, the
whole factor, as the gather uses it), and hands out one `PathTerm` per term that
ends on emission:

| term | ends with |
|------|-----------|
| `segment_emit` (volume emission on the arriving segment) | `O` after the vertex's arrival events |
| `emit_here` (primary and post-scatter vertices) | the emitter's `L` or `O` |
| each NEE light sample | that light's `L` |
| `next_emit · next_emit_weight` of the bounce | the hit emitter's `L` or `O` |
| each crossing (hidden lights, link twins) | the crossed light's `L` |
| the escaped segment's emission, each light at infinity | `O`, each light's `L` (a backdrop's too) |

A term's value is the prefix times the term with its MIS weight, so a sample's terms
sum to its radiance up to associativity, and weighted by `wᵢ / Σw` to the pixel.
A term whose value is exactly zero is no record. An NEE sample's value is the sum
of its per-lobe entries (the beauty adds the whole `brdf_value`; OpenPBR's split sums
to it within rounding, a MaterialX closure's bitwise).

The light sample boundaries are a side channel: `Route::nee` pushes `(end, light)`
per sample into `nee_samples` when the path is inspected, because two samples of
one light at one vertex are two records (the spec's "two NEE connections").

**Measured.** `consistency` — the largest relative difference between a sample's
radiance and its records, and between a pixel's weighted sums — is 1.1e-7 to
3.4e-7 on the Cornell box, `veach_mis`, `fog` (volume scatters and emission) and
`thin_window` (thin-walled glass passed straight through, `<TS'transmission'>`),
clamp off and on (`records_rebuild_every_sample_and_pixel`,
`clamped_records_sum_to_the_clamped_beauty`).

## D2. Labelled by the dominant lobe, the split beside it

A record's value does not depend on labels; its expression does. At each vertex the
expression names the event with the largest luminance share of that vertex's value
toward the record's direction: the NEE sample's entries for an NEE term, the bounce's
otherwise. Lobes of one event are merged first (a MaterialX closure can have two
diffuse leaves). The shares are listed per vertex (`lobe_split`), and a dominant
share under 0.6 anywhere sets `label_ambiguous`; `ambiguous_luma_share` says how
much of the region's light that is.

- **Not one record per lobe combination**: lobes^depth records per term, pruning and
  an `(other)` bucket, and the top of the list buried.
- **Not the picked lobe**: crust's BSDFs are one-sample mixtures, so the picked lobe
  is not the one carrying the value (`aovs` D11's trap). The split is already in
  `Route`, so the dominant lobe costs nothing new.
- A vertex with no event (a random walk's exit, `NO_EVENT`) adds no event and no
  split; `depth` counts the scattering events, so `lobe_split.len() == depth`.
- A negative (out-of-gamut) or NaN luminance carries no share; a vertex whose lobes
  carry none splits evenly.

## D3. The inspection alphabet; lights by index

`RouteCtx::inspection(lights)` compiles no expression. Its symbols are
`lpe::inspection_symbol`: `Lpe::symbol`'s layout over "no label" and the seven lobe
labels (`INSPECTION_LABELS`), so every lobe, scatter and event type has its own
symbol and `lpe::inspection_event` decodes it back; `inspection_text` writes it in
the grammar (`<RG'coat'>`, `<Ts>`, `V`, `O`, `L`, `<L.'sky'>`), pinned by
`every_lobe_event_round_trips_through_the_inspection_alphabet`: the text of each
lobe event, as `C<text>L`, accepts its own word and no other lobe's.

Every light is a plain `L` in the alphabet. Which light travels beside the symbol:
`RouteCtx::emitter` returns the light-list index with the symbol, and `Route`
stores it (`RouteVertex::emit_light`, `next_emit_light`, `nee_samples`,
`crossing_lights`, `terminal_lights`) only when `Route::begin(true)` — an LPE render
keeps the stores it had. One label per light in the `u16` symbol would overflow
around 1 600 lights, and a tag holding a quote cannot be a label (the diagnostic's
`an_unwritable_tag_is_left_out_not_the_breakdown`). Names (`LightList::name`, the
prim path) and tags (`LightList::lpe_tag`) are read once, when the report is
written; an expression names a light by its tag, plain `L` when untagged or when
the tag holds a quote. A backdrop has no index (`NO_LIGHT`): its records end in `L`
with `light: null`.

The DFA gather is not run for an inspection (`Route::gather` returns at once
without an `Lpe`), and the stage's own AOVs are not gathered.

## D4. Reproducing the frame's samples

The region render takes the samples of the frame's pixels: same seeds, same tile
grid, same per-pixel sample indices. A test pins it bitwise against full renders of
the Cornell box and `veach_mis` at 16 spp (`the_region_is_the_frame_bit_for_bit`),
and the CLI against `crust render`'s EXR (`the_inspected_pixels_are_the_rendered_ones`).

Two exceptions, each reported as `reproduces_frame: false` with its reason:

- **Path guiding** trains on the region's paths only.
- **Adaptive sampling's neighbour hold.** Found in implementation: the design
  assumed a pixel stops on its own samples, but the hold compares cross neighbours,
  and a neighbour outside the region is never sampled (`held_by_neighbour` treats it
  as absent). A pixel at the region's edge can then stop earlier than in the frame,
  and the difference walks inward round by round. The report says so whenever the
  render can stop early (`samples_adaptively`) with a non-negative tolerance on a
  region smaller than the frame — the default settings at 128 spp included. A fixed
  sample count, or `crust:adaptiveNeighbourTolerance = -1`, reproduces exactly.

Either way the records are the scene's paths; only their samples differ.

## D5. The record store

Per work unit (`tracer/inspection.rs`, `UnitStore`, on `UnitWork::inspect`): a bounded
min-heap of the top N records (N at most `MAX_TOP`, 1 000: with the pixel cap, what
bounds the store and the report — the groups count every record whatever N keeps)
and a hash map from group key to aggregate. Units
merge **in unit order** after the render (`collect_inspection`, `Collected::merge`),
and records rank by a total order — luminance (`total_cmp`) descending, then image
row, column, sample and the record's index within its sample ascending — so the
kept records and every group's sums are the same with any thread count, and the
same command writes the same bytes (`the_report_is_the_same_bytes_with_any_thread_count`,
one and eight threads).

- **Keys are interned** `[u16]`: the record's events and two `u16`s for what tells
  its end apart — the light's index (`expr+light`), its tag's id (`expr`: lights
  sharing a tag share `<L.'tag'>`), or under `light` the ending event and the index
  (a backdrop's `L` and an `O` both have no index). Lookups borrow a reusable
  buffer; a key is allocated once, when its group is new. A record is copied out of
  the fold only when it enters the heap.
- **Per-pixel sums per group** (`GroupAcc::pixels`, a short list with a cache of the
  last pixel: a pixel's samples come in runs), because a group's luminance share is
  its `Σ wᵢ lumaᵢ / Σ w` per pixel, and `Σ w` is known only once the pixel is done.
  The 100-pixel cap bounds the list.
- **Unbounded keys.** The number of groups grows with depth × lobes × lights; the
  cap bounds it (100 px × 1 024 spp × a few records a sample ≈ a million updates);
  in practice tens to a few hundred (186 on `fog` at 16 spp).
- `none` keeps no map.

## D6. What is measured

Per record, `luma` is the light list's working-space luminance (`LightList::luma`,
the beauty's). Per pixel the store mirrors the beauty's accumulators (`Σ wᵢ`,
`Σ wᵢ Lᵢ`, the `/ taken` fallback when the weights cancel), which is also what the
pixel `consistency` compares the records against. Per group:

- `luma_share`: Σ over pixels of the group's `Σ wᵢ lumaᵢ / Σ w`, over the same for
  every record — so the shares partition the region exactly (to 1e-6,
  `shares_partition_the_region`, every grouping).
- `second_moment_share`: each sample's `x² = (Σ_r x_r)²`, `x_r = |w| · max(luma_r, 0)`,
  split among its records by `x_r / x`; over the region's sum. An exact partition. This
  is what finds fireflies: on the caustic test scene one path holds 98% of the second
  moment with 8% of the light, in 12 of 25 600 samples
  (`a_rare_caustic_ranks_first_by_its_second_moment`).
- `hit_rate`: samples with a record in the group, over samples.
- `max_luma`, `records`, `top_records`.
- **The clamp.** The beauty's clamp factor (`limit / peak` of vertex 0's
  continuation, `Route::r1` — `aovs` D9) scales every record past the primary vertex
  (`contribution_clamped`); the primary vertex's records (its NEE, emission, and what
  its bounce finds, the two halves of direct light) are never clamped.
  `luma_share_clamped` is the group's clamped luminance over the *unclamped* total,
  so the clamped shares sum to `1 − clamp_removed_share`, and a group the clamp cuts
  shows it as a smaller share. The ranking uses the unclamped luminance: that is
  where the firefly is.

## D7. The report, for an agent first

`crust-light-paths/1`, the shared report shape (`format`, `crust_version`,
snake_case, `null` for non-finite). Key order: `summary`, `command`, `region`,
`resolution`, `camera`, `settings`, `reproduces_frame`, `reproduces_frame_reason`,
`samples`, `consistency`, `clamp_removed_share`, `ambiguous_luma_share`, `group_by`,
`groups`, `records`, `truncated`, `pixels`, `prims_note`, `hints`.

- **Conclusion first**: `summary` is one sentence from the first group (from the
  brightest record under `none`); groups precede records. A report whose samples are
  not the frame's says so in the sentence too.
- **Nothing silently cut**: `truncated`, `ambiguous_luma_share`, `prims_note`,
  `reproduces_frame_reason`.
- **Usable as is**: expressions compile (`every_expression_routes_its_path`, which
  also renders each of the first records' expressions as an `lpe` var and finds it
  non-zero at the record's pixel); prim paths; `region` and `command` in CLI syntax.
- **`pixels`**, added in implementation: every inspected pixel's beauty and sample
  count, which the spec's "same pixels as the frame" reads.
- **Coordinates** are image space where pixel `(x, y)` covers `[x, x+1] × [y, y+1]`:
  `x = i + fx`, `y = height − (j + fy)`. The filter's offset `fx` already carries the
  half pixel (`FilterSampler::sample` returns `0.5 + x`), so the task's
  `i + 0.5 + fx` would have put a box filter's samples a half pixel off.
- **Hints are codes**, data in `inspect::HINTS` with their thresholds
  (`RARE_HIT_RATE`, …): `rare_bright_path`, `single_path_dominates`,
  `clamp_hides_path`, `labels_ambiguous`. A group rule names the first group, in
  report order, it holds for: one hint per code.
- **Deterministic**: no timing (the CLI logs it), stable ordering (D5).

## D8. Surfaces

- **CLI** `crust light-paths` (`main.rs`): the scene flags `check` and `diagnostic`
  take, plus `render`'s `-s` — its settings are `apply_overrides`' order, `-s` then
  the scene flags (`light_paths_settings`, pinned against `render`'s). `--region`
  required; `--top` (32, at most 1 000), `--group-by` (`expr+light`), `--json PATH|-`.
  `light_paths_request` refuses, as usage errors before the import (exit 2): no `-i`,
  no `--region`, and what `inspect::check_request` refuses — a region empty or over
  100 pixels (counted with `checked_mul`: a rectangle out to `usize::MAX` wraps
  `PixelRect::area` to 1, which the frame would then clip to the whole frame; and
  `PixelRect::is_empty` compares the sides, since its old `area() == 0` overflowed
  first), more
  than 1 000 records, `expr+prims+light`. The engine's refusals are typed,
  `Error::Inspection(Refusal)`, and a region with no pixel in the frame is the
  settings' own `Error::EmptyRegion`; the hosts print them. Clap refuses
  `-o` and the other render flags. A region partly outside the frame is clipped, as
  `--region` is; one wholly outside is an error after the import (exit 1). With
  `--json -` the Markdown goes to stderr.
- **MCP** `light_paths(render_id, region, group_by, top)` (`mcp/session.rs`). Each
  `Job` keeps a hash of the layer bytes it rendered (`Open::layer_hash`, at
  `start_render`); the call is refused when the saved layer's hash differs (an undo
  back to it matches again — the scene of an in-place update is the full import's),
  when the render did not complete, when another render is still running (it holds
  the renderer: `Renders::running` joins a returned one), for a region outside the
  render's, and for what `check_request` refuses. It re-renders with the job's `RenderSettings`
  (`retune`, then back), and answers the report's JSON value; its `command` names the
  session's layer, which `crust light-paths -i <layer>` reproduces.

## D9. Cost

Renders that do not inspect run the code they ran:

- The beauty-only render (callgrind, Cornell box at 2 spp, one thread): 4 222 616 455
  instructions before, 4 222 604 148 after — the difference is the import's
  hash-ordered sorts; `advance_pixel::<_, false, _, _>` is 828 573 528 both times,
  and `render_pass` +2 714, three instructions a work unit (its store's `None`).
- A trap fallen into once: the first build read the layout *after* `render_pass`'s
  pixel gather loop to merge the records, which kept the layout live through the
  loop, spilled `min_taken` and cost every render two instructions a pixel
  (+0.011%). The merge is taken before the loop, beside the film, which reads the
  layout too, and is out of line (`collect_inspection`).
- The inspection's fold runs in the AOV instantiation behind one check of an
  `Option` on the unit (`UnitWork::inspect`) per sample; recording the light index
  beside each `L` entry is behind `Route::inspect`, a flag the AOV instantiation
  reads per `L` entry. On `samples/aovs_lpe.usda` (nine LPE vars, 2 spp, one
  thread) `advance_pixel::<_, true, _, _>` gained 4.4 M instructions (+0.12% of the
  render) and `Route::nee` 0.36 M, while `Route::gather` — its source unchanged —
  lost 20.5 M to codegen over the reshaped `Route`: 3 616 674 190 before,
  3 596 790 211 after, every channel bitwise the same.

## D10. Prims, in two steps

Every record carries `prims`. Until a hit is resolved to its prim, it is
`null`, `prims_note` says why, and `expr+prims+light` is refused. The tables
that resolve it now exist (`crust_core::identity::Identity`, from
`add-identity-aovs-openexrid`, kept when an identity AOV asks for them); step 2
is to keep them for an inspection too and read them per vertex. Step 2 fills `prims` with each vertex's **instance**
prim path (from `(geom_id, prim_id, placement)`, never `/__Prototype_N`, which the
stage epoch renumbers), enables the grouping, and drops `prims_note`; the format
stays `/1`: a field that was `null` gains values.

## Known gaps

- **No prims** (D10): `prims` is `null`, `expr+prims+light` refused.
- **Guided and neighbour-held renders are not reproduced** (D4): the records are the
  scene's paths, not the frame's samples.
- **Dominant-lobe grouping approximates mixed vertices** (D2): exact for unambiguous
  records; `ambiguous_luma_share` says how much is not.
- **One camera.** Records carry the render camera's path; a stage rendered through
  several cameras is inspected one camera at a time (`--camera`).
- **The hint thresholds and the 0.6 ambiguity threshold are first guesses**, not yet
  calibrated on ALab: data in `inspect::HINTS` and `AMBIGUOUS_SHARE`, changing no spec.
- **Not run from `crust diagnostic`** when `firefly_energy` fires: a follow-up once
  the report has been used on real scenes.
