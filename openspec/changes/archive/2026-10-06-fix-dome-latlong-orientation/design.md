## Context

`EnvironmentMap` (`crates/crust-core/src/environment.rs`) owns the only
direction ↔ texel mapping for lat-long maps. `radiance`, `sample` and `pdf` all
call `direction_to_uv` or `uv_to_direction`, so the light's NEE density and the
bounce side's `escaped` pdf cannot disagree. The dome's prim transform is applied
outside the map, as `DomeLight`'s world-to-light rotation. The mapping is
therefore purely the convention in the light's own frame.

The spec text is in `pxr/usd/usdLux/schema.usda` (`DomeLight` and `DomeLight_1`
docs), quoting the OpenEXR documentation:

> Pixel (dataWindow.min.x, dataWindow.min.y) has latitude +pi/2 and longitude
> +pi; pixel (dataWindow.max.x, dataWindow.max.y) has latitude −pi/2 and
> longitude −pi. […] Latitude 0, longitude 0 points into positive z direction;
> and latitude 0, longitude pi/2 points into positive x direction.

Longitude λ therefore runs from +π at u = 0 to −π at u = 1, so λ = π(1 − 2u).
The direction for (λ, latitude) is
`(cos lat · sin λ, sin lat, cos lat · cos λ)`. Solving gives
`u = ½ − atan2(x, z) / 2π`.

## Goals / Non-Goals

**Goals**

- Match the UsdLux/OpenEXR convention exactly, and match Typhoon (the OpenUSD
  reference delegate) bit for bit in the mapping formula.
- Keep `radiance`, `sample` and `pdf` one density, as today.

**Non-Goals**

- `RectLight` texture coordinates (`lux::RectTexture`) follow hdEmbree's own
  convention and are untouched.
- `DomeLight_1`'s `poleAxis` is not read today. This change does not add it.
  When it is added, `poleAxis = Y` is this mapping.
- Formats other than `latlong`/`automatic` remain refused.

## Decisions

### D1. Fix the mapping, not the import

The alternative is to keep the mapping and pre-rotate every dome by 180° at
import. That would leave a second convention hidden in the importer, and any
other caller of `EnvironmentMap` would inherit the wrong one. The mapping is the
single place the convention lives, so it is where the fix goes. After the change,
the module header and the formula say the same thing as the spec.

### D2. Exact formulas

```text
direction_to_uv(d):  v = acos(clamp(d.y)) / π
                     u = wrap(½ − atan2(d.x, d.z) / 2π)
uv_to_direction(u,v): θ = π v,  φ = 2π (½ − u)
                     d = (sin θ sin φ, cos θ, sin θ cos φ)
```

These are Typhoon's `_DirectionToLatLongUv` and `_LatLongUvToDirection`, without
its half-open clamps. crust already wraps u with `rem_euclid` and clamps v.

### D3. No environment switch

`CLAUDE.md` keeps environment switches for A/B tests of an optimization against
the behaviour it replaced. This is a correctness fix with no performance side, so
there is nothing to A/B and no switch is added. The before/after is recorded once,
as numbers, in the `lighting` design record.

## Risks / Trade-offs

- **Every textured-dome render changes.** That is intended. The risk is a sample
  whose author rotated the dome to make the old convention look right. Task 3.2
  reviews the samples that author a dome rotation.
- **Users' existing scenes lit under the old convention flip.** The spec is the
  contract, and Typhoon (the OpenUSD reference delegate) already follows it, so such
  scenes now render as they do everywhere else.
