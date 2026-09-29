# Spec Delta

## ADDED Requirements

### Requirement: The render profile reports subsurface walks

With `--profile`, the report SHALL time every subsurface random walk, entry to exit
record, as a section named `Subsurface` nested under `MainLoop`, counted once per
walk. A scene without subsurface walks SHALL show no `Subsurface` row and SHALL
render with the same per-vertex work as before the section existed.

#### Scenario: Walks are their own row

- **WHEN** `samples/materialx_subsurface.usda` is rendered with `--profile`
- **THEN** the render profile lists `Subsurface` under `MainLoop` with a call count
  equal to the `subsurface walks` count of the same report's ray statistics

#### Scenario: A scene without walks pays nothing

- **WHEN** `samples/cornellbox.usda` is rendered with `--profile`
- **THEN** no `Subsurface` row appears, and without `--profile` the render
  executes the same instructions as before the section within 0.01%
