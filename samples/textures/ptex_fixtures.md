# Ptex sample textures

Four small `.ptx` files, copied verbatim from
[`ptex-rs`](https://github.com/doubleailes/ptex-rs)'s `tests/fixtures/` (MIT,
© 2026 Philippe Llerena). Upstream generates them with `tools/gen_fixtures.cpp`
against **wdas/ptex v2.4.3**, the reference C++ writer, and pins its decode
against C++ dumps of the same files — so they are real Ptex, not something
crust's own reader agreed with itself about.

The repository otherwise ships no `.ptx` at all, which is why the Ptex tests
that existed before these hand-built their arenas in memory: that pins the mip
reduction and the lookup, but it cannot pin *addressing a file*, which is
exactly what the streaming backend does. Copied rather than fetched so
`cargo test` stays offline, and rather than generated because generating one
means the C++ writer.

They live here rather than under a crate's `tests/` because they are used from
both sides: `crates/crust-assets/tests/ptex_stream.rs` reads them directly, and
`samples/ptex_quads.usda` binds two of them to real geometry so the importer,
the face-id table and both backends can be exercised by an actual render. One
copy, so the unit invariant and the rendered one cannot be checked against
different bytes.

| file | mesh | data | channels | faces | why it is here |
| --- | --- | --- | --- | --- | --- |
| `quad_tiled.ptx` | quad | `uint8` | 1 | 2 | the point of the whole exercise: face 0 is 1024x512 and really is stored as a **tile grid**, over stored mip levels. Also covers a non-square face and the single-channel replicate-to-grey path. |
| `quad_u8.ptx` | quad | `uint8` | 4 (alpha 3) | 4 | the ordinary case, and the one with an alpha channel the decode must ignore. |
| `tri_u16.ptx` | triangle | `uint16` | 1 | 4 | the triangle parameterisation, whose reduction is mirrored rather than a 2x2 box, and a 16-bit sample type that misses the `u8` decode table. |
| `quad_f32.ptx` | quad | `float32` | 3 | 2 | float samples, the other side of that table. |
