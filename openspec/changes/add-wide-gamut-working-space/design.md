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

**D7 — Luminance heuristics stay Rec.709.** `utils::luminance` is on the hot
path; threading the working space's weights there is a separate change. The
image stays unbiased. Recorded as a known gap.
