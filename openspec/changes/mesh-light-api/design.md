# Design

## Context

See `proposal.md` for the motivation and the decisions taken with the user (opt-in
`MeshLightAPI` only; one radiance for every ray; flat emission first, the production
card being a flat-coloured MaterialX surface). The state of the code that shapes the
approach:

- **A light's radiance is a constant.** `AreaLight` pairs an `AreaShape` (a closed enum:
  `Sphere`, `Affine`, `Rect`) with an `Arc<Emissive>`, and both MIS halves ask
  `Emissive::radiance_toward`. A mesh's emission comes from `Material::emitted_at`,
  which may be textured. The `materials` design record's rule: a material that emits
  only through `emitted_at` never becomes a light-list entry.
- **`Material::emitted()` is hit-free but not direction-free.** OpenPBR's emission under
  a coat or fuzz fades with the viewing angle (`emitted_directional`); `MtlxMaterial`
  reports zero from `emitted()` and answers only through `emitted_at`.
- **The bounce side knows the point, not the triangle.** `bounce_emission_weight_at`
  (`tracer/path.rs`) finds the light by `geom_id` (`LightList::by_geom`, one geometry to
  one light) and asks `Light::pdf_at_point(from, point)`, which takes the light normal
  from `LightShape::normal_at(point)`. A mesh has no normal at a bare point; the hit
  has its `prim_id`.
- **Mesh placement is deferred.** `flush_meshes` (`usd_import/mesh.rs`), run once after
  traversal and before the world commit, decides per placement: baked into world space
  (one static placement), or a kernel instance of a local-space scene (several
  placements, or motion). Subdivision and displacement are already applied to the
  triangles it holds. Prototype placements (native instances, `PointInstancer`) report
  a forwarded hit id that does not name the placement (`rt_world.rs`, `VertexSource`).
- **The transparent-emitter path is analytic.** A camera-hidden UsdLux light lets rays
  through and collects its emission via `LightShape::hits` (`AreaLight::found_along`).
- Typhoon (hdEmbree, OpenUSD `typhoon/main` 70c45e8) does not read `MeshLightAPI`; its
  `lightSamplerCommon.h` only records the intent of "a shared emitter sampler … with
  consistent sample/evaluate PDFs". There is no reference implementation to follow.

## Goals / Non-Goals

**Goals:**
- A `MeshLightAPI` mesh with a uniform emission is NEE-sampled and MIS-weighted under
  every sampling strategy and light selection (`power`, `uniform`, `tree`, `learned`,
  `neural`), with the "one sampler" invariant kept structural.
- Applying the API with default inputs is a pure variance reduction: the image agrees
  with the API-less render within noise.
- Zero cost, bit for bit, for stages without the API.

**Non-Goals:**
- Textured or directional emission as a light (needs the material evaluated at the NEE
  sample: a synthetic hit with interpolated primvars).
- Solid-angle sampling of triangles (Arvo 1995), triangles as light-tree leaves, a
  per-triangle power table.
- `VolumeLightAPI`, `GeometryLight`, emissive curves and instanced mesh lights.
- Motion-blurred lights.

## Decisions

### 1. A mesh shape in `AreaShape`, one entry per mesh

`AreaShape::Mesh(MeshShape)`: the world-space triangles of one placement and a CDF over
their areas. `sample_point(u, v)` picks a triangle by `u` (rescaled after the search, so
one pair of numbers drives both choices) and a uniform point on it by `v` and the
remainder. With a uniform radiance, picking by area is picking by power, and the area
density is `1 / A_total` everywhere on the mesh, so `inv_pdf_area` is the default
`area()`. `solid_angle_sampler` returns `None` (area sampling): the existing
`AreaLight` code then handles NEE, the pdf, refusal at infinite density and `power()`
unchanged. The mesh's `kind()` is `"mesh"` for `--stats`.

*Alternatives.* One light per triangle (pbrt-v4): breaks `by_geom`'s one-to-one map,
multiplies the light list (and the `power` table, the tree, `learned`'s grid) by the
triangle count, and dilutes the defensive shares. A new `Light` implementation beside
`AreaLight`: duplicates `sample_li` / `pdf_at_point` / bounds logic that the enum
reuses for free.

### 2. The triangle reaches the pdf through the hit's `prim_id`

`Light::pdf_at_point` gains the hit's primitive (`prim: u32`), which
`bounce_emission_weight_at` already holds in its `WorldHit`, and `LightShape::normal_at`
the same argument. Analytic shapes ignore it. `MeshShape::normal_at` recomputes the
geometric normal of triangle `prim` from its own vertex table, with the same arithmetic
NEE used for the sampled triangle, so both sides compute one cosine. Under area
sampling the density depends on nothing else about the triangle.

*Alternatives.* Pass the hit's geometric normal: the kernel's normal and the light's
are computed by different code and need not agree to the ulp, and a future per-triangle
power table would need the index anyway. Look the triangle up from the point: a
spatial query per bounce hit, and ambiguous on shared edges.

The `prim_id` of a direct instance's hit is the local triangle index, which is the
index into the light's table, since the light is built per placement from the same
triangle order.

### 3. "Uniform emission" is a material query, stricter than `emitted()`

`Material::uniform_emission() -> Option<Vec3A>`: `Some(c)` only when `emitted_at`
returns exactly `c` at every hit and every angle. Default `None`.
- `OpenPBR`: `Some(emission_color · emission_luminance)` when `coat_weight <= 0` and
  `fuzz_weight <= 0`.
- Pattern materials (`UsdPreviewSurface`, `crust:openpbr` with patterns): through their
  `constant_emission`, under the same coat/fuzz condition.
- `MtlxMaterial`: the compiled program's constant emission (decision 4).
- `Emissive`: `Some(color)` when unshaped, untextured and two-sided.
A bitwise test runs every material that answers `Some` through `emitted_at` at random
hits and angles. `emitted()` keeps its meaning; the light list's existing reads are
unchanged.

*Alternative.* Reuse `emitted()`: wrong for a coated OpenPBR, whose `emitted()` is the
uncoated value while `emitted_at` fades it.

### 4. The MaterialX constant fold happens at compile time, in `crust-mtlx`

After the closure tree is built, each emission term's `weight` and `color` slots are
folded with `Compiler::fold`, and its `generalized_schlick_edf` falloff is accepted
only when absent or when `color0` and `color90` fold to equal values. If every term
folds, the program records `constant_emission: Option<[f32; 3]>`, computed with the
same per-channel sanitising `MtlxMaterial::emission` applies (non-finite or negative
channels contribute nothing). `fold` evaluates with the interpreter's own `apply`, and
the JIT is pinned bitwise to the interpreter, so the constant equals `emitted_at`
bitwise. `crust-mtlx` stays free of crust types: it reports plain `[f32; 3]`.

### 5. A wrapper material carries the light's radiance to every ray

The mesh's geometry is bound to a `MeshLightMaterial { inner, emission: Arc<Emissive> }`
that forwards everything to `inner` except emission: `emitted`, `emitted_directional`,
`emitted_at` and the `emitted` field of the `Resolution` its `resolve` returns all
answer the light's `Emissive`, which is the same `Arc` the `AreaLight` holds. So camera,
bounce and NEE read one radiance (the "one radiance for every ray" decision), through
`Emissive::radiance_toward`, the function the pair rule names. Under
`noMaterialResponse` the geometry is bound to the light's `Emissive` itself, as a
UsdLux light's geometry is, and has no surface response.

The radiance is computed at import by `materialSyncMode`:

```
L        = lux_params(...)            -- color · 2^exposure · intensity [· blackbody]
glow     = inner.uniform_emission()   -- only read under materialGlowTintsLight
radiance = glow · L                   (materialGlowTintsLight; None -> refuse)
         | L                          (independent, noMaterialResponse)
radiance /= world area                (normalize)
```

It is two-sided (`Emissive::new`, not `Emissive::light`), as material emission is.

*Alternative.* Scale the material's own emission in place: needs a per-material-type
rewrite of its emission parameters, and the bound material may be shared with prims
that are not lights.

The wrapper costs one extra virtual call per hit on mesh-light geometry only. Nothing
else is wrapped.

### 6. Built at `flush_meshes`, from the triangles the kernel holds

The traversal records a `MeshLightAPI` prim's inputs (`light_inputs`, `lux_params`,
`materialSyncMode`, linking collections) and binds the wrapper material when it places
the mesh. At flush time, once each placement's representation is decided, a mesh-light
placement copies its world-space triangles: the baked vertices for a baked placement,
or the local triangles through its static transform for a direct instance. It then adds
the `AreaLight` with that placement's `geom_id`. Refused placements, raised there on the
importing thread with their `WarningCode`, are those with motion (`l2w_end`) and those
under a prototype. Lights are added before the after-traversal light-link resolution.
`crust ls light` (`usd_import/listing.rs`) lists `MeshLightAPI` meshes with kind
`mesh`.

*Memory.* The copy is 36 bytes per triangle plus 4 for the CDF, beside the kernel's own
vertex table. That is negligible for cards, and recorded as a known gap for very dense
mesh lights. Reading the kernel's table instead would make the light module depend on
`World`.

### 7. Visibility: solid geometry with the light's camera rules

A mesh light's geometry keeps the mesh's own ray mask (visible to every ray), and
`crust:rayMask`, `crust:light:cameraVisible` and the RenderMan primvar override it as
they do for other lights. It is never registered as a transparent emitter: that path
intersects the analytic shape (`LightShape::hits`), which a mesh does not have. A hidden
mesh light therefore still occludes, like a masked UsdLux light ("Visible and masked
light sources stay solid").

### 8. Light tree bounds

`MeshShape::extent()` is the triangles' world box. A two-sided emitter gives the tree's
`θo = π`, `θe = π/2`, and its flux is `Emissive::peak_flux(area)`, already doubled for
two sides. A flat card is bounded no tighter than a sphere would be; per-triangle
leaves are the follow-up that tightens it (non-goal).

### 9. Warning codes

New, all `skipped`, each with a row in `site/content/docs/reference/warnings.md`:
`light.mesh_light_varying_emission` (glow not uniform under `materialGlowTintsLight`),
`light.mesh_light_cutout`, `light.mesh_light_instanced`, `light.mesh_light_moving`,
`light.mesh_light_not_a_mesh` (API on a non-`Mesh` prim), `light.mesh_light_no_emission`
(zero radiance).

### 10. Diagnostic `unlit_emitters`

The finding keeps its trigger (the measured `C.*O` luminance) and its `Action::None`,
whose reason now names `MeshLightAPI` and up to five prims. The prims are read from
the scene: emissive geometry (`may_emit`) with no light-list entry, by prim path, plus
the prims on `light.mesh_light_*` warning records with their code. Evidence gains a
numeric `prims` count; the `crust-diagnostic/1` format is unchanged.

### 11. Live edits (MCP)

Applying or removing `MeshLightAPI`, or editing a mesh light's inputs, changes the
light list. Such edits go through the full re-import the session already uses for light
changes, never in place, so the "edit in place ↔ re-import" bit-identity pair is not
widened.

## Risks / Trade-offs

- [Area sampling on large, close mesh lights is noisier than solid-angle sampling] →
  Measure on a quad card against an equal `RectLight` (same radiance, same area);
  record the gap in `docs/light_sampling.md`; Arvo triangle sampling is the follow-up.
- [A one-entry mesh under `tree` is ranked by a loose box] → Same as a sphere light
  today; per-triangle leaves are recorded as a known gap.
- [The `pdf_at_point` signature change touches every light and the inspection and LPE
  code that call it] → The analytic shapes ignore `prim`; the zero-mesh-light render is
  pinned bit-identical and its instruction count checked with callgrind
  (`cornellbox -s 2`) before and after.
- [The schema's `materialSyncMode` reading differs from RenderMan's] → Documented as a
  design choice in `site/content/docs/usd/lights.md` and the lighting design record,
  with the reason (an image that depends on MIS weights).
- [A shared material bound to a light mesh and to ordinary meshes] → The wrapper is per
  light placement; other prims keep the unwrapped material.
- [Memory for dense mesh lights] → Decision 6; recorded as a known gap.

## Migration Plan

Additive. Stages without `MeshLightAPI` render bit-identically. A stage that already
authors `MeshLightAPI` (exported from Houdini or Maya for RenderMan or Karma) changes
from BSDF-only to NEE-sampled emission: same expectation, less noise, unless it authors
light inputs or a non-default sync mode, which now take effect. Rollback is reverting
the change; there is no switch, because the "off" behaviour is "remove the API".
