## Context

The long-form record is `docs/color_management.md` (§ OpenColorIO and the
working space); this file keeps the decisions and why.

## Decisions

**D1 — The working space travels with each request, not in a global.** The
OCIO config is process-wide (as OpenColorIO's own current config is), but the
working space is per stage: `ColorSpace` carries it, `ImportCaches::working`
holds it, `Scene::working_space` reports it. Two stages in different spaces in
one process — every integration-test binary — cannot see each other's.

**D2 — Unauthored means "already in the working space".** MaterialX says so for
a document without `colorspace`, UsdLux for `inputs:color`, and OCIO renderers
do the same. The alternative — unauthored is `lin_rec709` — would silently
shift every constant of an ACEScg-authored scene. Consequence: a Rec.709 scene
rendered with `--working-space acescg` keeps its untagged constants' numbers,
which then mean AP1 colours; such a scene should tag them, or render in
`lin_rec709`.

**D3 — Split each conversion into curve and matrix, from the processor
itself.** The optimised OCIO processor of every texture space is per-channel
ops then at most one `Matrix`; crust takes the matrix exactly (`f64`) and runs
the rest as an OCIO processor. Anything else is refused with a warning. A
numerical decomposition (columns of the processor applied to unit vectors) was
prototyped and rejected: it is exact only to `f32` round-off and cannot tell a
shape it does not handle.

**D4 — Byte textures keep byte storage; the matrix runs per lookup.** Storing
converted `f32` texels would quadruple residency, and the streaming cache's
budget is a byte count. A linear map commutes with the area-weighted mip
average and the bilinear/trilinear filter, so applying it after filtering is
the same light. One function (`color::apply_gamut`) applies it everywhere, so
streamed and preloaded backends stay bit-identical.

**D5 — Clamp a change of primaries at zero, nothing else new.** An out-of-gamut
colour has a negative component, meaningless as reflectance or emission. A
curve alone is not clamped on output (ACEScct's lowest codes decode negative,
as in OCIO).

**D6 — Chromaticities only off Rec.709.** OpenEXR defines a file without the
attribute as Rec.709 / D65, so writing it in `lin_rec709` adds nothing and
would break the beauty's byte-identity. Primaries are looked up by ASWF interop
ID (fixed by their standards), not derived through the config's reference
space, whose chromatic adaptation would report D65 for ACES's white.

**D7 — Luminance weights are the working space's, carried by value.** Every
heuristic that weighs a colour by one number — light power, the light cache,
guiding training, environment importance, lobe selection, adaptive sampling,
the `variance` AOV — uses the `Y` row of the working space's RGB → XYZ matrix
(`color::luma`), as Typhoon does. Cycles instead avoids luminance in its
heuristics (average / max); crust keeps luminance, which the `variance` AOV
and adaptive stopping are defined in. The weights travel with the scene
(`LightList::luma`, `OpenPBR::luma`, `MtlxMaterial`), never in a global, for
D1's reason. `lin_rec709` keeps `Luma::REC709`, multiplied in the old order,
so a default render is bit-identical.

**D8 — XYZ comes from the config's scene-referred XYZ space.** The
`cie_xyz_d65_interchange` role names the *display*-referred one in the ACES
configs, so `color::to_xyz` converts into `cie_xyz_d65_scene`, or into
`aces_interchange` plus the standard AP0 → XYZ-D65 matrix (Cycles'
fallback). Blackbody goes from the locus's XYZ straight into the working
space through it (Typhoon), and the working space is identified by comparing
it with each standard's Bradford-adapted matrix to 1e-4 (Cycles), so a config
without interop IDs still writes `lin_ap1_scene`.

**D9 — `UsdColorSpaceAPI` inheritance, without its fallback.** A colour's
space is resolved as `ComputeColorSpaceName` does — attribute metadatum, then
`colorSpace:name` on the prim and its ancestors — but with nothing authored the
value is taken as already in the working space (D2), not as USD's
`lin_rec709_scene`. Neither Typhoon nor Hydra resolves the inheritance for
material inputs.
