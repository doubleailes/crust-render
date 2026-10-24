# Proposal

## Why

Emissive geometry is found only when a BSDF-sampled ray happens to hit it, never by
next-event estimation (#343). Fire and FX cards, practical lights modelled as geometry,
signs and screens are common in production, and where they light the scene they
dominate its noise. On a production hair-backlight pass lit by one flat-coloured
MaterialX emissive card, `crust diagnostic` attributed most of the frame's noise to the
`emission` / `unlit_emitters` rows (relative error 0.031 / 0.035 against 0.015 for
direct diffuse), and the pass needed about 20,000 spp to reach 5% noise. USD already has
a way to say "this mesh is a light": `UsdLuxMeshLightAPI`. Crust does not read it, and
neither does the reference delegate (Typhoon / hdEmbree lists it as unsupported).

## What Changes

- **`MeshLightAPI` makes a mesh a light.** A `UsdGeomMesh` with the API applied becomes
  a light-list entry: its triangles are sampled by NEE and MIS-weighted on both sides,
  like every other area light. Opt-in only, as in Arnold (`mesh_light`) and RenderMan
  (`PxrMeshLight`): emissive geometry without the API keeps today's behaviour. There is
  no setting that turns every emissive mesh into a light.
- **One radiance for every ray.** The schema defines `light:materialSyncMode` by which
  rays see what: camera and secondary rays see the material's glow, "light rays" see
  the light's inputs. Taken literally, NEE and the bounce would see different radiance
  for one emitter, and the image would depend on the MIS weights. Crust deliberately
  reads each mode as one radiance seen by camera, bounce and NEE alike:
  - `materialGlowTintsLight` (the `MeshLightAPI` fallback): glow × `color` ×
    2^`exposure` × `intensity` (÷ area under `normalize`). With the light's inputs
    unauthored this equals the glow, so applying the API to an emissive mesh changes
    its noise, not its brightness.
  - `independent`: the light's inputs alone; the material's glow is ignored; the
    material still reflects.
  - `noMaterialResponse`: the light's inputs alone, and no surface response at all.
  This deviation from the schema's wording is documented as a design choice.
- **MaterialX constant emission becomes hit-free.** A MaterialX surface whose emission
  folds to a constant at compile time (no texture, primvar or shading-point input, and a
  neutral falloff) reports it from `Material::emitted`, the value the light list reads.
  Today every MaterialX surface reports zero there. Rendering is unchanged by this alone.
- **What cannot be a mesh light is refused with a warning** and keeps today's
  BSDF-only behaviour: a glow that varies over the surface (textured, primvar-driven)
  or with direction (under a coat or fuzz) under `materialGlowTintsLight`; a cutout
  material; a mesh placed through a native-instance or `PointInstancer` prototype; a
  mesh that moves over the shutter.
- **Mesh lights are camera-visible and solid by default**, being scene geometry, unlike
  UsdLux lights whose source is hidden from the camera. `crust:light:cameraVisible`,
  the RenderMan camera-visibility primvar and `crust:rayMask` decide it as they do for
  other lights. Light and shadow linking, `inputs:diffuse` / `inputs:specular`, and
  colour temperature apply as for every UsdLux light.
- **`crust diagnostic`'s `unlit_emitters` names the remedy**: the emitting prims, and
  "apply `UsdLuxMeshLightAPI`", instead of "crust has no setting".
- Phase 1 samples triangles by area, one light-list entry per mesh. Solid-angle triangle
  sampling, textured emission and triangles as light-tree leaves are recorded as known
  gaps.
- No image changes for a scene without `MeshLightAPI`.

## Capabilities

### New Capabilities

None: mesh lights are lights, owned by `lighting`.

### Modified Capabilities

- `lighting`: a new "Mesh lights" requirement (what a mesh light emits under each
  `materialSyncMode`, its sampling, visibility, what is refused); "Known gaps" no longer
  lists mesh lights as unsupported, and lists the phase-1 limits instead.
- `usd-scene-import`: "Light schema mapping" reads `MeshLightAPI` on `UsdGeomMesh`
  prims (it currently says mesh lights SHALL NOT be read).
- `materials`: a new requirement that a MaterialX surface's constant emission is
  reported hit-free, bit-identical to what it emits at every hit.
- `diagnostics`: a new requirement that the `unlit_emitters` finding names the emitting
  prims and `MeshLightAPI` as the remedy.

## Impact

- `crust-core/src/light/`: a mesh shape (`AreaShape` variant) with a per-mesh triangle
  table; the bounce side's light density at a hit needs the triangle's geometric normal,
  which `Light::pdf_at_point` does not receive today (`tracer/path.rs`,
  `bounce_emission_weight_at`).
- `crust-core/src/scene/usd_import/`: `lights.rs` (`MeshLightAPI` inputs and
  `materialSyncMode`), `mesh.rs` (world-space triangles at `flush_meshes`, after
  subdivision and displacement), new `WarningCode`s.
- `crust-core/src/material/`: a mesh-light material wrapper that replaces the bound
  material's emission (and, under `noMaterialResponse`, its response), `materialx.rs`
  (`emitted`).
- `crust-mtlx`: the compiled program reports its constant emission (`Compiler::fold`
  already exists).
- `crust-core/src/diagnostic/checks.rs` (`unlit_emitters`).
- Performance: no change for scenes without mesh lights. A mesh light adds one triangle
  table per mesh (one `f32` per triangle) and one CDF search per NEE sample on it.
- Documentation: `site/content/docs/usd/lights.md`, `site/content/docs/reference/warnings.md`,
  `site/content/docs/help/diagnosing-a-render.md`, the `lighting` and `materials` design
  records, `docs/light_sampling.md`.
