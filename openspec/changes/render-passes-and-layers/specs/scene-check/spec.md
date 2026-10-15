# Spec Delta

## ADDED Requirements

### Requirement: A pass described

With `--pass`, the report's `products` SHALL list the files the pass's pattern would write at the
checked frame, in the order the render writes them. Each entry SHALL give its file and its channels
as the `render-passes` capability names them, with the pass's path as its prim. A pattern the
render would refuse SHALL fail the check the same way.

#### Scenario: Files of a layered pass

- **WHEN** a pass with layers `hero` and `default` and the pattern `$l_$n.$04f.$x` is checked with
  `-f 3`
- **THEN** `products` lists `render_hero.0003.exr` and `render_default.0003.exr`, each with its
  layer's channels

#### Scenario: A refused pattern

- **WHEN** the pass's pattern names an unset `$(IMAGES)`
- **THEN** `crust check` exits non-zero, naming `IMAGES`, as `crust render` would
