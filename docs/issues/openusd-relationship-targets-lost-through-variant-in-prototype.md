# openusd: relationship targets resolve to zero inside a native instance's prototype when the prim arrives through a variant selection

**Target repository:** <https://github.com/mxpv/openusd>
**Affected version:** `openusd` 0.5.0 (latest on crates.io as of 2026-06)
**Severity:** silent data loss — a populated relationship reads as empty
**Status:** **fixed upstream in openusd 0.6.0** — the reproduction below now
reports 1 target for the variant case, where 0.5.0 (and the interim fork) reported
0. On the island this recovers all six lost `PointInstancer`s — the
"has no `prototypes` targets" warnings are gone entirely, worth ~74 500 resident
instances of bay cedar understory. `isBayCedarA1` alone imports at 136 133
geometries / 4 060 638 triangles, against 136 124 / 4 058 910 before.

---

## Summary

A relationship on a prim inside a native instance's prototype resolves to **zero
targets** when that prim was brought in through a **variant selection**. The
relationship itself is present and every sibling attribute reads correctly; only
`Relationship::targets()` comes back empty.

Composing the identical content without the `variantSet` — one variable changed,
nothing else — resolves the target correctly. So this is target *resolution* into
the prototype namespace, not missing or unreadable data.

Observed on `UsdGeomPointInstancer::prototypes`, which is where it bites in
practice: an instancer whose `prototypes` reads as empty places nothing, so the
geometry it scatters silently disappears.

## Reproduction

Five files. `bundle.usda` holds an instancer and its prototype; the two
`content_*.usda` differ **only** in whether a `variantSet` wraps the subtree; the
two `outer_*.usda` place that content through an `instanceable` prim.

`bundle.usda`

```usda
#usda 1.0
( defaultPrim = "bundle" )
def Xform "bundle"
{
    def PointInstancer "instancer"
    {
        rel prototypes = [ </bundle/instancer/Proto1> ]
        int[] protoIndices = [0]
        point3f[] positions = [(0, 0, 0)]

        def Xform "Proto1"
        {
            def Mesh "M"
            {
                int[] faceVertexCounts = [4]
                int[] faceVertexIndices = [0, 1, 2, 3]
                point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
            }
        }
    }
}
```

`content_plain.usda` — control, resolves correctly

```usda
#usda 1.0
( defaultPrim = "C" )
def Xform "C"
{
    def Xform "geometry" (
        prepend payload = @./bundle.usda@</bundle>
    )
    {
    }
}
```

`content_variant.usda` — the same subtree behind a variant selection, fails

```usda
#usda 1.0
( defaultPrim = "C" )
def Xform "C" (
    variants = {
        string element = "v1"
    }
    prepend variantSets = "element"
)
{
    variantSet "element" = {
        "v1" {
            def Xform "geometry" (
                prepend payload = @./bundle.usda@</bundle>
            )
            {
            }
        }
    }
}
```

`outer_plain.usda` / `outer_variant.usda` — identical but for the payload target

```usda
#usda 1.0
( defaultPrim = "Root" )
def Xform "Root"
{
    def Xform "Inst" (
        instanceable = true
        payload = @./content_plain.usda@</C>      # or content_variant.usda
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
    for layer in ["outer_plain.usda", "outer_variant.usda"] {
        let stage = usd::Stage::builder()
            .load(usd::InitialLoadSet::LoadAll)
            .open(layer)
            .expect("open");

        // Reach the instancer through the instance's prototype, as a consumer would.
        let inst = stage.prim_at(sdf::Path::new("/Root/Inst").expect("path"));
        let proto_path = inst.prototype().expect("prototype()").expect("some");
        let instancer_path = format!("{proto_path}/geometry/instancer");
        let instancer = stage.prim_at(sdf::Path::new(&instancer_path).expect("path"));

        println!("{layer}");
        println!("  type_name  = {:?}", instancer.type_name());
        println!("  properties = {:?}", instancer.property_names());
        println!(
            "  prototypes -> {:?} target(s)",
            instancer.relationship("prototypes").targets().map(|t| t.len())
        );
        println!();
    }
}
```

The files above are checked in under `docs/issues/repro/variant-rel-targets/`
(`bundle.usda`, `content_plain.usda`, `content_variant.usda`,
`outer_plain.usda`, `outer_variant.usda`); the output below is from running the
driver in that directory.

## Actual behaviour

```text
outer_plain.usda
  type_name  = Ok(Some("PointInstancer"))
  properties = Ok(["prototypes", "protoIndices", "positions"])
  prototypes -> Ok(1) target(s)

outer_variant.usda
  type_name  = Ok(Some("PointInstancer"))
  properties = Ok(["prototypes", "protoIndices", "positions"])
  prototypes -> Ok(0) target(s)      <-- present, populated, but resolves to nothing
```

Note the prim is otherwise entirely intact in the failing case: correct type,
`prototypes` listed among its properties, and `protoIndices` / `positions` both
readable. Only the relationship's *targets* are lost.

On the island's real instancers the same holds with a fuller property set —
`["orientations", "positions", "protoIndices", "prototypes", "scales",
"invisibleIds"]` all present, `prototypes` resolving to zero.

## Expected behaviour

`outer_variant.usda` should report 1 target, re-anchored into the prototype
namespace — i.e. `<prototype>/geometry/instancer/Proto1` — exactly as the control
case does.

## What was ruled out

Each of these was tested against the same bundle layer and resolves correctly, so
none is the trigger:

- **Target location.** Targets pointing inside the instancer's own subtree, and
  targets pointing at a sibling subtree, both resolve.
- **Arc type into the bundle.** `payload` and `references` both resolve.
- **Nesting depth of the payload.** An extra `Xform` level between the prototype
  root and the instancer changes nothing.
- **Being inside a prototype at all.** The control case *is* inside a prototype
  and resolves fine — so this is narrower than "relationships don't work in
  prototypes".

The variant selection is the only variable that changes the outcome.

## Impact

On Disney's [Moana Island Scene](https://disneyanimation.com/resources/moana-island-scene/)
this loses exactly the geometry scattered by variant-selected prototypes. Of the
20 elements, only `isBayCedarA1` composes its geometry through a `variantSet`
(`instance.usda` selects `element = "bonsaiA"` among `base`/`bonsaiA`/`bonsaiB`/
`bonsaiC`), and **all six lost instancers in the whole island trace back to it**:

| where | instancers lost |
| --- | --- |
| `isBayCedarA1` — its own `geometry/instancer`, two prototypes | 2 |
| `isDunesB` — `xgTreeFill` scatters isBayCedarA1 trees, one instancer per variant (`base`, `bonsaiA`, `bonsaiB`, `bonsaiC`) | 4 |

Every other prototype-internal instancer on the island resolves normally — for
instance `isDunesB`'s own `xgRoots` (14 targets), `xgPandanus` (1), `xgTreeFill`
(4), `xgTreeSkyLine` (2), `xgTreeSpecific` (1), and `isGardeniaA`'s `xgBonsai`
(12). That contrast is what narrowed it to variants.

Confirmation that the data is present and readable: opening the bundle layer
directly, outside any prototype, resolves the same relationship —

```text
$ rel_probe elements/isDunesB/xgenInstances/xgPandanus.usd
instancer /bundle/instancer
  prototypes -> 1 target(s)
      /bundle/instancer/xgPandanus_isPandanusAlo_base
```

## Possible location

`src/pcp/instancing.rs` — `redirect_anchor` / `materialize_prototype`, whichever
path re-anchors relationship targets onto the prototype namespace. The variant
arc presumably introduces a node in the prim index whose enclosing-prim lookup
does not map onto the prototype root, so the target path fails to redirect and is
dropped rather than reported.

Worth noting the failure mode: the target is silently discarded. Even before a
fix, surfacing it (an error, or leaving the unresolved path in place) would let
consumers tell "no targets authored" apart from "targets authored but not
resolvable".

## Environment

- `openusd` 0.5.0 (default features off; `geom`, `lux`, `shade`, `render` on)
- rustc stable, edition 2024, Linux x86-64
- Found while adding Ptex support to
  [crust-render](https://github.com/doubleailes/crust-render)
