# Spec Delta

## ADDED Requirements

### Requirement: The stats report shows the geometry layout

With `--stats`, the kernel-memory block SHALL list, besides its existing rows, the
bytes held by vertices, per-vertex normals and triangle records, the bytes of gathered
and of indexed triangle packets separately, the share of packet lanes holding a
triangle, and the kernel bytes per resident triangle.

#### Scenario: A subdivided scene's report

- **WHEN** `samples/subdivision.usda` is rendered with `--stats` at `--subdiv-level 3`
- **THEN** the report prints `vertices`, `vertex normals`, `triangle records`,
  `triangle packets (gathered)` or `(indexed)`, `lanes filled` as a percentage and
  `bytes per triangle`, and the rows sum to `kernel memory`

### Requirement: Two geometry-layout switches

`CRUST_TRI_PACKETS` (`gathered` | `indexed` | `auto`, default `auto`) SHALL force the
packet layout on every tree, and `CRUST_BVH_PACKET_SAH` (boolean, default on) SHALL
select the packet-aware leaf cost; both SHALL be parsed once into `Config`, warn once
on a bad value, and be listed in `docs/architecture.md` with the behaviour before this
change as their off side (`gathered`, off).

#### Scenario: A bad value

- **WHEN** `CRUST_TRI_PACKETS=fast` is set
- **THEN** one warning names the variable and the render proceeds under `auto`
