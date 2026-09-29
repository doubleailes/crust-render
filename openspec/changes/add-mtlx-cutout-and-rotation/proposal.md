# Proposal

## Why

The MaterialX known gaps listed opacity cutouts and anisotropy rotation as
"reported, not implemented". Both are authored constantly: glTF assets carry
`alpha_mode` MASK leaf cards and decals, `standard_surface` and OpenPBR carry
`opacity` / `geometry_opacity`, and brushed metals set `specular_rotation` /
`anisotropy_rotation`. In the Material Fidelity suite the four alpha samples
(`input_alpha_mode_mask`, `input_alpha_cutoff`, `opacity_mask`,
`alpha_mode_mask`) were the largest shading shortfalls, 8.7–17.7 dB below the
reference. A surface that should have holes rendered solid, with a warning.

The `add-mtlx-random-walk-subsurface` change already follows NVIDIA's Typhoon
(`typhoon/main` of NVIDIA-Omniverse/OpenUSD, hdEmbree) for the closure tree.
Typhoon reads the same inputs: it carries opacity as `presence` beside the
closure, and turns the tangent with a `Rotate3d` identical to MaterialX's
`mx_rotate_vector3`. This change follows MaterialX's nodegraphs where the two
differ (standard_surface's opacity is a luminance in the graph and a channel
average in Typhoon).

## What Changes

- **Opacity becomes presence.** Each surface node's graph ends in a `surface`
  node whose `opacity` crust now carries beside the closure tree
  (`Closures::opacity`): OpenPBR's `geometry_opacity`, `standard_surface`'s
  `luminance(opacity)`, and glTF's `alpha` through `alpha_mode` (OPAQUE 1,
  MASK `alpha ≥ alpha_cutoff`, BLEND `alpha`). A stdlib `surface` node's
  `opacity` is carried the same way.
- **The integrator honours it** through a new `Material::opacity` /
  `has_cutout` pair. A path meets a cutout hit with probability equal to its
  opacity and otherwise carries on along the same ray, spending no depth. A
  shadow ray is attenuated by `Π(1 − opacity)`. A world with no cutout
  material runs the old code bit for bit.
- **Anisotropy rotation turns the leaf frame.** `standard_surface`'s
  `specular_rotation` / `coat_rotation` and glTF's `anisotropy_rotation`
  become a per-leaf angle (`Leaf::rotation`), applied to the leaf's shading
  frame, with MaterialX's sign convention.
- **The load warnings for these inputs are removed.** glTF `occlusion` is
  still reported.
- **`--stats`** reports the ray queries cutouts cost and the hits passed
  through. `examples/mtlx_shade` prints the opacity and each leaf's tangent.
- **A fixture**, `samples/materialx_cutout.usda` + `.mtlx`: a MASK leaf card,
  a partly present OpenPBR sphere, a rotated brushed metal.

## Capabilities

### Modified Capabilities

- `materials`: MaterialX opacity and anisotropy rotation are applied rather
  than reported.
- `rendering`: the integrator treats a cutout as stochastic presence, on both
  the bounce and the shadow side.

## Impact

- MaterialX materials that author opacity or a rotation render differently:
  they get holes, and turned highlights. Every other scene is bit-identical
  (all 25 other samples at 16 spp). Instructions (callgrind, 2 spp): cornellbox
  +0.04%, `materialx_basic` +0.64%.
- In a scene with a cutout, every blocked shadow ray is re-walked by closest
  hits, since the kernel has no any-hit filter. The cost stays in those scenes.
- Native `crust:openpbr` `geometryOpacity`, PxrDisney `alpha` and
  `UsdPreviewSurface` `opacityThreshold` still render opaque. The hook is
  generic; turning them on changes other importers' output (Moana among them)
  and is left to its own change.
- Depends on `add-mtlx-random-walk-subsurface` being archived first: its
  reporting requirement is the one this change modifies.
