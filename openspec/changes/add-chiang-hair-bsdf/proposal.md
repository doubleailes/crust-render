# Proposal

## Why

Crust imports `UsdGeomBasisCurves` as round tubes and intersects cubic spans
exactly, but it has no fibre scattering model: a strand is shaded by whatever
surface material is bound to it, against the tube's outward normal. A groom
therefore renders as a mass of thin plastic or matte cylinders: no
forward-scattered glow when the hair is backlit, no secondary (TRT) highlight
shifted by the cuticle tilt, and no colour from absorption inside the fibre.

MaterialX 1.39 standardises a hair closure, `chiang_hair_bsdf` (Chiang, Bitterli,
Tappan and Burley 2016, the model pbrt-v3 ships), together with three helper
nodes that turn artist parameters into its inputs. The closure is the only
portable way a USD asset carries a hair look, and crust's MaterialX path is
already how it reads every other closure. Implementing that node, and making a
curve hit report the strand direction the model needs, lets a USD groom carry
its look into crust.

## What Changes

- **`chiang_hair_bsdf` becomes a supported MaterialX closure leaf.** It is
  evaluated as the energy-conserving, importance-sampled Chiang 2016 model
  (pbrt-v3's `HairBSDF`), with the MaterialX parameterisation: per-lobe tints
  (`tint_R`, `tint_TT`, `tint_TRT`; TRRT+ uses `tint_TRT`), per-lobe
  `(longitudinal variance, azimuthal scale)` roughness pairs, `ior`,
  `cuticle_angle` remapped from [0, 1] to [−π/2, π/2], and
  `absorption_coefficient`. It scatters over the whole sphere of directions, not
  a hemisphere. It combines with other leaves through the usual `mix`, `add`
  and `multiply` combinators. Today the leaf is dropped, with a warning that
  names the node as having no operator.
- **The three helper nodes become pattern nodes**, matching MaterialX's GLSL
  implementations: `chiang_hair_roughness`, `chiang_hair_absorption_from_color`
  and `deon_hair_absorption_from_melanin`. They work in the interpreter and in
  the JIT, and give bit-identical results in both.
- **A curve hit reports its strand direction.** Rays that hit a `BasisCurves`
  prim get the curve's tangent as the shading tangent, which is what
  `chiang_hair_bsdf`'s `curve_direction` (`Tworld`) defaults to. The position
  across the strand is derived from the tube's normal, as MaterialX does.
  OpenPBR, `UsdPreviewSurface` and `Emissive` ignore the tangent, so curves
  with those materials render bit-identically. A MaterialX leaf on a curve now
  builds its frame around the strand, where before the frame was arbitrary.
  Its image is the same up to sample noise, and an anisotropic highlight now
  runs along the strand.
- **A strand does not shadow or block its own scattered light.** A
  `chiang_hair_bsdf` hit already accounts for the light's path through the
  fibre. So a ray leaving that hit toward the light, or continuing through the
  strand, ignores the strand's own far wall rather than meeting it from inside.
- **A fixture**, `samples/hair.usda` + `samples/hair.mtlx`: backlit and
  front-lit tufts of curves, using each way of reaching the leaf (a bare leaf
  with constant inputs, the roughness helper, absorption from colour, and
  absorption from melanin).

## Capabilities

### New Capabilities

None. Hair shading is a MaterialX closure, and the materials capability owns
those.

### Modified Capabilities

- `materials`: `chiang_hair_bsdf` joins the supported closure leaves, with its
  scattering contract (sphere-wide, energy-conserving, sampling consistent with
  evaluation). The three hair helper nodes join the pattern nodes evaluated as
  the MaterialX reference.
- `intersection-kernel`: a curve hit reports its span parameter and its
  tangent, carried through instances like the normal. A ray can ask to ignore
  the hits where it leaves a curve's tube.
- `usd-scene-import`: a new requirement that `BasisCurves` import as round
  curves shaded along their strand. Today curves are only described in the
  design record.

## Impact

- **Code:**
  - `crates/crust-mtlx`: parsing, the closure leaf and the helper nodes.
  - `crates/crust-jit`: lowering for the helper nodes.
  - `crates/crust-core`: the hair lobe's eval, sample and pdf; frame
    construction on curve hits; the self-hit rule for rays leaving a hair hit.
  - `crates/crust-rt`: the curve intersectors return the span parameter they
    already compute, and the tangent. Instances carry the tangent through. A ray
    flag skips curve exit hits. A curve hit's `t` and normal stay bitwise
    unchanged.
- **Images:** scenes without curves stay bit-identical, and so do curves
  shaded by OpenPBR, `UsdPreviewSurface` or `Emissive`. Every sample scene
  falls in one of those two groups. MaterialX on curves changes only its
  sample pattern, plus the direction of any anisotropy. The hit record grows by
  one vector. On `cornellbox`, which has instances but no curves, the
  budget is +0.3% instructions, measured with callgrind.
- **AOVs:** in light path expressions, a hair leaf is classified by hemisphere,
  like every other leaf: glossy `specular` reflection on the viewer's side, and
  glossy `transmission` on the far side. There is no new label.
- **Docs:** the materials page of the user documentation (`site/`), the
  materials and usd-scene-import design records, and a new known gap for what
  is approximated (for example, near-field azimuthal scattering on a tube that
  is very thick relative to the pixel).
- **No new dependencies.** No new environment switch: nothing existing is
  replaced, so there is no older behaviour to A/B against.
