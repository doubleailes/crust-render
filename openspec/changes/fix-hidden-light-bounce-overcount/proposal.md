## Why

On the branch implementing `camera-invisible-lights-do-not-occlude`, a BSDF-sampled ray
that crosses a small hidden sphere light collects its emission twice about a third of the
time. On a diffuse plane under one hidden sphere light (4096 spp), BSDF-only renders
1.04×, 1.34× and 1.64× brighter than light-only at a distance/radius ratio of 40, 160 and
200, and agree at ratio 2. On `veach_mis` with LightTiny alone, the smoothest plate comes
out 16.6% brighter than before the change. Light-only is unchanged.

The excess is an exact 2.0000× on the affected pixels (425 of 1155 lit pixels in the
repro) and bit-identical elsewhere. Instrumenting the crossing walk shows each affected
segment recording the **same entry point twice** (t = 8.215210, then 8.215306, both
front-facing and emitting), then the exit. The cause is in two layers:

1. **The analytic sphere test loses precision far from a small sphere.**
   `SpherePrim::hit` computes the discriminant as `half_b² − a·c`. Seen from 8 units
   away, both terms are about 67, while their difference is of order r² (0.0025). The f32
   cancellation leaves about 1e-4 of error on the reported hit distance.
2. **The pass-through restart relies on precision the kernel does not have.**
   `hit_past` restarts the segment at `resume_before(t) = t − 0.001 + 1e-5·t` and relies
   on the tracer's `t_min` to exclude the surface it just passed. That margin is 8e-5 at
   t ≈ 8. When the first hit is reported more than that short of the true surface, the
   restarted ray finds the entry again.

The same restart serves cutouts (`pass_cutouts`, `cutout_through`) and thin walls
(`pass_walls`). A duplicate there would square a cutout's pass probability or a wall's
transmittance.

## What Changes

- **Accurate analytic sphere hits.** `SpherePrim::hit` computes the discriminant from the
  closest approach (Haines et al., *Ray Tracing Gems* ch. 7):
  `disc = r² − |oc − (oc·d̂) d̂|²`. The near root is taken in the stable form. The hit
  distance becomes accurate to about 1e-7·t instead of about 1e-4 absolute.
- **Each surface is crossed once.** Every pass-through walk (hidden light sources,
  cutouts, thin walls, on the bounce and the shadow side) drops a hit on the same
  geometry, from the same side, within a relative distance of the crossing it has just
  recorded. That is a numerical re-hit, not a surface: no closed or single-sided surface
  can be entered twice in a row from the same side. This guards every primitive, not
  only spheres.
- **Images:** the over-count disappears. Scenes with analytic spheres (sphere lights,
  `Sphere` prims) change at the ulp level wherever a sphere is hit, and by noise alone
  otherwise. Scenes without spheres and without pass-throughs stay bit-identical.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `intersection-kernel`: a new requirement, "Analytic spheres report accurate hit
  distances".
- `rendering`: a new requirement, "A pass-through crosses each surface once".

This change fixes the implementation of `camera-invisible-lights-do-not-occlude` and
should land on its branch before that change merges.

## Impact

- `crates/crust-rt/src/prim.rs`: `SpherePrim::hit`. The instanced unit sphere goes through
  the same function in local space.
- `crates/crust-core/src/tracer/path.rs`: `pass_cutouts`, `pass_walls`, `cutout_through`
  and the shadow-side thin-wall walk gain the one-crossing guard.
- Tests: a kernel precision test, a re-hit test for the pass-through walk, and the
  strategy-agreement repro as an integration test.
- Performance: the closest-approach form costs a few more flops per sphere test. The
  guard is one comparison per crossing, and the walk runs only in worlds with
  pass-throughs.
