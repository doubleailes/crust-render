# Tasks

## 1. crust-mtlx

- [x] 1.1 VDF tree (`anisotropic_vdf`, `absorption_vdf`, `mix` / `add` / `multiply`), `volume`, `mix` of `volumeshader`s, `volumematerial`; `flatten_volume`, `compile_terminals`, `Compiled::volume_only`, `Doc::from_nodes`. Verify with `bsdf.rs` unit tests: mix weights the anisotropy by scattering, add, multiply, vacuum, a volume terminal replaces a layered VDF, volume-only flags.

## 2. crust-core materials

- [x] 2.1 `Material::is_medium_boundary` / `boundary_medium`, `MtlxMaterial` volume-only, `closure::volume_medium`, `World::has_medium_boundaries`, `materialx::from_compiled`.

## 3. Integrator

- [x] 3.1 Crossing (carry, `restarted_past`, per-stretch domains), `Enclosure`, enclosure medium on every traced ray, NEE + `PrevVertex::Phase` at enclosure scatters, `medium_shadow`, `pass_boundaries`, light-cache visibility. Verify: `tests/volume_materials.rs` (Beer–Lambert exact to the restart epsilon at unit and ×10 directions; white furnaces under MIS, NEE-only and BSDF-only; a white ball inside fog); every sample without a boundary or region bit-identical against the previous binary at 16 spp; cornellbox within 0.5% instructions (measured +0.43%); mesh boundary vs `crust:volume` region of the same medium agree to 0.3%.
- [x] 3.2 Volume regions measure distance (`Volumes::sample_interaction` / `transmittance` scale by the direction's length). Verify: `tracking_measures_distance_not_the_ray_parameter`; the region image no longer depends on the camera's focus distance.

## 4. USD import

- [x] 4.1 `scene/usd_import/mtlx_network.rs` (nodedef parse, connections through graph outputs and interface inputs, layer-anchored assets); volume terminal first, `mtlx` last surface context. Verify with `nodedef_names_split_into_category_and_signature`, `tests/volume_materials.rs` (inline surface, preview precedence, surface + volume interior) and `usd_scene.rs::loads_materialx_volume_usda`.

## 5. Records

- [x] 5.1 `samples/materialx_volume.usda`; materials, rendering and usd-scene-import design records; `docs/architecture.md` invariant; README; site `usd/materials.md`, `usd/volumes.md`, `architecture/limitations.md`.
