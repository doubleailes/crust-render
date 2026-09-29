# Design

## Context

A MaterialX surface is a closure tree plus the `surface` node that holds it,
and that node's `opacity` blends the whole surface with nothing. The tree
collapses per vertex (`closure/mod.rs`). The integrator (`tracer/path.rs`)
finds each segment's closest hit, shades it, runs NEE with an any-hit shadow
query, and bounces. A cutout does not fit the tree: it says the geometry
isn't there, which the integrator has to know before it shades.

## Decisions

- **D1 — Opacity rides beside the tree, not in it.** `Closures::opacity` is a
  program slot, `None` when it folds to 1. A transparent lobe inside the tree
  was the other option. It would have made a skipped hit a vertex: depth
  spent, MIS records written, the medium question asked, and the volume
  segment already clipped at a surface that isn't there.
- **D2 — Its own slice of the program.** Opacity is asked before shading, at
  every crossing, so `Program::optimize(&[slot])` produces a program with only
  what it depends on. It is JIT-compiled like the whole program, and its
  value is bit-identical to the full program's. With `CRUST_MTLX_OPT=0` it is
  the whole program, unoptimised.
- **D3 — `alpha_mode` selects at compile time.** It is a uniform, and a select
  through a texture-driven `alpha` never folds. The graph's `ifequal` chain
  would have made every OPAQUE glTF material with a textured alpha a cutout:
  correct, and expensive. The chain is built only for a mode that doesn't
  fold.
- **D4 — Bounce side stochastic, shadow side a product.** `pass_cutouts` meets
  a hit with probability `opacity` (a `pcg::Rng` off `K_CUTOUT`) and otherwise
  asks for the next hit along the same line. It restarts the ray
  (`restarted`) rather than raising `t_min`, which would break the
  `(0.001, ∞)` constant propagation into the kernel, and adds the restart's
  offset back onto the next hit's `t`. The restart is short of the hit by
  that 0.001 (`resume_before`), so the interval begins just past it and a
  surface layered right behind a cutout is still met. `t`, the medium, the volume regions and the cone
  then all still measure from the segment's origin. `cutout_through` (under
  `cutout_shadow`) multiplies `1 − opacity` over every crossing of a shadow
  ray and stops at the first opaque hit. Both point-sample the opacity and
  follow the same 256 crossings, so they estimate one visibility, pinned by
  the strategy-agreement, footprint and crossing-limit tests.
- **D5 — Gated twice.** `World::has_cutouts` is fixed at commit. Without a
  cutout material the integrator takes exactly its old code. With one, a
  shadow ray still asks `occluded` first and walks only when it is blocked.
- **D6 — Rotation is a frame turn on the leaf.** A graph's `rotate3d` turns
  the authored tangent about the leaf's own normal input, then the BSDF
  projects it. That equals turning the projected tangent, which works for the
  host's `Tworld` too, which has no program slot. `Leaf::rotation` is a
  right-handed angle in radians. `mx_rotate_vector3` is Rodrigues' formula at
  −θ (`v × axis`), and so is Typhoon's `Rotate3d`. So `standard_surface`'s
  `rotation · 360°` is −2π·rotation, and glTF's `−rotation · 57.29578°` is
  +rotation.

## Risks / Trade-offs

- Blocked shadow rays in a scene with a cutout cost a closest-hit walk. An
  any-hit filter in `crust-rt` is the fix, and is a kernel change.
- Opacity is asked with the shadow ray's direction on one side and the path's
  on the other. A view-dependent opacity sees opposite views.
- The learned light cache's training shadow rays see cutouts as NEE does;
  its training paths still stop at a cutout as if it were present. It is a
  guide only, so that costs variance, not bias.
- Opacity is point-sampled, so a distant cutout reads its alpha map's finest
  level: more texture traffic, in exchange for NEE and the bounce side
  agreeing.
