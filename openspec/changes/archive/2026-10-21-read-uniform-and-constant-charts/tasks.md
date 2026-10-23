# Tasks

## 1. Read constant and uniform charts per mesh and per face (usd-scene-import)

- [x] 1.1 In `mesh_uvs`, rewrite a `constant` / `uniform` chart (and an unauthored one of a single value) into faceVarying with synthesized indices composed with the authored `:indices`. Verify with `a_uniform_or_constant_chart_is_read_per_face_or_per_mesh` in `crust-core/tests/usd_inline.rs` (uniform, indexed uniform, constant and unauthored single-value charts, each at levels 0 and 2).

## 2. Documentation

- [x] 2.1 `usd-scene-import` design record: the read-time normalisation and why.
