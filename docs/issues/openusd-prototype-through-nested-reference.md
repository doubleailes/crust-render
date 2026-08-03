# openusd: a native instance's prototype does not materialize when the instanceable prim is composed through a reference on a non-root prim

**Target repository:** <https://github.com/mxpv/openusd>
**Affected version:** `openusd` 0.5.0 (latest on crates.io as of 2026-06)
**Severity:** blocker for reading production stages — silently yields empty geometry
**Status:** **fixed upstream in openusd 0.6.0** — verified against the
reproduction below; all three nested cases now materialize the prototype. The
workspace tracks `openusd = "0.6"` from crates.io and no longer patches to a
fork. 0.6.0 also renames `Stage::prim_at` to `Stage::prim` and gives
`sdf::Value::Token` an interned `tf::Token` in place of a `String`, which is what
an importer written against 0.5.0 has to migrate.

---

## Summary

When a prim carrying `instanceable = true` is brought onto a stage through a
`references` arc **on a prim that is not at the stage root**, its prototype is
reported but never materialized. `Prim::prototype()` returns
`Some(/__Prototype_0)`, yet `Stage::prim_at("/__Prototype_0")` yields an invalid
prim: no type name, no children, and `is_active() == false`.

The same content composed one level higher — the reference sitting on a
root-level prim — materializes correctly. Only the *depth of the prim carrying
the reference* changes between the passing and failing cases.

Because the prototype path is still handed back, a consumer has no signal that
anything went wrong: it walks a prototype with zero children and imports no
geometry. Nothing errors and nothing warns.

## Reproduction

Four files. `proto.usda` holds the geometry, `inner.usda` places it through an
`instanceable` prim, and the two outer layers differ *only* in whether the
reference sits on a root prim or one level below it.

`proto.usda`

```usda
#usda 1.0
( defaultPrim = "P" )
def Xform "P"
{
    def Mesh "M"
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
    }
}
```

`inner.usda`

```usda
#usda 1.0
( defaultPrim = "Root" )
def Xform "Root"
{
    def Xform "A" (
        instanceable = true
        payload = @./proto.usda@</P>
    )
    {
    }
}
```

`outer_root.usda` — reference on a **root-level** prim (works)

```usda
#usda 1.0
( defaultPrim = "World" )
def Xform "World" (
    prepend references = @./inner.usda@</Root>
)
{
}
```

`outer_nested.usda` — reference on a **non-root** prim (fails)

```usda
#usda 1.0
( defaultPrim = "World" )
def Xform "World"
{
    def Xform "G" (
        prepend references = @./inner.usda@</Root>
    )
    {
    }
}
```

Driver:

Only `openusd` is needed as a dependency.

```rust
use openusd::{sdf, usd};

fn main() {
    for (layer, inst_path) in [
        ("outer_root.usda", "/World/A"),
        ("outer_nested.usda", "/World/G/A"),
    ] {
        let stage = usd::Stage::builder()
            .load(usd::InitialLoadSet::LoadAll)
            .open(layer)
            .expect("open");
        let inst = stage.prim_at(sdf::Path::new(inst_path).expect("path"));

        println!("{layer}  instance {inst_path}");
        println!("  is_instance      = {:?}", inst.is_instance());

        let proto_path = inst.prototype().expect("prototype()");
        println!("  prototype()      = {proto_path:?}");

        if let Some(p) = proto_path {
            let proto = stage.prim_at(p);
            println!("  proto type_name  = {:?}", proto.type_name());
            println!("  proto children   = {:?}", proto.children().map(|c| c.len()));
            println!("  proto is_active  = {:?}", proto.is_active());
        }
        println!();
    }
}
```

The files above are checked in under `docs/issues/repro/prototype-nested-reference/`
(`proto.usda`, `inner.usda`, `outer_root.usda`, `outer_nested.usda`); the output
below is from running the driver in that directory. That directory also holds
`inner_ref.usda` / `outer_nested_ref.usda`, the same nested case with a
`references` arc on the instanceable prim instead of a `payload` — it fails
identically, which is the basis for the first "ruled out" item below.

## Actual behaviour

```text
outer_root.usda  instance /World/A
  is_instance      = Ok(true)
  prototype()      = Some(Path { path: "/__Prototype_0" })
  proto type_name  = Ok(Some("Xform"))
  proto children   = Ok(1)
  proto is_active  = Ok(true)

outer_nested.usda  instance /World/G/A
  is_instance      = Ok(true)
  prototype()      = Some(Path { path: "/__Prototype_0" })   <-- path is reported
  proto type_name  = Ok(None)                                <-- but the prim is invalid
  proto children   = Ok(0)
  proto is_active  = Ok(false)
```

## Expected behaviour

The second case should match the first: `/__Prototype_0` should be a valid
`Xform` with one child (`/__Prototype_0/M`, a `Mesh`). Nesting the prim that
carries the reference should not affect whether a prototype materializes.

Failing that, `prototype()` returning a path that does not resolve is itself a
problem — either it should return `None`/`Err`, or the prim should exist.

## What was ruled out

- **Not the arc type on the instanceable prim.** Reproduces with `payload` and
  with `references` on the inner prim alike.
- **Not payload loading.** The stage is opened with `InitialLoadSet::LoadAll`.
- **Not reference depth on its own.** A reference on a root prim to a layer
  containing instanceable prims composes correctly; only moving that reference
  under another prim breaks it.
- **Not the outer prim's type.** Fails identically with `def Xform "G"` and with
  an untyped `def "G"`.
- **Not the `is_active() == false`.** That looks like a symptom of the prim being
  invalid rather than a separate cause: exempting the prototype root from an
  inactive-prim check does not recover the children.

## Impact

This makes Disney's [Moana Island Scene](https://disneyanimation.com/resources/moana-island-scene/)
unreadable through its own root layer. `usd/island.usda` is exactly the failing
shape:

```usda
def Xform "island" (kind = "assembly")
{
    def Xform "isBayCedarA1" (
        prepend references = @./elements/isBayCedarA1/element.usda@</isBayCedarA1>
    ) { }
    ...
}
```

Every element places its geometry through `instanceable = true` prims (1 to 14
per element), so with the reference sitting at `/island/<element>` — one level
below the root — each element imports as **empty**. Opening an individual
`elements/<name>/element.usda` directly works and yields the geometry, which is
how the discrepancy was noticed.

Measured on one element (`isGardeniaA`, 14 instanceable placements): 0
geometries through the nested reference, 742 instances / 245 392 triangles when
composed at the root.

## Workaround

Author a root layer that references each element onto a **stage-root** prim:

```usda
def Xform "isGardeniaA" (
    prepend references = @…/elements/isGardeniaA/element.usda@</isGardeniaA>
) { }
```

For the island this is geometrically equivalent, since `/island` carries no
transform of its own. With that change all 20 elements import (~3.15 M
geometries, ~57.7 M top-level triangles).

## Possible location

`src/pcp/instancing.rs` — `materialize_prototype` and `redirect_anchor`. The
module doc mentions re-anchoring an instance's composed namespace onto the
prototype root; the failing case is presumably one where the enclosing-prim
lookup that drives that re-anchoring does not find its target.

## Environment

- `openusd` 0.5.0 (default features off; `geom`, `lux`, `shade`, `render` on)
- rustc stable, edition 2024, Linux x86-64
- Found while adding Ptex support to
  [crust-render](https://github.com/doubleailes/crust-render)
