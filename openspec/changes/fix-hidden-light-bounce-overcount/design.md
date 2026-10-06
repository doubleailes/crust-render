## Context

See proposal.md (Why). The walk that passes hidden light sources, cutouts and thin walls
(`pass_cutouts` / `pass_walls` on the bounce side, `cutout_through` and the thin-wall
visibility on the shadow side) restarts each segment just short of the surface it passed
(`resume_before`), then asks the world for the next hit with the tracer's `t_min`
(0.001). `resume_before` was introduced so a surface lying within 0.001 *behind* a cutout
card is not skipped (`a_surface_just_behind_a_cutout_is_not_skipped`). Its relative step
(1e-5·t) assumes the reported `t` is accurate to better than that.

Measured on the repro (radius 0.05, 8 units away): the first hit's `t` and the re-hit's
differ by 1e-4 to 2e-3, and 37% of hits are recorded twice.

## Goals / Non-Goals

**Goals**

- Every pass-through surface is crossed exactly once, whatever the primitive and
  however imprecise its hit distance.
- Sphere and cylinder hit distances accurate enough that the guard is a backstop, not
  the fix.
- Keep the cutout fix that motivated `resume_before`: a surface 0.001 behind a card is
  still found.

**Non-Goals**

- Changing `resume_before`'s margin. A larger step would re-open the skipped-surface bug.
- Precision work on disks or curves, beyond what the guard covers. Disks are a plane
  test with no cancellation; curves are not pass-throughs in practice.

## Decisions

### D1. Closest-approach sphere and cylinder tests

Write `l = oc − (oc·d) d / a`, the vector from the sphere centre to the ray's closest
approach. Then `disc = a·(r² − |l|²)`, the near root is `q = −half_b − sign(half_b)·√disc`,
and the roots are `c/q` and `q/a`. Both quantities are of the order of r², not of |oc|²,
so the f32 error scales with the sphere's size instead of its distance. This is the
standard remedy (Haines, Günther, Akenine-Möller, *Ray Tracing Gems* ch. 7, "Precision
Improvements for Ray/Sphere Intersection"). Alternative: an f64 sphere test. That is
exact enough, but it is slower in the BVH inner loop and does not help other primitives.

The cylinder wall is the same quadratic in the plane perpendicular to its axis
(`CylinderPrim::hit` projects `oc` and `d` there first), so it had the same cancellation
and takes the same form. With both fixed, every analytic primitive the renderer builds
lights on reports its distance to about 1e-6·t, and the guard below can be narrow.

### D2. One crossing per surface: a structural guard

The walk records the last crossing it accepted: `(geom_id, prim id, side, t)`. A new hit
with the same geometry and primitive, from the **same side**, within
`t_prev + ε·max(|t_prev|, 1)` (ε = 1e-4, the same shape as `resume_before`'s step), is a
numerical re-hit and is skipped: the walk restarts past it without recording it, and
without spending a crossing of the budget. A genuine second surface on the same geometry
is either the other side (an exit) or farther than ε·t (a different part of a concave
mesh). This holds for every primitive, so it also covers triangles, disks and curves,
whose precision D1 does not touch. The bounce and the shadow side share the rule, so
their visibility stays one estimate.

**Why ε is 1e-4 and not wider.** The first draft used 1e-3, wide enough to cover the
unfixed cylinder. But a hit's `(geom_id, prim id)` is not always one surface: inside a
nested prototype every placement of a leaf part reports the same id
(`InstanceHitId::As(first)` in `usd_import/instancing.rs`), so two placements of one
cutout card stacked within the window, facing the same way and hit on the same triangle
index, would read as a re-hit and the second would be skipped — a light leak in dense
instanced foliage, 2 cm wide at t = 20 under 1e-3. With spheres *and* cylinders at
1e-6·t, 1e-4 is still 100× the pinned error and 2 mm at t = 20, the thickness of a
coplanar overlap. The exposure is recorded in the rendering design record's known gaps.

### D3. Both, not either

D1 alone leaves the walk exposed to the next imprecise primitive. D2 alone would hide a
1e-4 sphere distance error that also affects where NEE and the bounce side place a hit on
the sphere. They are cheap and independent, so the change ships both, with separate tests.

## Risks / Trade-offs

- [The sphere and cylinder test changes move bits in every scene with one] → Expected
  and bounded. `scripts/check_images.sh` lists the moving samples, and for three of them
  the difference is shown to fall as 1/√N.
- [The guard could skip a real surface] → Only a hit on the same primitive, from the same
  side, within 1e-4·t. A real surface needs an exit between two entries, so this cannot
  happen for closed or single-sided geometry with its own id. For nested-instance
  placements sharing an id, see D2: the window is a coplanar overlap. A unit test with
  two coplanar-close cards on *different* primitives checks both are still crossed.
- [`Tri4` ↔ scalar bit-identity] → Unaffected: the change touches spheres and cylinders
  only.
