## Context

See proposal.md - Why/What Changes for the motivating gap. Relevant current
code (from investigation, not restated in full):

- `usd_import.rs::preview_surface_openpbr` reads `diffuseColor`/`emissiveColor`
  raw (no conversion) into `OpenPBR::base_color`/`emission_color`.
- `usd_import.rs::disney_to_openpbr` converts `baseColor` via a private
  `srgb_to_linear` that is actually a flat `powf(2.2)`, not the piecewise sRGB
  EOTF; every other `PxrDisneyBsdf` input is a scalar, read raw, correctly.
- `usd_import.rs::decode_crust_openpbr` reads every `crust:openpbr` color
  field raw via `shader_input_vec3`, by design (native format is
  linear-authored) but with nothing marking that as a decision.
- `crust-render/src/main.rs::PtexColor::open` decodes every Ptex texel with
  its own inline `powf(2.2)`, independent of `usd_import.rs`'s function.
- `main.rs::load_image_environment` already picks correctly per format
  (piecewise sRGB for LDR, linear pass-through for `.hdr`/EXR) and is not
  broken; it is a candidate to consume the same shared primitive, not a
  target of the bug fix.
- `OpenPBR` (`crust-core/src/material/openpbr.rs`) stores every field as
  plain `Vec3A` (colors) or `f32` (scalars) — the `Vec3A`/`f32` type
  difference already prevents a scalar from being decoded as a color (decode
  functions take `Vec3A`), so the actual, real gap is the opposite direction:
  nothing forces a color-valued `Vec3A` read off a USD attribute to pass
  through a decode step at all before landing in an `OpenPBR` field — which
  is exactly how the `UsdPreviewSurface` bug happened.
- The same "read raw, no decision made" gap exists in two more places, both
  outside `OpenPBR`/materials entirely: `usd_import.rs::lux_emission` reads
  `inputs:color` for `UsdLuxDistantLight`/`DomeLight`/`SphereLight`/`RectLight`
  via `attr_color3f`, and the volume-region importer (~usd_import.rs:482-484)
  reads `crust:volume:sigmaS`/`sigmaA`/`emission` via `custom_color3`. Neither
  helper does any conversion today, and — unlike `UsdPreviewSurface` — neither
  should: OpenUSD's own docs for `UsdLuxLightAPI:inputs:color` state it is "in
  the rendering color space," which for a linear-light-transport renderer
  *is* linear; `crust:volume:*` are project-custom attributes (not a standard
  USD schema) representing physical scattering/absorption/emission
  coefficients, authored the same linear-native way as `crust:openpbr`. So
  these two are not a numeric bug like `UsdPreviewSurface` — they are the same
  *enforcement* bug (nothing marks the "no conversion" choice as intentional).
- Worth being precise about `UsdPreviewSurface`: its own schema spec does
  **not** mandate a color space for `diffuseColor`/`emissiveColor` (the spec's
  only explicit color-space note is for normal maps). Decoding it as sRGB is
  this change's design choice — matching the common convention that such
  values are authored as DCC color-picker swatches — not something USD
  requires. Recorded here so the requirement text doesn't overstate it as a
  schema mandate.

## Goals / Non-Goals

**Goals:**
- One shared place (usable from both `crust-core` and `crust-render`) that
  defines the two color-space conversions actually in use (piecewise sRGB
  EOTF, flat gamma 2.2) plus the identity case, replacing the two
  independent inline implementations.
- Make it structurally impossible to add a new color-valued shader input to
  `usd_import.rs` without a call site explicitly stating its source color
  space — including explicitly stating "linear, no conversion" rather than
  achieving that by silently not calling anything.
- Fix the `UsdPreviewSurface` gap as part of the same mechanism, not as a
  one-off patch, so the next shader family gets the enforcement for free.
- Apply the same mechanism to every color-valued USD attribute read by the
  importer, not only material shader inputs — light colors and volume
  coefficients get an explicit `ColorSpace::Linear` call site each, even
  though (per Context) the correct value doesn't change.

**Non-Goals:**
- No change to `OpenPBR`'s own field types (`Vec3A`/`f32`) or its trait
  contract — this is an import-time/load-time concern, not a materials
  (`crust-core/src/material/`) capability change. Scalars are not wrapped:
  the existing `Vec3A` vs `f32` type distinction already prevents the
  scalar-decoded-as-color direction of the bug; only the
  color-not-decoded-at-all direction needs new enforcement.
- No generic/phantom-typed `Color<Space>` machinery threaded through the
  renderer's hot path. Decode happens once at import or texture-load time,
  never per-sample, so there is no case for a zero-cost-abstraction generic
  here — a plain enum plus a newtype confined to the importer module is
  enough and keeps the change's blast radius small.
- No attempt to unify the piecewise sRGB EOTF and the flat gamma-2.2 curve
  into one "the" sRGB conversion. They are deliberately different curves for
  deliberately different reasons (see Decisions) and this change documents
  that distinction rather than erasing it.
- No per-attribute `colorSpace` override authoring in USD (e.g. a
  hypothetical `crust:openpbr:baseColor:colorSpace` token) — see Open
  Questions.

## Decisions

**1. A small `ColorSpace` enum + free conversion functions, not a generic
`Color<Space>` wrapper type.**

`ColorSpace { Linear, Srgb, Gamma(f32) }` with `ColorSpace::decode(self, Vec3A)
-> Vec3A`, living in a new `crust-core/src/color.rs`, re-exported from
`lib.rs` so `crust-render`'s `main.rs` can use the same `Gamma(2.2)` arm for
Ptex that `usd_import.rs` uses for `PxrDisneyBsdf`.

Alternative considered: a generic `Color<S: ColorSpaceMarker>(Vec3A)` newtype
threaded all the way into `OpenPBR`'s fields, giving compile-time-verified
"this field is always linear" guarantees. Rejected: it would touch the
`materials` capability's data representation (a boundary this change
explicitly leaves alone per Non-Goals), ripple into every BRDF read site in
`material/brdf.rs`, and buys nothing beyond what's needed — conversion is a
one-time, load-time operation, so there is no runtime type-safety benefit
happening on the hot path, only at the ~30 importer call sites where the
enum-plus-function approach already suffices.

**2. A `RawColor3` newtype at every USD-color-attribute-reading boundary,
decoded explicitly at each call site — but no equivalent wrapper for
scalars.**

`RawColor3(Vec3A)` wraps the return of all three of the importer's
color-reading primitives, not just the material one:
`shader_input_vec3` (material shader inputs), `attr_color3f` (`UsdLuxLightAPI
inputs:color`, via `lux_emission`), and `custom_color3` (`crust:volume:*`
coefficients). The only way to obtain a `Vec3A` from a `RawColor3` is
`RawColor3::decode(self, space: ColorSpace) -> Vec3A`. Every color-field call
site — the ~27 across `preview_surface_openpbr`/`disney_to_openpbr`/
`decode_crust_openpbr`, plus `lux_emission` and the volume-attribute reads —
must therefore name a `ColorSpace` explicitly, including `ColorSpace::Linear`
for `crust:openpbr` fields, light colors, and volume coefficients — turning
today's silent "no conversion by omission" into an explicit, greppable
decision everywhere a color-valued USD attribute is read, not only in
materials.

`shader_input_float` (scalars) is **not** wrapped, and neither is any other
scalar reader (light `intensity`/`exposure`, volume `anisotropy`, etc.). As
noted in Context, the existing `Vec3A`/`f32` type split already prevents a
scalar from being routed through a color decode by accident — there is no bug
in that direction to guard against, so wrapping floats would be pure ceremony
for zero enforcement benefit.

**3. Preserve the two existing non-piecewise-sRGB decisions as explicit,
documented `ColorSpace` choices rather than unifying them.**

`PxrDisneyBsdf.baseColor` and Ptex color textures both keep flat
`ColorSpace::Gamma(2.2)`, not `ColorSpace::Srgb`, because (per `CLAUDE.md`)
this specifically matches the Moana island's `PxrColorCorrect` gamma-1/2.2
node and `HwPtexTexture_1`'s `sourceColorSpace = "sRGB"` authoring —
switching either to the piecewise EOTF would be a small but real numerical
change to every already-validated island render. `UsdPreviewSurface` gets
`ColorSpace::Srgb` (the piecewise EOTF), matching the conventional DCC
authoring assumption for that portable schema and `main.rs`'s existing LDR
image-decode path, which already uses the piecewise curve for the same
reason. That assumption is this change's own convention, not a schema
requirement (see Context) — recorded in the doc comment on the call site so
it isn't mistaken for one.

**4. Light colors and volume coefficients decode as `ColorSpace::Linear`
(identity), on the strength of upstream documentation, not local
convention.**

Unlike `UsdPreviewSurface` (Decision 3, a judgment call this project makes),
`UsdLuxLightAPI:inputs:color` is explicitly documented upstream as being "in
the rendering color space" — for a linear-light-transport path tracer, that
is linear by definition, so `ColorSpace::Linear` is not a guess. `crust:volume:*`
coefficients are this project's own attributes (no upstream schema to defer
to), and are treated as linear for the same reason `crust:openpbr` is: they
are physical quantities (scattering/absorption coefficients, emission
radiance) authored directly, not painted swatches. Both get an explicit
`RawColor3::decode(.., ColorSpace::Linear)` call site rather than silent
passthrough, per Decision 2 — the value is unchanged, only the decision
becomes visible and enforced.

## Risks / Trade-offs

- **[Risk]** The `UsdPreviewSurface` fix changes rendered output for any
  scene using it (proposal's **BREAKING** note) → **Mitigation**: task
  breakdown includes regenerating golden images
  (`scripts/check_images.sh record`) only for the specific sample scenes
  that use `UsdPreviewSurface`, after visually confirming the new (decoded)
  output is the intended one — not a blanket re-record.
- **[Risk]** A future contributor sees two different "gamma 2.2-ish" curves
  (piecewise sRGB vs flat pow 2.2) and "simplifies" them into one →
  **Mitigation**: doc comments on both `ColorSpace::Srgb` and
  `ColorSpace::Gamma` cite the specific upstream reasoning (DCC sRGB
  convention vs. the island's `PxrColorCorrect`/`HwPtexTexture_1` authoring),
  and a unit test asserts the two curves produce numerically different
  output at a representative value (e.g. 0.5) so an accidental unification
  fails a test instead of silently changing island renders.
- **[Trade-off]** `RawColor3` adds one wrapper/unwrap step at ~27 material
  call sites plus the light and volume color reads → accepted, since that
  friction is the entire point (each site must state its color space), and
  it is confined to `usd_import.rs` with no runtime cost and no change
  outside the importer.
- **[Risk]** A reviewer sees `ColorSpace::Linear` at every light/volume call
  site and assumes it's a no-op not worth reviewing carefully, missing a
  genuine future case where an attribute should decode differently →
  **Mitigation**: each `Linear` call site cites *why* (upstream doc quote for
  lights, "project-custom, linear-authored" for volumes) rather than being
  bare, so a reviewer sees the reasoning, not just the identity conversion.
- **[Risk]** None of this affects render throughput (decode is O(materials
  loaded), not O(samples)) — flagged only to close the loop per this
  project's performance-first guiding principle; no benchmark is needed for
  this change.

## Migration Plan

1. Add `ColorSpace` enum + `decode` fn in a new `crust-core/src/color.rs`,
   re-exported from `lib.rs`.
2. Change `shader_input_vec3`, `attr_color3f`, and `custom_color3` in
   `usd_import.rs` to return `RawColor3`; add `RawColor3::decode`.
3. Update `preview_surface_openpbr`: decode `diffuseColor`/`emissiveColor`
   with `ColorSpace::Srgb` (the bug fix). Scalar fields untouched.
4. Update `disney_to_openpbr`: decode `baseColor` with `ColorSpace::Gamma(2.2)`
   via the shared function; delete the private `srgb_to_linear` it used to
   call.
5. Update `decode_crust_openpbr`: decode every color field explicitly with
   `ColorSpace::Linear` (identity — makes "intentionally no conversion"
   explicit rather than implicit).
6. Update `crust-render/src/main.rs`'s `PtexColor::open` to call
   `ColorSpace::Gamma(2.2).decode(...)` from `crust-core` instead of its own
   inline `powf(2.2)`.
7. Update `lux_emission` (feeding `emit_distant_light`/`emit_sphere_light`/
   `emit_rect_light`/the dome-light path) and the volume-region attribute
   reads to decode with `ColorSpace::Linear` explicitly.
8. Add unit tests: each shader family's chosen `ColorSpace` (regression
   pinning), that `Srgb` and `Gamma(2.2)` remain numerically distinct, and
   that light/volume colors pass through `ColorSpace::Linear` unchanged.
9. Regenerate golden images only for sample scenes using
   `UsdPreviewSurface` colors (confirm visually first, then
   `scripts/check_images.sh record`), and rerun `scripts/check_images.sh
   check` for everything else — light and volume scenes included — to
   confirm no unintended change anywhere else.

Rollback: this is a pure import/load-time behavior change with no persisted
data format or schema change — reverting the commit(s) is sufficient; no
migration of stored state is involved.

## Open Questions

- Should `crust:openpbr` ever gain an optional per-input `colorSpace`
  override (e.g. for an artist who wants to author an sRGB value directly in
  the native format) instead of always assuming linear? Deferrable: today's
  behavior (always linear, no override) is preserved by this change either
  way, and adding an override later is additive to the `ColorSpace` enum
  introduced here, not a change to it.
