## Context

This change builds on `add-usd-render-products-and-aovs` Phase 2. The pieces
it relies on are already there:

- **The lobe split.** At every path vertex the AOV instantiation of
  `trace_path` splits NEE's and the bounce's value by lobe (`LobeSplit`,
  filled by `ShadingPoint::eval_lobes` / `scatter_split`), each share tagged
  with its event (`R`/`T`, scatter kind, OpenPBR label).
- **The routing** (`tracer/route.rs`). It runs the beauty's backward
  recurrence once per expression and leaves each camera sample's value per
  expression in `Route::out`. The film then accumulates it with the beauty's
  filter weights (`aov.rs`, `SampleExtras`).
- **The albedo.** `ShadingPoint::albedo` sums every lobe's tint at the first
  non-delta hit, following delta chains. It aliases `diffuse_albedo` to
  itself.

What is missing is a per-lobe *colour* to divide by, and a place to divide.
See `proposal.md` for why the division must happen per sample.

## Goals / Non-Goals

**Goals:**

- Raw diffuse light that multiplies back to the diffuse lighting per sample.
- The same mechanism for any expression that starts at a diffuse reflection
  (light groups, `<RD'diffuse'>` only, …).
- Nothing to pay for a render that asks for neither.

**Non-Goals:**

- Raw passes for other lobes: V-Ray's `RawReflection` / `ReflectionFilter`,
  `RawRefraction`. The mechanism generalises — a filter per lobe kind — but
  a glossy lobe's "colour" is a Fresnel tint that depends on the angle, so
  its definition needs its own decision. A later change can add it.
- Following mirrors and glass to the first diffuse surface behind them. Raw
  expressions start at the camera's first surface, by definition (D4).
- A pixel-level identity at texture and object edges. It cannot hold
  (`mean(a·b) ≠ mean(a)·mean(b)`); V-Ray documents the same.

## Decisions

### D1. Divide per camera sample, in the renderer

`raw = (the expression's value for this sample) / (this sample's diffuse
filter)`, then filtered like any colour AOV.

**Rejected: divide the pixels in the compositor.** That is what an artist
can already do with `C<RD>[LO]` and an albedo. It produces halos at every
edge and texture boundary, where a pixel mixes two colours.

### D2. Several diffuse lobes: the ratio of sums

A surface can carry more than one diffuse lobe: a MaterialX `mix` of two
diffuse BSDFs, say. The raw value is the expression's value (which already
sums the lobes) divided by the **sum** of the lobes' filters. So
`raw × filter` is the lighting exactly, whatever the number of lobes.

**Rejected: divide each lobe's share by its own colour and add the results.**
Two half-weight lobes would each return the full light, and the sum would
double it.

### D3. The filter is the lobe's colour with its layer weights, not its directional albedo

A diffuse lobe's share, as `eval_split` computes it, is
`base_atten · coat_atten(ωo, ωi) · dark · eon(ρ, ωo, ωi) · (1 − F̄)` for
OpenPBR, with `ρ = diffuse_color · base_weight · (1 − metalness) · (1 −
transmission)`. The filter keeps the factors that describe the surface's
colour and do not depend on direction: `ρ · (1 − F̄) · base_atten · dark`.
The coat's passage (`coat_atten`) depends on the view and light directions,
so it stays in the raw light: it is light lost on the way, not surface
colour.

- **MaterialX closure:** each `Diffuse` leaf's `color × weight` (its weight
  already carries the tree's layering). `Translucent` and `Subsurface` leaves
  are transmission (`T`), not diffuse reflection, so they have no filter.
- **Materials queried directly** (no OpenPBR, no closure): no diffuse
  filter, so 0. Their raw values are 0.

**Rejected: divide by the directional albedo** (the lobe's reflectance
integrated over the hemisphere). It is exact, but view-dependent, so the
filter AOV would shade. It also needs a table or extra samples.

**Accepted trade-off:** EON's multiple-scattering term is nonlinear in `ρ`.
So raw light keeps a faint dependence on the colour (well below a percent
for ordinary albedos). The identity `raw × filter = lighting` is exact
regardless, by construction.

### D4. The filter is taken at vertex 0

Raw expressions must start `C<RD>` (D5). So the diffuse reflection they
divide by is the camera ray's first surface, after cutouts are passed. The
AOV instantiation records that surface's diffuse filter at vertex 0, next to
`FirstHit`, only when a raw or `diffuse_albedo` AOV is requested.
`diffuse_albedo` accumulates the same per-sample value, so the two AOVs
share their divisor exactly.

This is why `diffuse_albedo` does not follow delta chains, unlike `albedo`.
Through a glass pane, the pane's diffuse filter is 0, and `C<RD>…`
expressions see nothing either: a refraction is `T`.

### D5. "Starts with a diffuse reflection" is a DFA check, at import

For expression `i`, take the state after `C`. Every event that is not `R`
with scatter `D` (any label) must lead to a state from which `i` cannot
accept (`Lpe::live_mask` has no bit `i`). The camera's direct emission
counts too: `L` or `O` straight after `C` must not accept `i`. This is a
property of the language, so it covers every spelling:
`C<RD>.*<L.'key'>`, `C<RD'diffuse'>.*L`, `C(<RD>|<RD'diffuse'>)L`. A bare
label is refused: `C'diffuse'.*L` is `<..'diffuse'>`, which also matches a
transmission or an emission carrying that label. Crust never emits one, but
the check judges the language, not crust's lobes. Write `<RD'diffuse'>`.

The importer compiles the expression, refuses the var with one `WARN` when
the check fails, and accepts it otherwise. `rawLight` / `rawGI` /
`rawTotalLight` are fixed expressions that pass by construction.

**Rejected: allow any expression and divide wherever.** `C.*[LO]` raw would
divide the sky, glass and mirrors by a diffuse colour they do not have.

### D6. Where the division happens: in the film, per sample

The raw values are not separate routes. A raw var is an LPE var with a
`raw` flag; its film slot is keyed by (expression, raw), so `C<RD>[LO]` and
`rawLight` share one DFA bit. At accumulation, the film divides the
sample's routed value by the sample's diffuse filter, channel by channel,
and writes 0 below 1e-4. The beauty's indirect clamp has already been
applied inside the routed value, so the identity holds clamped as well.

**Rejected: divide inside the gather.** It would need a second recurrence
per raw expression, for a division that only ever applies at the end.

### D7. Names

`rawLight`, `rawGI` and `rawTotalLight` are canonical: camel case, like
`sampleCount` and `primId`. V-Ray's `RawLighting`, `RawGI` and
`RawTotalLighting` are aliases. `diffuse_albedo` (Arnold's name) becomes
canonical, with V-Ray's `DiffuseFilter` as an alias. `crust:aov:raw` sits on
the RenderVar under crust's own namespace, as every crust-specific attribute
does.

## Risks / Trade-offs

- **[Risk] A near-black diffuse colour turns light noise into fireflies.**
  → Channels whose filter is below 1e-4 are 0 (spec). A very dark surface
  then has a dark raw pass. That is visible, but bounded.
- **[Trade-off] The identity fails per pixel at edges and on textures.** It
  holds per sample, and per pixel wherever the filter is constant. This is
  documented in the user guide with the reason. V-Ray behaves the same.
- **[Risk] `diffuse_albedo` changes meaning** for anyone using the Phase 2
  alias. → Phase 2 is not merged yet; if it merges first, the change is
  called out in the user docs and the design record. The all-lobe value
  stays available as `albedo`.
- **[Risk] Cost.** → One filter evaluation at vertex 0 when requested, and
  one division per raw slot per sample. No per-vertex work. The zero-request
  path is unchanged; a callgrind check pins it, as for the other AOVs.

## Migration Plan

Lands after `add-usd-render-products-and-aovs` is archived, because it
modifies that change's `aovs` capability. Nothing changes for a stage that
asks for none of the new sources. Rollback is reverting the change: no data
or format depends on it.
