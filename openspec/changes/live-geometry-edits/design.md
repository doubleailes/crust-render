## Context

`crust-rt` builds one SBVH → BVH4 tree over every top-level primitive: baked
triangles, spheres, curves and one `Instance` per placement of an instanced
mesh. The build is deterministic in its input (`build_is_deterministic`), and
geometry ids are traversal order. The import decides between baking and
instancing a distinct mesh once the whole stage is walked (`flush_meshes`): a
mesh placed once is baked, one placed more than once is instanced.

## Decisions

### D1. Rebuild, never refit

A geometry edit replaces whole geometries in a kept copy of the
`SceneBuilder` and commits it again. With the same geometries in the same ids
the tree is the one a fresh import builds, so the bit-identity pair "in place ↔
full re-import" holds by construction. A refit was rejected (design records,
`mcp-session` and `usd-scene-import`): its tree is not a fresh build's.

The cost is the top-level build, which includes every baked triangle. On a
stage of mostly instanced geometry that is small; on one of mostly baked
geometry it is the BVH share of an import (19 s of Moana's 5:13, 1.4 s of
ALab's 2:57), still without the traversal, composition and decoding.

### D2. The world keeps its inputs, on request

`World` keeps an `Option<SceneBuilder>` (`WorldBuilder::keep_inputs`): every
top-level `Geometry`, cloned before the commit consumes them. Instances share
their committed trees by `Arc`; baked triangles are copied. Only a session asks
(`UsdImportOptions::keep_geometry`), so `crust render`'s memory and time do not
move.

### D3. The geometry is the import's

Nothing is recomputed by a second implementation:
- **an instanced placement** keeps its committed mesh (the kept input's `Arc`)
  and takes the transform `local_to_world` gives, the traversal's own fold;
- **a baked placement** is read again by `mesh_source` and interned into a
  scratch arena (`MeshArena::intern`, which triangulates and builds the side
  tables), then baked by `flush_meshes`' own `bake_verts`, `bake_indices`,
  `bake_normals`, with its tables set as `flush_meshes` sets them;
- **a sphere** is placed by `emit_sphere` into a scratch world at its id;
- **an area light's emitter** is emitted by `emit_light`, as a `lights`
  rebuild already does, and its geometry taken with it.

### D4. What decides baking must not move

A fresh import decides baking from placement counts and content keys over the
whole stage. A geometry edit is refused when it could move that decision:
- a moved placement keeps its key: a baked mesh read again must give the key it
  had (the move did not change the mesh);
- a reshaped mesh must be placed once, by nothing else (no prototype or
  instancer), and its new key must equal no other distinct mesh's key. The
  provenance keeps every slot's key with its material as a group number,
  since material `Arc`s are per import (`MeshSlotFacts`);
- adaptive subdivision, displacement, shutter motion and primvar AOVs are
  refused: each makes the geometry depend on more than the prim;
- an in-place reshape writes its slot's new key into the provenance
  (`Update::mesh_keys`), and reshapes in one batch are checked against each
  other's new keys, so a later reshape compares against the meshes as they are
  (Qodo review of #384).

### D6. Edits the geometry runs cannot see are refused

The geometry runs name the prim that *emitted* geometry. A prim can also be
read by others: through an `inherits`, `references` or `specializes` arc, or as
a `PointInstancer`'s prototype, whose geometry is attributed to the instancer.
Moving such a prim in place would move its own geometry and leave every
composed copy behind. So an import that keeps its geometry records every prim
site other than a prim's own path, and every instancer's prototype targets
(`Provenance::arc_sources`), and a geometry edit at, above or below one is
`full`. A transform on a prim whose subtree the runs show nothing for (an
instancer's prototype part, an empty Xform) is `full` too: it cannot be proven
inert. A path in a referenced layer's namespace may coincide with an edited
one; that costs a full import, never a stale render.

### D7. The kept geometry is committed by reference

`SceneBuilder::commit_ref` builds from borrowed geometries, through the same
expansion as the owning `commit_with` (which drops each geometry once
expanded), so a geometry edit pays no copy of the kept builder.

### D5. The provenance says what was placed how

`Provenance::meshes` (geom id, distinct mesh, baked, moving) is recorded by
`flush_meshes`, and `Provenance::spheres` by the traversal. The classifier asks
`movable(path)` and `reshapable(path)` before choosing `geometry`; the rebuild
checks again and refuses with `Structural`, which falls back to `full`.

## Risks

- **Memory.** The kept copy doubles the baked triangles. Measured on the
  stages below; `CRUST_SCENE_DELTAS=0` turns it off with every in-place edit.
- **A host that mutates geometry ids.** None does: ids are traversal order,
  which a geometry edit does not change.
