# Design

The long-form record is in `openspec/specs/materials/design.md` (§ MaterialX
volume terminals and inline networks) and `openspec/specs/rendering/design.md`
(§ Medium boundaries, § History: volume tracking read the ray parameter as a
distance). The decisions, briefly:

- **D1 — Typhoon's model, not `UsdVolVolume`.** A volume is a material
  terminal: the medium inside the bound geometry. Under a surface it replaces
  that surface's interior (`ApplyVolumeToSurfaceClosure`); alone it is a
  transparent boundary (`MakeVolumeSurfaceClosure`, `IsVolumeOnlyBoundary`).
- **D2 — VDF combinators as program ops.** Coefficients linear, anisotropy
  weighted by scattering lane sums (Typhoon's `_EvalMixVdf` /
  `_AddVdfClosures`), built from `Mul` / `Add` / `DotProduct` / `Div` so a
  textured VDF works and the JIT needs nothing new.
- **D3 — A boundary is not a cutout.** A cutout changes no medium and
  `pass_cutouts` would step over it, so it gets its own trait methods and its
  own world flag; worlds without one keep every fast path and render
  bit-identically.
- **D4 — A crossing is not a vertex.** The stretch before it is carried
  (transmittance and pre-weighted region emission) into the next event; each
  stretch draws from its own domain, numbered `records.len() + crossings`.
- **D5 — One owner at a time** (Typhoon's `MediumState`): enter through a
  front face in vacuum, leave through the owner's back face, nothing else
  changes the medium inside — and every ray the path traces carries it.
- **D6 — NEE inside an enclosure only.** A scatter in a boundary's medium runs
  `volume_nee` (with the vertex's light-sample count, as a region scatter) and
  leaves `PrevVertex::Phase`; a refracting interior keeps the
  old no-NEE pairing, whose shadow rays its own surface blocks.
- **D7 — Inline networks become the document the XML parser would build**,
  literals formatted as `value` text and read through `parse_literal`, so the
  two routes cannot diverge.
- **D8 — `mtlx` last.** A decodable universal or preview surface still wins, so
  no stage that rendered changes; a MaterialX volume beside a non-MaterialX
  surface is ignored with a warning.
