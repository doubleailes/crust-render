# Tasks

## 1. crust-mtlx

- [x] 1.1 `Closures::opacity` and `Leaf::rotation`, visited by `for_each_slot` so the optimiser remaps them.
- [x] 1.2 The surface builders: OpenPBR `geometry_opacity`, `standard_surface` `luminance(opacity)`, glTF `alpha_mode` (D3), the stdlib `surface` node's `opacity`. `specular_rotation` / `coat_rotation` behind `ifgreater(anisotropy, 0)`, glTF `anisotropy_rotation` on every base leaf, not the clearcoat (D6). Remove their reports.

## 2. crust-core

- [x] 2.1 `Material::has_cutout` / `opacity`; `MtlxMaterial` runs the opacity's own program (D2); `World::has_cutouts`.
- [x] 2.2 `present_hit` and `cutout_transmittance` (D4, D5); `--stats` counters. Verify: every sample scene renders bit-identically at 16 spp, cornellbox's instruction count unchanged within noise, strategies agree through a cutout, an opaque occluder still blocks with and without cutouts in the world.
- [x] 2.3 `Frame::rotated` in `closure::prepare`. Verify with probe tests of every rotated leaf and the furnace on a rotated anisotropic metal.

## 3. Native cutouts

- [x] 3.1 `OpenPBR::has_cutout` / `opacity` from `geometry_opacity` (so `crust:openpbr` `geometryOpacity` and PxrDisney `alpha`). Verify: a black sphere at 0.5 in the furnace reads 0.25.
- [x] 3.2 `UsdPreviewSurface` `opacityThreshold > 0`: a constant thresholded into `geometry_opacity`, a texture as `PreviewSurface::with_cutout`, forwarded through `PatternMaterial`. Verify in `preview_surface_opacity_refracts_unless_it_is_a_cutout`.

## 4. Records

- [x] 4.1 `samples/materialx_cutout.{usda,mtlx}`; materials and rendering design records, cli cookbook, README, `docs/architecture.md`, `docs/material_fidelity.md`, `docs/embree_comparison.md`, `docs/openpbr_reference_alignment.md`.
