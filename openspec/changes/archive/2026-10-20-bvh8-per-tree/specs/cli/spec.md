# Spec Delta

## ADDED Requirements

### Requirement: A BVH-width switch

`CRUST_BVH_WIDTH` (`four` | `eight` | `auto`, also `4` | `8`, default `auto`) SHALL
force the node width of every kernel tree; it SHALL be parsed once into `Config`, warn
once on a bad value, and be listed in `docs/architecture.md` with the behaviour before
8-wide nodes as its off side (`four`).

#### Scenario: A bad value

- **WHEN** `CRUST_BVH_WIDTH=16` is set
- **THEN** one warning names the variable and the render proceeds under `auto`
