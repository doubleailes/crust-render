## Context

See proposal.md — Why/What Changes for the motivating gap. Relevant current
code (from investigation, not restated in full):

- `usd_import.rs::prim_ray_mask` reads `crust:rayMask` as an `int` and falls
  back to `MASK_ALL`. Called for meshes, spheres, curves and prototype
  parts; **never** for light prims.
- `usd_import.rs::emit_sphere_light` / `emit_rect_light` call
  `WorldBuilder::attach`, which is `attach_masked(.., MASK_ALL)`. Both
  functions take the `SphereLight`/`RectLight` schema object and the world
  matrix, not the `Prim` — but the dispatch site that calls them already has
  `prim` in scope.
- `crust_rt::ray` defines `MASK_CAMERA = 1<<0`, `MASK_SHADOW = 1<<1`,
  `MASK_INDIRECT = 1<<2`, `MASK_ALL = u32::MAX`. An intersection counts when
  `geometry_mask & ray_mask != 0`.
- Ray categories are stamped in exactly four places: `camera.rs` (camera),
  `tracer.rs` NEE surface + volume shadow rays (shadow), and `tracer.rs`
  BSDF-bounce / volume-scatter / medium-scatter continuations (indirect).
- `AreaLight` stores its geometry's `geom_id`; `LightList::find_by_geom` has
  exactly one consumer, `bounce_emission_weight`, which is the bounce half
  of MIS. `AreaLight` does not implement `Light::escaped` (that is the
  infinite-light half).
- The direct-camera-view emission term is the `prev == None` branch of the
  forward walk, which assigns `emit_here` at full weight with no MIS factor.
- Shadow rays are already traced to `distance - 0.001`, so a light's own
  surface has never self-occluded its NEE samples; leaving `MASK_SHADOW` set
  preserves today's behavior exactly.
- Only `samples/motionblur.usda` authors `crust:rayMask` today (`= 6` on a
  blocker card), covered by `loads_motionblur_usda`. `MASK_INDIRECT` has no
  test coverage anywhere in the workspace.

## Goals / Non-Goals

**Goals:**
- Make a light's emissive surface invisible to primary rays by default,
  matching the industry convention, while leaving its lighting contribution
  bit-for-bit unchanged.
- Reuse the existing `crust:rayMask` attribute as the opt-in rather than
  introducing a parallel visibility mechanism, and in doing so fix the fact
  that authoring it on a light is currently ignored.
- Keep the existing sample suite's rendered output unchanged, so that the
  golden-image check is a meaningful gate on this change rather than a
  wholesale re-record.

**Non-Goals:**
- No per-light enable/disable of a light's *contribution* (the doubled
  dome-light problem on the Moana island). That is a different axis —
  whether a light lights at all — and this change deliberately does not
  touch it, nor claim to.
- No new ray category. Splitting `MASK_INDIRECT` into
  diffuse/specular/transmission visibility (Arnold-style per-lobe light
  visibility) would let a light be hidden from diffuse bounces but visible
  in a mirror. That needs new bits in the kernel and new stamping sites in
  the integrator; out of scope.
- No change to `Emissive`, to `AreaLight`, or to the integrator. The whole
  behavioral change is which mask the importer passes at attach time.
- No change to `UsdGeomImageable`'s `visibility` or `purpose`, neither of
  which the importer reads today.

## Decisions

**1. Reuse `crust:rayMask`, with a light-specific default, rather than a new
`crust:light:cameraVisible` bool.**

`crust:rayMask` already means exactly "which ray categories see this
surface", is already parsed, already documented, and already has a sample
and a test. A light's surface is a surface; the only thing special about it
is the *default*. So the change is one helper whose sole difference from
`prim_ray_mask` is the `unwrap_or`:

```rust
fn light_ray_mask(prim: &Prim) -> u32 {
    custom_i32(prim, "crust:rayMask")
        .map(|m| m as u32)
        .unwrap_or(MASK_ALL & !MASK_CAMERA)
}
```

Alternative considered: a dedicated `crust:light:cameraVisible` bool, read
via the existing `custom_bool` helper. Rejected — it would be a second
visibility mechanism that has to be reconciled with `crust:rayMask` (which
wins if both are authored?), and it can only express the camera axis, so
"hide this light from shadow rays too" would still need the mask. One
mechanism, one precedence rule, one thing to document.

**2. `MASK_ALL & !MASK_CAMERA`, not `MASK_SHADOW | MASK_INDIRECT`.**

The two are identical today. They differ the moment a fourth category is
added: the literal pair silently leaves the new category *off* for every
light in every existing scene, whereas the negation keeps the "visible to
everything unless stated otherwise" meaning `MASK_ALL` already carries. The
failure mode of the literal form is a silent one — a new category that
mysteriously never sees lights — which is the kind this codebase has been
bitten by before.

**3. Pass the resolved mask into the emit functions, rather than the `Prim`.**

`emit_sphere_light`/`emit_rect_light` currently know nothing about prims or
custom attributes; they take a schema object and a matrix. Passing `mask:
u32` keeps that separation and puts both lights' visibility policy on one
line each at the dispatch site, where it is visible next to the rest of the
per-prim attribute handling. Passing `&Prim` instead would let each function
re-derive the mask, duplicating the policy.

**4. Only the camera bit is dropped, and that is a correctness constraint.**

Spelled out in proposal.md — Risk. Summarised: `MASK_INDIRECT` must stay set
or `bounce_emission_weight` loses the bounce half of MIS (energy loss that
grows with `bounce_pdf / light_pdf`, plus a sky-gradient hole where the light
was); `MASK_SHADOW` must stay set to preserve today's NEE occlusion behavior.
The camera bit is the only one whose removal no estimator depends on.

**5. The sample suite stays bit-identical, and that is the verification.**

Masks do not affect the BVH build — it partitions on bounds, and the mask is
tested at intersection time — so a sample's image changes if and only if a
camera ray actually reached a light. Authoring `crust:rayMask = 7` on the
lights that are in frame therefore makes the whole existing suite
bit-identical, turning `scripts/check_images.sh check` into a sharp
pass/fail gate. A scene needing the opt-in that does not get it shows up as a
diff, not as a subtle shift nobody notices.

Which samples frame their light was settled by that golden check, not by
arithmetic — and the arithmetic got it wrong, which is the argument for
measuring. Predicting from each light's *centre* elevation against the
vertical half-FOV said `rectlight.usda` was ~33° out of a ~14° half-frame and
therefore safe; the check showed 19% of its pixels changing. Two errors
compounded: that scene renders 64×64, not 16:9, so its vertical half-FOV is
actually ~23.6°, and more importantly a light is an *extent*, not a point —
the rect's lower edge sits ~15° off axis and is comfortably in shot even
though its centre is not.

The measured set needing the opt-in is therefore `rectlight`, `fog`, `smoke`,
`veach_mis` and `openpbr_showcase`. Everything else — including
`cornellbox_guided`, whose light is shrouded by design — is untouched by the
new default, confirming the intent rather than merely assuming it.

**6. `world::simple_scene`'s lights stay camera-visible.**

That scene is constructed in Rust with no USD prim, so there is no attribute
to author and no way for a user to opt back in. Hiding its emissive spheres
would make the no-arguments `cargo run` render strictly worse with no
recourse. Recorded as a deliberate asymmetry in a code comment so it does
not read as an oversight.

## Risks / Trade-offs

- **A scene silently loses its visible light.** Mitigated by the attribute
  being the same one already used for this purpose elsewhere, and by the
  change being called out as BREAKING. Not mitigated by a warning: warning
  on every light in every scene would be noise, since the new default is the
  intended behavior.
- **`Emissive` is two-sided, `AreaLight` NEE is effectively one-sided.** A
  rect light seen from behind used to render as a bright quad; now it does
  not appear to the camera at all. This change makes that pre-existing
  asymmetry less visible rather than fixing it; the underlying mismatch
  stays as it is.
- **Sphere-light NEE efficiency is unchanged but still imperfect** — the
  whole sphere is sampled uniformly, so far-hemisphere samples are occluded
  by the light's own near surface via `MASK_SHADOW`. Untouched here.

## Open Questions

- Should a light's surface also be hideable from indirect rays, given that
  doing so is unbiased only if the light is simultaneously removed from the
  light list (or given an `escaped`-style bounce term)? Deferred — it needs
  a per-lobe visibility design, not a mask default.
- Should `UsdGeomImageable.visibility = "invisible"` hide a light (and any
  geometry) outright? Unrelated to this change but adjacent, and currently
  unread by the importer.
