+++
title = "Volumes"
description = "Homogeneous, procedural smoke and voxel-grid volumes with crust:volume:* attributes."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-01T08:00:00+00:00
draft = false
weight = 60
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'A prim with a <code>crust:volume:type</code> attribute becomes a box of fog, smoke, absorbing medium or fire.'
toc = true
top = false
+++

## How a volume is defined

Any prim with a `crust:volume:type` attribute is imported as a **volume region**. It is
not rendered as a surface. The region is a box:

- If the prim authors a `size` attribute, as a `Cube` does, the box goes from `-size/2` to
  `+size/2` on each axis. USD's default `Cube` size is 2.
- Otherwise the box is the unit cube, from `-0.5` to `+0.5`.

The prim's transform places, rotates and scales the box. Volumes are lit by every light,
cast shadows, and scatter light between surfaces.

```usda
def Cube "Fog"
{
    double size = 2
    token crust:volume:type = "homogeneous"
    color3f crust:volume:sigmaS = (0.15, 0.15, 0.15)
    color3f crust:volume:sigmaA = (0.01, 0.015, 0.025)
    float crust:volume:anisotropy = 0.3
    float3 xformOp:scale = (5, 3, 5)
    uniform token[] xformOpOrder = ["xformOp:scale"]
}
```

Crust Render doesn't read `UsdVolVolume` or OpenVDB files. Volumes are only the three
types on this page.

## crust:volume:type

`token`, **required**.

| value | density inside the box |
|-------|------------------------|
| `homogeneous` | constant: 1 everywhere |
| `smoke` | procedural fractal noise; see [Smoke](#smoke) |
| `grid` | a voxel grid authored on the prim; see [Grid](#grid) |

Any other value logs a warning, and the prim is skipped.

## Medium properties

These attributes apply to every volume type. At each point, the scattering and absorption
coefficients are `sigma × densityScale × density`, where `density` comes from the
volume type.

| attribute | type | default | meaning |
|-----------|------|---------|---------|
| `crust:volume:sigmaS` | `color3f` | (0.5, 0.5, 0.5) | scattering coefficient per channel, per scene unit, at density 1 |
| `crust:volume:sigmaA` | `color3f` | (0, 0, 0) | absorption coefficient per channel, per scene unit, at density 1 |
| `crust:volume:densityScale` | `float` | 1.0 | multiplies both coefficients |
| `crust:volume:anisotropy` | `float` | 0.0 | Henyey–Greenstein `g`, between -1 and 1: above 0 scatters forward, below 0 scatters backward, 0 scatters evenly |
| `crust:volume:emission` | `color3f` | (0, 0, 0) | emitted radiance. Its contribution is weighted by absorption, so emission follows the density, as fire does. It has no effect where `sigmaA` is 0. |

Some typical media:

| medium | settings |
|--------|----------|
| thin fog | `sigmaS` ≈ 0.1–0.2, `sigmaA` ≈ 0.01 |
| white smoke | high `sigmaS`, low `sigmaA`, `anisotropy` ≈ 0.2 |
| dark, sooty smoke | `sigmaA` close to `sigmaS` |
| coloured liquid or glass tint | `sigmaS` = 0, coloured `sigmaA` |
| fire | `sigmaA` > 0, bright `emission`, low `sigmaS` |

## Smoke

With `crust:volume:type = "smoke"`, the density is fractal value noise (fBm), evaluated
over the box's local coordinates from 0 to 1:

`density = max(0, fbm − threshold) / (1 − threshold)`

The threshold carves wispy holes, which make the noise look like smoke rather than haze.
The noise is deterministic: the same seed renders the same smoke on any machine.

| attribute | type | default | meaning |
|-----------|------|---------|---------|
| `crust:volume:noiseScale` | `float` | 4.0 | base frequency: the number of noise cells across the box at the first octave |
| `crust:volume:noiseOctaves` | `int` | 4 | number of noise octaves, at least 1 |
| `crust:volume:noiseGain` | `float` | 0.5 | amplitude multiplier from one octave to the next |
| `crust:volume:noiseLacunarity` | `float` | 2.0 | frequency multiplier from one octave to the next |
| `crust:volume:noiseThreshold` | `float` | 0.3 | noise level below which density is 0. Clamped to 0–0.999. |
| `crust:volume:noiseSeed` | `int` | 0 | changes the noise pattern |

```usda
def Cube "Smoke"
{
    double size = 2
    token crust:volume:type = "smoke"
    float crust:volume:densityScale = 12
    color3f crust:volume:sigmaS = (0.8, 0.8, 0.8)
    color3f crust:volume:sigmaA = (0.08, 0.08, 0.08)
    float crust:volume:anisotropy = 0.2
    float crust:volume:noiseScale = 4
    int crust:volume:noiseOctaves = 4
    float crust:volume:noiseThreshold = 0.25
    int crust:volume:noiseSeed = 42
    float3 xformOp:scale = (0.9, 1.6, 0.9)
    uniform token[] xformOpOrder = ["xformOp:scale"]
}
```

## Grid

With `crust:volume:type = "grid"`, the density comes from a voxel grid authored on the
prim. The grid fills the box. Values are taken at voxel centres and interpolated
trilinearly. Outside the outer voxel centres, the edge values are held.

| attribute | type | required | meaning |
|-----------|------|----------|---------|
| `crust:volume:gridDims` | `int[3]` | yes | number of voxels along x, y and z: `[nx, ny, nz]` |
| `crust:volume:gridData` | `float[]` | yes | `nx × ny × nz` density values |

The data is ordered with **x fastest**, then y, then z: the voxel `(x, y, z)` is at index
`x + nx × (y + ny × z)`.

The prim is skipped with a warning if either attribute is missing, if `gridDims` doesn't
have exactly three values, or if the length of `gridData` isn't `nx × ny × nz`.

```usda
def Cube "Puff"
{
    double size = 1
    token crust:volume:type = "grid"
    float crust:volume:densityScale = 6
    color3f crust:volume:sigmaS = (0.7, 0.7, 0.7)
    int[] crust:volume:gridDims = [2, 2, 2]
    float[] crust:volume:gridData = [0, 1, 1, 0, 1, 0, 0, 1]
}
```

For large grids, write the `.usda` from a script, or store the data in a binary `.usdc`
layer.

## Examples

- `samples/fog.usda`: homogeneous fog in a room, with light shafts around a ball.
- `samples/smoke.usda`: procedural smoke, a glowing homogeneous "ember" and a small grid
  volume.
