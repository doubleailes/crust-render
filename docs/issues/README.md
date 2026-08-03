# Upstream issue reports

Bug reports written up for filing against the libraries crust-render depends on.
Each is self-contained — summary, minimal reproduction, actual vs expected,
hypotheses ruled out, and measured impact — with the reproduction files checked in
under `repro/` and the driver output taken from an actual run.

| report | target | status |
| --- | --- | --- |
| [openusd: prototype does not materialize through a non-root reference](openusd-prototype-through-nested-reference.md) | [mxpv/openusd](https://github.com/mxpv/openusd) | fixed in [0.6.0](https://github.com/mxpv/openusd/releases/tag/v0.6.0) |
| [openusd: relationship targets lost through a variant inside a prototype](openusd-relationship-targets-lost-through-variant-in-prototype.md) | [mxpv/openusd](https://github.com/mxpv/openusd) | fixed in [0.6.0](https://github.com/mxpv/openusd/releases/tag/v0.6.0) |

Both were found while adding Ptex support and rendering Disney's Moana Island
Scene, and both are fixed in openusd 0.6.0, which the workspace now tracks from
crates.io. They are kept as a record of what the symptoms looked like from the
consumer side: in each case the data was present and readable, the API reported
success, and the importer simply saw less than was there. Nothing errored, which
is what made them expensive to find and worth writing down.

Not filed here, because it was our own bug rather than a library one: Ptex face
tables were wired only through the direct-mesh import path and not through
prototypes, so per-face textures applied to none of the island's
instanced geometry. Fixed in `attach_proto_parts`.
