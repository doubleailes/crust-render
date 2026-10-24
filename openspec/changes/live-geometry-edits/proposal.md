## Why

An interactive host (Mantle's IPR, the Hydra delegate, an MCP agent) moves
things far more often than it rebuilds a stage. Today every transform other
than the render camera's, every point edit and every area light's size is a
full re-import: on a 430k-triangle stage that is ~900 ms per drag step
against ~400 ms for a light edit, and on a production shot two minutes. The
design records rejected BVH refit because a refit tree is not the tree a fresh
build gives, which would break the session's bit-identity with `crust render`.
A *rebuild* of the top-level tree does not have that problem: the SBVH build is
deterministic in its input.

## What Changes

- **A `geometry` class**, between `lights` and `full`. A batch in it replaces
  the geometry it names in the committed world and commits the top-level tree
  again from every top-level geometry; materials, lights, side tables of the
  other geometries and the instanced meshes' own trees are kept.
- **What it covers:**
  - a transform on a prim whose subtree places only direct meshes, static
    analytic spheres and lights (no volume, no camera the render reads), on a
    scene imported without shutter motion;
  - points, normals, topology, subdivision tags and primvars of a mesh placed
    once (baked), not displaced, when the new mesh is identical to no other
    distinct mesh and subdivision is not adaptive;
  - an area light's transform and shape inputs (`width`, `height`, `radius`,
    `length`, `treatAsPoint`, `treatAsLine`): its emitter's geometry is
    replaced with the light.
  A distant or dome light's transform is a `lights` edit.
- **Kept geometry.** `UsdImportOptions::keep_geometry` makes the world keep a
  copy of its top-level geometries (`WorldBuilder::keep_inputs`). A live
  session asks for it (unless `CRUST_SCENE_DELTAS=0`); `crust render` never
  does. The copy costs the baked triangles' memory a second time.
- **Probe identity.** A `geometry` update counts as new geometry for `probe`,
  as a full import does.
- **No render change.** The integrator, the kernel's traversal and the
  import's output are unchanged; the import records two more lists in the
  provenance.
