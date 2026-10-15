## Why

A shaped light (`ShapingAPI` cone) is sampled without regard to its cone. The cone
only enters through `Emissive::radiance_toward`, after the light has been picked and a
point drawn on it. NEE samples the cone cuts off cost no shadow ray, but they still
return zero, so the variance they would have reduced stays. Disk lights are also
sampled by area by default, because the spherical-ellipse strategy loses at equal time
on glossy receivers and small disks. The waste comes in two forms:

- **(A) Selection.** A small spot (a sphere or an IES fixture) gives every point on
  its surface the same answer about a receiver outside its beam. Power selection
  cannot see the cone, so on a stage of disjoint pools most picks go to lights that
  return zero.
- **(B) Placement on the light.** A wide disk or rect with a narrow cone emits toward
  a receiver from only part of its surface. That part is the light's intersection
  with a circle of radius `h · tan θc` centred on the receiver's foot point on the
  light's plane.

No checked-in scene shows (B). `usdlux.usda`'s spot is a 0.5 × 0.5 rect lighting a
pool of radius 1.6, which is case (A). The size of either effect has never been
measured.

## What Changes

- **Measurement scenes, checked in.**
  - `samples/spotlights.usda`, a stage of six small spots (sphere and IES) on disjoint
    pools under a dim fill, plus one wide disk spot close to a subject.
  - A generator for a wide-source sweep: rect and disk, light radius relative to
    `h · tan θc`, cone angle, softness and focus, diffuse and glossy floor, plus the
    archived unshaped-disk rows.
- **A baseline** at 16 spp against 1024 spp references with `--indirect-clamp 0`:
  power vs `learned` selection on the stage, and `area` vs `ellipse` disks on the
  sweep. It is recorded in the change's `notes.md`.
- **Gated on that baseline:**
  - **Case (A):** record whether `learned` selection removes the waste. Any follow-up
    belongs to `learned-light-selection-by-default` or to a light tree. This change
    builds no selection code.
  - **Case (B), rect:** a new switch `CRUST_CONE_CLIP` (`off` | `on`). With `on`, a
    rect light whose shaping axis is its normal is sampled by the spherical rectangle
    of its intersection with the bounding square of the cutoff circle.
  - **Case (B), disk:** a new value `CRUST_DISK_SAMPLING = polar`. It samples a disk
    in polar coordinates about the receiver's foot point, follows `cos θl / r²` (and
    the focus falloff) along each slice in closed form, and is capped at the cutoff
    radius when `CRUST_CONE_CLIP = on`.
  - The default of each switch changes only if the design's equal-time gate passes.
    Until then the defaults are `off` and `area`, and every render is bit-identical
    to today.
- The `lighting` design record and spec, `docs/architecture.md` § Environment
  switches, and the user documentation (`reference/environment-variables.md`, the
  lighting limitations page) describe the new switch and value, and the gate result.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `lighting`: two added requirements, "Rect lights can be sampled within their cone"
  and "Polar disk sampling", each behind its switch. "Area lights" keeps its text: its
  "every value of `CRUST_DISK_SAMPLING`" unbiasedness scenario now also covers
  `polar`. "Known gaps" narrows "cone-aware sampling of
  shaped lights" to what this change leaves: selection, sphere and cylinder lights,
  sheared or elliptical placements, and the IES distribution.

## Impact

- `crates/crust-core/src/light/rect.rs`: the clipped sub-rectangle feeds the existing
  spherical-rectangle map.
- `crates/crust-core/src/light/shape.rs`: a polar disk strategy beside the ellipse in
  `AffineShape`.
- `crates/crust-core/src/light/area.rs`: `AreaLight::new` tells the shape the shaping
  cone (axis, cutoff), as it tells it one-sidedness today.
- `crates/crust-core/src/config.rs`: `CRUST_CONE_CLIP` and the `polar` value.
- `samples/spotlights.usda` and `scripts/make_spot_sweep.py` (new). The integration
  tests load every sample, so the stage has to load cleanly.
- Performance: with both switches off, the render runs the same instructions as today
  except for one branch at light construction. With them on, the cost per sample is
  part of the equal-time gate.
- Interacts with `learned-light-selection-by-default`. The stage baseline is evidence
  for that change, and neither change edits the other's requirement.
