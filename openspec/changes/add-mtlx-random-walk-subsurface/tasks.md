# Tasks

## 1. The walk

- [x] 1.1 Port `ty::RandomWalkSSS` to `crust-core/src/subsurface.rs` (Chiang remap, channel MIS, Dwivedi guiding, similarity, owner-only tracing, `ExitLambertian`). Verify with `subsurface/tests.rs`: slab albedo within 0.05 of the colour for `g ≤ 0.6` (measured within 0.02), a chromatic radius keeps the albedo, a thin slab transmits, foreign geometry is ignored, Dwivedi sampling matches its pdf, the remap's shape.

## 2. The closure leaf and the integrator

- [x] 2.1 `Lobe::Subsurface` with zero value, the entry refraction and the entry interface (D2, D3); a zero radius stays a diffuse. Verify with the probe tests in `tests/mtlx_surfaces.rs`.
- [x] 2.2 Run the walk from `trace_path` and resume at the exit (D4, D6); `--stats` counters. Verify: every sample scene without a subsurface leaf renders bit-identically (`scripts/check_images.sh`), cornellbox's instruction count within 0.5%, the white furnace bounds every subsurface fixture, `resolve.rs` pins the new fixture.
- [x] 2.3 Remove the `subsurface_bsdf` report.

## 3. Records

- [x] 3.1 `openspec/specs/materials/design.md` (the walk, its traps, known gaps), rendering and cli records, README, `docs/architecture.md`, `docs/openpbr_reference_alignment.md`, `docs/material_fidelity.md`.
