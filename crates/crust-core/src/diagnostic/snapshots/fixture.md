# crust diagnostic: samples/cornellbox.usda

## Verdict

- **Top time sink:** Trace (41%)
- **Top noise source:** indirect_diffuse
- **Best change:** `--light-samples 2` (ΔEff 1.25)
- **Converged:** no

## Scene

- path: `samples/cornellbox.usda`
- frame: –
- camera: –
- resolution: 640×360
- region: full frame

## Effective settings

| setting | value | flag | USD attribute |
|---|---|---|---|
| light_selection | power | --light-selection | crust:lightSelection |
| path_guiding | false | – | crust:pathGuiding |

## Run

- budget 120 s, used 61.23 s (import 0.5 s, not counted), 16 threads, 3 repeats
- probe conditions: indirect clamp off, adaptive sampling off, fixed spp true, 640×360
- P1: 4 s
- exit status 0

## Static findings

- **textures_without_tx** (time): 3 UV texture(s) have no .tx beside them. Evidence: textures 3. Action: set `--auto-tx` = on.

## Baseline

- 16 spp full frame in 3.2 s (setup 0 s, render 3.2 s; calibration 0.2 s)
- MRSE 0.01235, 12350000 rays/s
- mean path length 1.5, Russian roulette kill rate 25%, ended by max depth 0%, shadow rays per vertex 0.5
- profile: Trace 41%
- cache hit rates: texture –, Ptex –
- peak memory 1024.0 MiB

## Noise breakdown

Each row's error is its own (`var / mean²`) and against the beauty (`var / beauty²`); rows are not shares of the beauty's variance.

| component | expression | mean luminance | relative error | vs beauty |
|---|---|---|---|---|
| indirect_diffuse | `C<RD>.+[LO]` | 0.2 | 0.03 | 0.01 |

Light groups by: none.

## Crops

| crop | rect | reason | relative variance | baseline thread-s | reference MRSE |
|---|---|---|---|---|---|
| crop_a | [0, 0, 128, 128] | highest_relative_variance | 0.5 | 0.3 | 0.001 |

## Trials

| trial | overall ΔEff | verdict | per crop (median [min, max]: verdict) |
|---|---|---|---|
| light_samples=2 | 1.25 | better | crop_a @16 spp: 1.25 [1.2, 1.3]: better |

## Sample budget (estimates)

- target MRSE 0.0025 (from variance_threshold), with light_samples=2 settings
- estimated spp to reach it: 48; projected full-frame render time 9.6 s (sampling only)

## Picture-changing settings (measured, not ranked)

- indirect clamp 10: removes 0.4% of the image's luminance (mean 0.001 per pixel), touching 2% of the pixels
- max depth 32: 0% of paths ended there
- subdivision: meshes per level []; the scene holds 0 unique triangles in 0.0 MiB of kernel geometry; its build time is not recorded

## Not tried

- combined (tier 1): not_applicable — 1 factor(s) better; a combination needs two

## Suggestions

- **light_samples=2**: flag `--light-samples`, attribute `crust:lightSamples`, value `2`, expected ΔEff 1.25 (evidence: light_samples=2)

## Converged

No.

## Suggested command

```sh
crust render -i samples/cornellbox.usda --light-samples 2
```

## Deltas

not comparable: a different camera
