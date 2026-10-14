# Spec Delta

## Purpose

Tells an artist or an agent which individual light paths make a small pixel region
bright or noisy — each path's expression, light, contribution and sample — so the
path behind a firefly can be named and acted on.

## ADDED Requirements

### Requirement: Inspecting a small region

`crust light-paths` SHALL require `--region X0,Y0,X1,Y1` covering at most 100
pixels, and SHALL render only that region. A region larger than 100 pixels, empty,
or outside the frame SHALL be refused as a usage error before the stage is loaded.

#### Scenario: A 10×10 region

- **WHEN** the user runs
  `crust light-paths -i samples/cornellbox.usda --region 100,50,110,60`
- **THEN** 100 pixels are rendered and a report on their light paths is printed

#### Scenario: A region too large

- **WHEN** the user runs `crust light-paths -i scene.usda --region 0,0,20,20`
- **THEN** the arguments are refused as a usage error naming the 100-pixel cap,
  and nothing is loaded

#### Scenario: No region

- **WHEN** the user runs `crust light-paths -i scene.usda`
- **THEN** the arguments are refused as a usage error

### Requirement: The inspection reproduces the render's samples

The inspection SHALL render with the settings and sample count a `crust render` with
the same flags would use. Each inspected pixel's beauty SHALL equal that render's
pixel bit for bit, except under path guiding and under adaptive sampling's neighbour
hold (a region smaller than the frame, a render that can stop early, a non-negative
neighbour tolerance), where the report SHALL say the samples are not the frame's and
why. The report SHALL list every inspected pixel's beauty.

#### Scenario: Same pixels as the frame

- **WHEN** the user runs `crust render -i scene.usda -s 16 -o f.exr` and
  `crust light-paths -i scene.usda -s 16 --region 40,40,50,50 --json r.json`
- **THEN** every pixel's `beauty` in `r.json` equals the pixel of `f.exr` at the
  same coordinates, and the report's `reproduces_frame` is `true`

#### Scenario: A guided scene

- **WHEN** the inspected stage enables path guiding
- **THEN** the report's `reproduces_frame` is `false` with a reason naming guiding,
  and the inspection still runs

#### Scenario: Adaptive sampling holds pixels for their neighbours

- **WHEN** the region is inspected at a sample count past the adaptive minimum, with
  the default neighbour tolerance
- **THEN** the report's `reproduces_frame` is `false` with a reason naming adaptive
  sampling, since a pixel at the region's edge can stop earlier than in the frame

### Requirement: A light path record

Every contribution of a camera sample that ends on emission SHALL be one record:
an NEE connection, emission hit by a bounce, a hidden light crossed, a light at
infinity on escape, or emission from geometry or a volume that is not a light. A
sample's records SHALL sum to its radiance up to rounding.

#### Scenario: Records partition a sample

- **WHEN** a sample's path makes two NEE connections and its bounce hits an area
  light
- **THEN** the sample has three records, sharing its pixel, coordinates and sample
  index, whose contributions sum to the sample's radiance

#### Scenario: Records sum to the pixel

- **WHEN** a pixel's records are weighted by their samples' filter weights and
  summed
- **THEN** the result equals the pixel's beauty up to rounding, and the report's
  `consistency` states the largest relative difference over the region

### Requirement: What a record holds

A record SHALL hold its RGB contribution with and without the indirect clamp, its
luminance, its expression, its light's prim path (`null` for non-light emission),
its pixel, its sub-pixel film coordinates, its sample index, its depth and its
camera's prim path.

#### Scenario: A sky dome seen in two glossy bounces

- **WHEN** a recorded path reflects twice off a glossy coat and escapes to a dome
  light at `/World/lightRig/skyDome` whose `crust:light:lpeTag` is `sky`
- **THEN** its record's `expression` is `C<RG'coat'><RG'coat'><L.'sky'>`, its
  `light` is `/World/lightRig/skyDome`, and its `coordinates` lie within the
  pixel filter's radius of its `pixel`'s centre

#### Scenario: Coordinates under a box filter

- **WHEN** a region is inspected with `--filter box`
- **THEN** every record's `coordinates` lie inside its `pixel`

#### Scenario: An untagged light

- **WHEN** a recorded path ends on a light with no LPE tag
- **THEN** the expression's last event is `L`, and the record's `light` still names
  the light's prim path

#### Scenario: Emission that is not a light

- **WHEN** a path ends on an emissive material that is not a light-list entry
- **THEN** its record's expression ends in `O` and its `light` is `null`

### Requirement: Expressions are labelled by the dominant lobe

A record's expression SHALL name, at each vertex, the lobe carrying the largest
share of that vertex's luminance toward the path's direction, and its light by the
light's LPE tag when it has one. Each vertex's
per-lobe shares SHALL be listed beside it, and a record whose dominant share is
below 0.6 at any vertex SHALL be flagged `label_ambiguous`.

#### Scenario: A coated diffuse surface

- **WHEN** a recorded path's first vertex is a coated diffuse surface whose coat
  carries 30% and diffuse 70% of the value toward the sampled direction
- **THEN** the expression's first event is `<RD'diffuse'>`, its `lobe_split` lists
  diffuse 0.7 and coat 0.3, and the record is not `label_ambiguous`

#### Scenario: An expression can be authored as an AOV

- **WHEN** a record's `expression` is authored as the `sourceName` of an `lpe`
  RenderVar and the region is rendered at the same sample count
- **THEN** the stage imports with no LPE parse warning, and the AOV in that region
  is non-zero wherever the record's pixel is

### Requirement: Ranking and grouping

The report SHALL list the records with the largest luminance, at most `--top N`
(default 32; more than 1000 SHALL be refused as a usage error), and SHALL aggregate every record by `--group-by` (`none`, `light`,
`expr`, `expr+light`; default `expr+light`). Each group SHALL report its share of
the region's luminance and second moment, its hit rate and its largest record.

#### Scenario: A rare bright path

- **WHEN** a region's fireflies come from one caustic path that 1 sample in 10 000
  takes
- **THEN** its group has a hit rate near 1e-4, the largest share of the second
  moment, and the first records of the top list are its records

#### Scenario: Truncation is stated

- **WHEN** a region yields 4 811 records and `--top 32` is used
- **THEN** the report lists 32 records and states that it kept 32 of 4 811

### Requirement: The clamp is reported, not hidden

Each record and each group SHALL report its contribution before and after the
indirect clamp, and the report SHALL state the share of the region's luminance the
clamp removed.

#### Scenario: A clamped caustic

- **WHEN** a region is inspected with the default `--indirect-clamp 10` and its
  brightest indirect record exceeds the clamp
- **THEN** that record's `contribution` is larger than its `contribution_clamped`,
  and the report's `clamp_removed_share` is above zero

### Requirement: Machine-readable report

`--json PATH` (or `-` for stdout) SHALL write `crust-light-paths/1`: a `summary`
sentence first, then `region`, `settings`, `reproduces_frame`, `groups`, `records`,
`truncated` and `hints`. The same command SHALL write the same JSON byte for byte.
Markdown on stdout SHALL be rendered from the same value.

#### Scenario: Reading the conclusion only

- **WHEN** an agent parses the report and reads only `summary` and the first group
- **THEN** it learns the dominant expression, its light and its share of the
  region's luminance

#### Scenario: Run twice

- **WHEN** the same `crust light-paths` command runs twice, with different
  `RAYON_NUM_THREADS`
- **THEN** the two JSON reports are byte-identical

#### Scenario: Hints are codes

- **WHEN** one group holds most of the region's second moment at a hit rate below
  1e-3
- **THEN** `hints` holds an entry with code `rare_bright_path`, naming that group's
  key and expression

### Requirement: Prims along the path

Every record SHALL carry `prims`. Until the renderer can resolve a hit to its prim
path, `prims` SHALL be `null`, the report SHALL hold a `prims_note` saying why, and
`--group-by expr+prims+light` SHALL be refused. Not yet implemented beyond that:
once resolvable, `prims` SHALL list each vertex's instance prim path.

#### Scenario: Before prims are resolvable

- **WHEN** the user runs `crust light-paths ... --group-by expr+prims+light`
- **THEN** the arguments are refused with a message naming the missing prim table

#### Scenario: Prims in the report

- **WHEN** any record is reported before the prim table exists
- **THEN** its `prims` is `null` and the report's `prims_note` names the reason
