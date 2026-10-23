# Tasks

## 1. Supply primvars to any hit (materials)

- [x] 1.1 Move the primvar values from `CurveCtx` to `ShadeCtx` and read `Op::Primvar` from there. Verify with `primvars_are_read_by_name_in_program_order` (a mesh hit) in `crust-mtlx/tests/graph.rs` and `the_jit_matches_the_interpreter_on_curves` (mesh points added) in `crust-jit/tests/jit.rs`.
- [x] 1.2 `Material::primvars`, `PrimvarTable::shading`, `MeshHit`, `HitPart::Mesh`, filled by `World::intersect` and read by `materialx::run`. `a_world_hit_stays_an_inline_copy` keeps the record at 96 bytes.

## 2. Read them at import (usd-scene-import)

- [x] 2.1 Decode the material's primvars beside the render vars' in `mesh_source`, refine them with the surface, split them into the table. Verify with `crust-core/tests/mesh_primvars.rs`: every interpolation, indices, colour space, conversion, refinement, mirrored and instanced placements, a primvar-driven cutout, refusals.
- [x] 2.2 Constant primvars on implicit shapes and analytic spheres, direct and in prototypes (`implicit_shapes_read_constant_primvars`).
- [x] 2.3 `Signature` carries a mesh slot's primvar names, so a material edit that changes them re-imports.

## 3. Documentation

- [x] 3.1 `materials` spec and design record; `usd-scene-import` and `aovs` design records.
- [x] 3.2 User documentation: geometry (primvars in materials), materials, limitations, warnings.
