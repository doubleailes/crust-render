# Proposal

## Why

A USD `Material` has three terminals — `surface`, `displacement` and `volume` —
and crust read only the first. A material whose `volume` terminal describes the
fog, smoke or murky liquid inside an object rendered as grey ("no surface
shader"), and a glass whose interior was authored as a volume kept the medium
its own `transmission_*` inputs implied instead. Volumes existed only as
crust's own `crust:volume:*` boxes.

NVIDIA's Typhoon (`typhoon/main` of NVIDIA-Omniverse/OpenUSD, hdEmbree), the
reference crust already ports its MaterialX layering and random walk from,
renders volumes *only* this way: it has no `UsdVolVolume` support at all, and a
volume is the medium inside the geometry a volume material is bound to. It
receives every material as an inline MaterialX network (`ND_*` shaders), which
crust did not read either — inline MaterialX surfaces fell back to grey too.

## What Changes

- **MaterialX volume terminals.** crust-mtlx reads `volumematerial`, `volume`,
  `mix` of volume shaders and the VDF tree (`anisotropic_vdf`,
  `absorption_vdf`, `mix` / `add` / `multiply`) with Typhoon's combinators, and
  `compile_terminals` compiles a material from its surface and volume
  terminals.
- **A volume under a surface is its interior**, replacing the one the
  surface's transmission inputs describe, below a thick transmitting surface.
- **A volume with no surface is a medium boundary**: a new
  `Material::is_medium_boundary` / `boundary_medium` pair. The integrator
  crosses it without a vertex and travels in its medium inside, with NEE + MIS
  at scatters there and shadow rays that cross boundaries paying Beer–Lambert —
  Typhoon's single-owner `MediumState`.
- **Inline MaterialX networks in USD** (`ND_*` shaders on `outputs:mtlx:*` or
  the universal terminals) are translated into crust-mtlx documents and
  compiled as the same graph in a `.mtlx` would be. `mtlx` is the last surface
  context, so stages that render through a preview surface are unchanged.
- **Fix: volume-region tracking measured the ray parameter, not distance.**
  Camera rays are not unit, so region fog seen from the camera was ten times
  thinner (at the default focus distance) than it should be. `fog` and `smoke`
  render denser; every other sample is bit-identical.
- **A sample**, `samples/materialx_volume.usda`.

## Capabilities

### Modified Capabilities

- `materials`: MaterialX volume terminals; inline MaterialX networks.
- `rendering`: medium boundaries; volume tracking in distance.
- `usd-scene-import`: material resolution reads `volume` terminals and `ND_*`
  networks.

## Impact

`crust-mtlx` (`bsdf.rs`, `lib.rs`, `parse.rs`), `crust-core`
(`material/{material,materialx}.rs`, `material/closure`, `tracer/path.rs`,
`volume.rs`, `rt_world.rs`, `ray.rs`, `light_cache.rs`,
`scene/usd_import/{materials,mtlx_network}.rs`). No new dependency, no
`unsafe`, no new CLI flag or environment switch.
