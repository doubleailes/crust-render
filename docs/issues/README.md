# Upstream issue reports

Bug reports written up for filing against the libraries crust-render depends on.
Each is self-contained — summary, minimal reproduction, actual vs expected,
hypotheses ruled out, and measured impact — with the reproduction files checked in
under `repro/` and the driver output taken from an actual run.

| report | target | status |
| --- | --- | --- |
| [openusd: prototype does not materialize through a non-root reference](openusd-prototype-through-nested-reference.md) | [mxpv/openusd](https://github.com/mxpv/openusd) | fixed in the [doubleailes fork](https://github.com/doubleailes/openusd) (`45fc15c`), which the workspace now patches to |
| [openusd: relationship targets lost through a variant inside a prototype](openusd-relationship-targets-lost-through-variant-in-prototype.md) | [mxpv/openusd](https://github.com/mxpv/openusd) | still open — reproduces on `45fc15c` |

Both were found while adding Ptex support and rendering Disney's Moana Island
Scene; together they account for essentially all of the geometry the importer
loses on that stage. See "Known incomplete work" in `CLAUDE.md` for how the
importer works around them today.

Not filed here, because it was our own bug rather than a library one: Ptex face
tables were wired only through the direct-mesh import path and not through
prototypes, so per-face textures applied to none of the island's
instanced geometry. Fixed in `attach_proto_parts`.
