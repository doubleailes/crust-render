# Tasks

Tasks 2.4 onward read curve properties and primvars through the strand
tables of `curve-attributes-to-materials`. No task starts until that change
(PR #322) has merged and this branch is rebased onto `main`.

## 1. Measured spike

- [x] 1.1 Build `samples/mass_spike.usda` (two sheets of strands crossing at an angle, and one round clump) and a spike example that bakes a dense, unbricked field from its strands and ray-casts the camera's pixels, writing the per-pixel tube normal and mass normal at the first hit; verify it runs on the sample and writes both images
- [ ] 1.2 Measure the gradient energy of the normal across the crossing line against the same measure away from it, for the tube normal and for the mass normal, and the same for a per-strand colour against the coverage-weighted mass colour; verify the mass ratios are recorded beside the tube ones and the mass ratio is near 1
- [x] 1.3 Repeat 1.2 over five consecutive frames of a jittered groom (each strand's points displaced per frame by a fixed fraction of the radius) and measure the frame-to-frame change of the mass normal; verify the numbers are recorded
- [x] 1.4 Record the spike's measurements, and the radius and threshold they settle on, in `openspec/changes/curve-mass-field/design.md`; if the mass ratio does not approach 1 or the normal shimmers between frames, stop and revise the design before group 2

## 2. Import: attributes, grouping and the colour terminal

- [ ] 2.1 Add the warning codes `mass.invalid_setting`, `mass.not_curves`, `mass.color_unsupported` and `mass.unplaceable`, each with one fixed kind, and their rows in `site/content/docs/reference/warnings.md`; verify the warnings-table test passes
- [ ] 2.2 Read `crust:mass:radius`, `crust:mass:threshold`, `crust:mass:group` and `crust:mass:maxResolution` on `BasisCurves` and `NurbsCurves`, refusing out-of-domain values with `mass.invalid_setting`; verify inline-USD tests for a negative radius, a zero cap and the defaults
- [ ] 2.3 Report `crust:mass:*` on any other prim with one `mass.not_curves` per prim, baking nothing; verify a test with a radius on a `Mesh` that renders as before
- [ ] 2.4 Read a `Material`'s `outputs:crust:mass:color` terminal through the inline-network translator into a colour program kept beside the material, ignoring any non-MaterialX connection with `mass.color_unsupported`; verify a test that the material's surface shading is bit-identical with and without the terminal
- [ ] 2.5 Collect opted-in prims into fields: one per prim, or one per `crust:mass:group` token under the same placement; verify tests for three prims sharing a group (one field) and two placements sharing a token (two fields)
- [ ] 2.6 Document the `crust:mass:*` attributes and the colour terminal on a new `site/content/docs/usd/` page linked from the USD overview's attribute table; verify `zola build` in `site/` passes its link check

## 3. The bake

- [ ] 3.1 Sample each opted-in strand at a fixed fraction of the voxel size and splat coverage, weighted by width, into sparse bricks in a fixed accumulation order; verify unit tests that a lone straight strand as wide as the radius has coverage 1 on its centre line and one twice as wide has 2
- [ ] 3.2 Size voxels at half the radius under `crust:mass:maxResolution`, growing them when the cap binds and logging that once at DEBUG; verify a test where bounds 1000 radii across bake 512 voxels along that axis
- [ ] 3.3 Compute the signed distance to the coverage threshold by fast sweeping in a narrow band of a few radii, clamped beyond; verify a lone strand's distance is negative on its axis, positive at twice the radius and crosses zero between, and a dense round clump's matches its analytic distance within half a voxel
- [ ] 3.4 Bake the colour terminal's program at the strand samples, with each sample's position, curve properties and primvars, into a coverage-weighted mean; refuse nodes needing a view or a hit with `mass.color_unsupported` and their defaults; verify tests for two coloured sheets meeting, a root-to-tip ramp, and a `viewdirection` node refused
- [ ] 3.5 Run every field's bake in parallel after the USD read, with the frame's time passed explicitly; verify a test that bakes the same scene with one and with eight threads and compares every field bit for bit
- [ ] 3.6 Report fields, bricks, bytes and bake time on `--stats`; verify the stats test and the `--stats` description in the cli design record
- [ ] 3.7 Write `openspec/specs/curve-mass-field/design.md` with the bake's decisions, the spike's measurements and the known gaps (instanced placements, motion blur); verify it links from `CLAUDE.md`'s design-record table

## 4. Lookups

- [ ] 4.1 Keep the baked fields beside the world, keyed by geometry, and read one at a hit's position: as is for top-level curves, and through the placement's inverse (normal through its inverse transpose) for a direct, static, unlabelled instance; verify tests that crossing sheets read the same normal and colour at the crossing, and that two rotated instances of a groom read normals rotated with them
- [ ] 4.2 Give hits through any other placement every lookup's default, raising one `mass.unplaceable` per prim; verify a `PointInstancer` test
- [ ] 4.3 Compute the mass normal as the normalised central-difference gradient of trilinear distance, depth as the negated distance where negative, and density as coverage; verify the round-clump test (radial normal at the silhouette, unit normal and positive depth on the axis)
- [ ] 4.4 Add `crust_mass_color`, `crust_mass_normal`, `crust_mass_depth` and `crust_mass_density` nodedefs in a crust library shipped beside the standard library, compiled to ops reading the field through the shading context, each returning its `default` off a field; verify crust-mtlx tests, a mesh test with no warning, and the interpreter ↔ JIT bit-identity test over all four
- [ ] 4.5 Document the four nodes on the mass page and in the materials design record; verify `zola build`

## 5. The mass diffuse leaf

- [ ] 5.1 Add `crust_mass_diffuse_bsdf` (`weight`, `color`, `normal`): Lambertian around its normal, cosine-sampled with the matching pdf, its normal kept whichever way it faces and replaced by the hit's only when non-finite or zero; verify a white-furnace test returning `color`, a sampling-histogram test, the opposed-normal shading test and its `tests/resolve.rs` entry
- [ ] 5.2 Make the leaf's light pass out of its strand as a fibre's does, and widen the mixed-vertex split to any non-fibre leaf that transmits or whose prepared normal differs from the hit's; verify the tilted-diffuse variant of `only_the_fibres_light_passes_out_of_a_mixed_strand`, the mass-lit-from-behind test, and that the existing hair tests are unchanged
- [ ] 5.3 Verify the widened split costs nothing where it does not engage: cornellbox and `samples/hair.usda` instruction counts (callgrind, `-s 2`) within 0.1% of the branch point, and their images bit-identical
- [ ] 5.4 Document the leaf, the normal exception and the widened split in the materials design record and the site's materials page; verify `zola build`

## 6. Live edits

- [ ] 6.1 Add every `crust:mass:*` attribute, group membership and the colour terminal's graph to `Signature`, so an MCP edit of any of them rebuilds the affected fields; verify tests in `tests/live.rs` that a radius edit, a group change and a colour-graph edit each render equal to a full re-import

## 7. Sample and integration

- [ ] 7.1 Add `samples/hair_mass.usda` / `.mtlx`: a groom whose material blends a per-strand colour and the mass colour, normal and diffuse, faded by density for flyaways, documented in the file; verify it renders and `crust check` reports no warning
- [ ] 7.2 Measure bake time and field memory on the largest groom available, and record them in the curve-mass-field design record; verify the numbers are there
- [ ] 7.3 Verify every existing sample renders bit-identically to the branch point (`scripts/check_images.sh check`), and run the CI set: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`, `cargo deny --locked check`, and the pinned nightly clippy

## Workflow follow-up

- Rebase onto `main` once `curve-attributes-to-materials` (PR #322) has merged, before task 1.1.
- Archive the change after review.
