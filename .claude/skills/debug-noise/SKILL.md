---
name: debug-noise
description: Debug a noisy crust render — measure where the noise comes from with `crust diagnostic`, locate the firefly pixels, name the light path behind them with `crust light-paths`, and recommend the setting (or the scene change) that addresses it. Use when the user says a render is noisy, grainy, has fireflies or speckles, converges slowly, or asks which settings would make it cleaner.
allowed-tools: Bash(cargo:*), Bash(target/release/crust:*), Bash(python3:*), Bash(mkdir:*), Bash(mktemp:*), Read
---

Find out **why** a crust render is noisy and **what to do about it**, backed by numbers
rather than by looking at the image. Two tools do the measuring:

- `crust diagnostic`: *which kind* of light carries the noise (the noise breakdown), whether
  fireflies matter (the picture findings), and which unbiased setting is measurably more
  efficient (tier-1 trials, verdicts, suggestions).
- `crust light-paths`: *which exact path* makes a handful of pixels bright: its light path
  expression, its light, how often it is taken, and its share of the noise.

Diagnose the frame first, then inspect the pixels. Inspecting before diagnosing means
guessing which pixels matter.

Reference, read when a step below is unclear: `site/content/docs/help/diagnosing-a-render.md`,
`site/content/docs/help/light-paths.md`, `site/content/docs/reference/command-line.md`
(`## diagnostic`, `## light-paths`). Design records: `openspec/specs/diagnostics/design.md`,
`openspec/specs/light-path-inspection/design.md`.

**Input**: a stage (`-i`), plus whatever the user rendered it with: frame (`-f`),
camera, and any `--strategy` / `--light-selection` / `--light-samples*` /
`--indirect-clamp` / subdivision flag. Call these the **scene flags** and pass them
unchanged to every command below, so every measurement is of the render the user saw.
The sample count is kept apart as **`<spp>`**: only `crust render` and `crust light-paths`
take `-s`; `check` and `diagnostic` refuse it. With no `-s` from the user, leave it off
everywhere (the stage's own count applies). If the user named a pixel or an area, note
it. If no stage is given, ask for one.

## Steps

**`$OUT`** is an absolute directory for every report and image; write nothing beside
the stage. Use the session scratchpad when the system prompt names one, else create one
with `mktemp -d`; `mkdir -p` it before step 1. Shell variables do not survive between
tool calls, so either write the absolute path literally in each command or start each
command with `OUT=<that path>;`. Never run a command with `$OUT` unset: the paths would
land at `/`.

0. **Build once.** `cargo build --release`, then use `target/release/crust` (abbreviated
   `crust` below). Don't use `cargo run` per step: every step would re-check the build.

1. **Check the stage.** `crust check -i <stage> <scene flags>`. Read its warnings: a
   refused or approximated light or material can be the cause of "noise" (e.g. an
   emitter that is not a light, so only bounces find it). Also note its findings that need
   no render (`visualization_strategy`, many lights picked uniformly, textures without
   `.tx`). Fix nothing yet; carry these into the summary.

2. **Diagnose.**

   ```bash
   crust diagnostic -i <stage> <scene flags> --budget 2m --json $OUT/d1.json > $OUT/d1.md
   ```

   No `-s` here: the diagnostic picks its own sample counts. Default budget 2m;
   on a heavy production stage (import over a minute, baseline at 4 spp) use 5m and say so.
   If the user already named the area, add `--region` with a rectangle of at least 128
   pixels around it, so every trial spends its samples there.

   Read `d1.md` top to bottom (it is ordered to be read that way), then pull the numbers
   from `d1.json`:

   - **`noise_breakdown`**: `dominant` names the row with the largest error *against the
     image* (`components[].relative_error_vs_beauty`); that is the noise's source. Direct rows (`direct_diffuse`, `direct_glossy`) point at light
     selection and light samples; indirect rows (`indirect_diffuse`, `indirect_glossy`)
     at guiding and indirect light samples; `transmission` / `volume` at what the camera
     sees through. `unlit_emitters` overlaps the rest: large means emissive geometry
     that is not a light, which only BSDF bounces reach; the finding of that name lists
     the prims (and, for a mesh that applies `MeshLightAPI` but was refused, the warning
     code that says why). Light groups, when present, say which light.
   - **The picture findings**: `firefly_energy` (the top 0.1% of pixels hold ≥ 20% of the
     luminance), `light_sampling_misses` (light sampling reaches < 90% of a crop's
     energy), `clamp_bias` (the clamp removes ≥ 5%), `visualization_strategy`. Their
     action is `none` on purpose: no setting fixes them.
   - **`trials` and `suggestions`**: only a `better` trial with overall ΔEff and ΔEff at
     the target ≥ 1.10 is a suggestion. `inconclusive` / `mixed` mean "does not matter
     here"; `insufficient_samples` means "rerun with a larger `--budget` or a `--region`".
     A `biased` tier-1 trial is a renderer bug: report it as such, don't recommend it.
   - **`crops`**: the `highest_relative_variance` crop's `rect` is where to look next.
   - **`sample_budget.adaptive`** (the adaptive sampling sweep): `authored` and `best` give
     each crop's predicted mean spp, error and time, and `target_reached` says whether any
     setting reaches the target. `front` is the time/error trade per crop. Trust `best` only
     when every `verification[].sample_count_matches` is true; an `adaptive_sampling.*`
     suggestion already needs that. When the user asks where adaptive sampling spends its
     time, rerun with `--adaptive-maps $OUT/adaptive.exr`. Its `authored.sampleCount` layer
     shows where the samples went, and `authored.variance` whether they were enough there.

3. **Decide whether to inspect paths.** Go to step 4 when any of these hold:
   `firefly_energy` or `light_sampling_misses` fired; the dominant row is indirect or
   `unlit_emitters`; the user pointed at specific bright pixels. Otherwise the noise is
   broad and well sampled: skip to step 6 and recommend the suggestions and the sample
   budget (`sample_budget`).

4. **Find the firefly pixels.** Neither tool lists pixel coordinates, so render the noisy
   crop at the user's sample count and pick the isolated outliers:

   ```bash
   crust render -i <stage> <scene flags> -s <spp> --region <crop rect> -o $OUT/crop.exr
   python3 .claude/skills/debug-noise/find_fireflies.py $OUT/crop.png --offset <x0>,<y0>
   ```

   The PNG holds only the region, so `--offset` is the crop's top-left corner; the script
   prints image-space pixels and a ready `--region` of 10×10 (100 pixels, the
   light-paths limit) around each. If the user named a pixel, use a 10×10 window around it
   instead. The PNG is clamped and 8-bit, so a firefly inside an area that already clips
   cannot be ranked: the script lists such areas separately, with a window each. A large
   clipped area is usually a light seen directly or its highlight (the `emission` and
   `direct_glossy` rows); inspect one of its windows in step 5 only when the dominant
   row is another one, or when the user points there. If it finds neither, the noise is grain, not fireflies:
   inspect one 10×10 window in the middle of the crop anyway, to name the dominant path.

5. **Name the paths.** For the strongest one or two windows:

   ```bash
   crust light-paths -i <stage> <scene flags> -s <spp> --region <window> --json $OUT/lp1.json > $OUT/lp1.md
   ```

   Same `-s` as the render: the region's samples are then the frame's, bit for bit
   (check `reproduces_frame`; guiding and adaptive neighbour hold break it, and the report
   says why). Read:

   - **`summary`**: one sentence, the top group by second moment.
   - **`groups`**: `second_moment_share` is the noise, `luma_share` the light,
     `hit_rate` how rare. A group with a big second-moment share, a small luma share and
     a tiny hit rate (< 1e-3) *is* the firefly. A dominant group with a high hit rate
     (≥ 1e-2) is not a firefly but a hard feature the outlier finder also catches, e.g. a
     light's glossy highlight (`C<RG…>L` at depth 1): its noise is the light-sampling
     kind, which the diagnostic's trials already measured.
   - **`hints`**: `rare_bright_path`, `single_path_dominates`, `clamp_hides_path` (rerun
     this step with `--indirect-clamp 0` to see what the path really carries),
     `labels_ambiguous` (the lobe names are uncertain; say so, and suggest per-lobe AOVs).
   - **`consistency.max_relative_error`** must be ~1e-7. If it is not, stop: the paths do
     not add up to the pixels, which is a bug to report, not a noise source.

   Read the top expression as a path, camera first: `C<RD'diffuse'><RS'specular'>L` is
   camera → diffuse bounce → mirror → light, i.e. a caustic. `<T.>` is transmission,
   `<V.>` a volume, `O` an emitter that is not a light.

6. **Recommend.** Map what was measured to an action, and give each its evidence (the
   ΔEff, the share, the hint):

   | measured | recommend |
   |----------|-----------|
   | a diagnostic suggestion | its flag / `crust:*` attribute, with its ΔEff; then verify it (step 7) |
   | `adaptive_sampling.*` suggestions | their `-s`, `--variance-threshold`, `--min-samples`, `--neighbour-tolerance` values together (one setting), with `delta_eff_over_authored`; `unguided: true` means the sweep did not measure guiding with them |
   | `target_reached: false` | no adaptive setting up to `spp_recorded` reaches the target: more samples (`sample_budget.spp_to_target`) or a looser `--target-mrse`, the user's call |
   | direct rows dominate, many lights, no suggestion | `--light-selection` / `--light-samples` are already measured: report their verdicts, not a guess |
   | a rare path through a specular or refractive vertex ending on a light (caustic), with `light_sampling_misses` | no setting makes it reachable by light sampling. Options, all the user's call: more samples (`sample_budget`), the `--indirect-clamp` trade (state the energy `clamp_removed_share` / `luma_share_clamped` say it removes), or a scene change (roughen the specular surface, light-link the light) |
   | `O` paths / large `unlit_emitters` | emissive geometry no shadow ray can find: apply `UsdLuxMeshLightAPI` to the emitting mesh the finding names (`prepend apiSchemas = ["MeshLightAPI"]`; with its light inputs unauthored the picture is unchanged and NEE samples it), or author a UsdLux light in its place. A prim listed with a `light.mesh_light_*` code was refused: a textured or coated glow needs `light:materialSyncMode = "independent"` and the light's inputs; a cutout, instanced or moving mesh cannot be a mesh light — use a UsdLux light |
   | `clamp_hides_path` | the clamp is hiding the firefly; the picture without it is the real one. Say how much light it removes |
   | path guiding trial `better` on indirect rows | `crust:pathGuiding = true` (no flag; author it on the stage) |

   Then offer the inspected expression as an `lpe` RenderVar so the user can see that
   path over the whole frame:

   ```usda
   def RenderVar "suspect"
   {
       uniform token dataType = "color3f"
       uniform string sourceName = "<expression from the report>"
       uniform token sourceType = "lpe"
   }
   ```

7. **Verify a change before calling it done.** For a setting change:

   ```bash
   crust diagnostic -i <stage> <scene flags> <new flag> --budget <same> \
       --baseline $OUT/d1.json --json $OUT/d2.json > $OUT/d2.md
   ```

   and read `deltas`: the baseline MRSE before and after (mind the spp each ran at), and
   which findings and suggestions went away. Its time delta compares two runs minutes
   apart: indicative only; the interleaved ΔEff is the evidence. Repeat until `converged`
   is `true` or the user stops. For a fix to fireflies, rerun step 5 on the same window
   and compare the top group's `second_moment_share`.

## Rules

- **Never change the stage or a setting yourself.** Report and recommend; the clamp, the
  sample count and any scene change trade the picture for the noise and are the user's.
- **Don't judge noise by eye**, and don't compare wall-clock times of runs made at
  different moments. Use ΔEff, MRSE, the shares.
- **Don't recommend `--strategy light` or `--strategy bsdf`.** They are visualisations: a
  light-only render looks cleaner because it is darker (ALab: 61% darker).
- **Unbiased comparisons need `--indirect-clamp 0`** on both sides; the diagnostic already
  runs that way, `crust render` and `crust light-paths` do not by default.
- **Every crop and window is in image pixels, top-left origin, `X1`/`Y1` excluded.**
- When something reads as a bug (a `biased` tier-1 trial, `consistency` off by more than
  rounding, a NaN in the breakdown), say so plainly and point at the numbers.

## Output

A short report to the user, in this order:

1. **Verdict**: one or two sentences: what carries the noise, and the one change to make
   (or "no setting fixes this; here is the trade").
2. **Evidence**: the dominant `noise_breakdown` row with its error, the picture findings
   that fired, the top light-path group (expression, light, hit rate, second-moment and
   luma shares) and the hints.
3. **Recommendations**, ranked, each with its flag or `crust:*` attribute and its number.
4. **Files**: the `$OUT` reports, so the user can open `d1.md` and `lp1.md` themselves.

The MCP server (`crust mcp`) exposes the same inspection as its `light_paths` tool and a
pixel `probe`: in a Claude Desktop session, use those instead of steps 4–5's commands.
